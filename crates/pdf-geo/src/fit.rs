//! Estimation of the page-space → world transform from control points.
//!
//! A CAD sheet places its drawing through a uniform scale, a rotation and a
//! translation, so a *similarity* transform is the correct model: fitting a
//! full affine would silently absorb bad correspondences into a shear. RANSAC
//! over 2-point hypotheses handles the label/row matching being ambiguous.

use serde::{Deserialize, Serialize};

use pdf_geometry::model::Point;

/// A 2D similarity transform: uniform scale, rotation, translation.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Similarity {
    /// `x' = a·x − b·y + tx`
    pub a: f64,
    /// `y' = b·x + a·y + ty`
    pub b: f64,
    pub tx: f64,
    pub ty: f64,
}

impl Similarity {
    pub const IDENTITY: Similarity = Similarity {
        a: 1.0,
        b: 0.0,
        tx: 0.0,
        ty: 0.0,
    };

    pub fn apply(&self, p: Point) -> Point {
        Point::new(
            self.a * p.x - self.b * p.y + self.tx,
            self.b * p.x + self.a * p.y + self.ty,
        )
    }

    /// Uniform scale factor, in world units per page unit.
    pub fn scale(&self) -> f64 {
        (self.a * self.a + self.b * self.b).sqrt()
    }

    /// Rotation in degrees, CCW.
    pub fn rotation_degrees(&self) -> f64 {
        self.b.atan2(self.a).to_degrees()
    }

    /// Drawing scale denominator, e.g. 500 for a 1:500 sheet. PDF user units
    /// are 1/72 inch, so one unit is 0.0254/72 metres on paper.
    pub fn drawing_scale_denominator(&self) -> f64 {
        self.scale() / (0.0254 / 72.0)
    }
}

/// How a page↔world pairing was proposed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CandidateSource {
    /// A drawn polyline matched the control polygon's shape, so the pairing
    /// already carries ordered geometric evidence.
    ShapeMatch,
    /// A vertex number printed on the drawing was read as text and paired with
    /// the table row of the same label. Individually unverified.
    TextMarker,
}

/// A page-space observation paired with its world-space truth.
#[derive(Debug, Clone, Copy)]
pub struct Correspondence {
    pub page: Point,
    pub world: Point,
    /// Index of the control-point table row this pairing came from.
    pub row: usize,
    pub source: CandidateSource,
}

#[derive(Debug, Clone)]
pub struct Fit {
    pub transform: Similarity,
    /// Indices into the input slice that agreed with the model.
    pub inliers: Vec<usize>,
    /// Root-mean-square residual over the inliers, in world units (metres).
    pub rmse: f64,
    /// Largest single inlier residual, in world units.
    pub max_residual: f64,
}

/// Least-squares similarity fit (Umeyama, without scaling ambiguity) over all
/// supplied correspondences. Returns `None` for fewer than two points or for a
/// degenerate configuration.
pub fn fit_similarity(points: &[Correspondence]) -> Option<Similarity> {
    if points.len() < 2 {
        return None;
    }
    let n = points.len() as f64;
    let (mut px, mut py, mut wx, mut wy) = (0.0, 0.0, 0.0, 0.0);
    for c in points {
        px += c.page.x;
        py += c.page.y;
        wx += c.world.x;
        wy += c.world.y;
    }
    let (px, py, wx, wy) = (px / n, py / n, wx / n, wy / n);

    let (mut sxx, mut sxy, mut norm) = (0.0, 0.0, 0.0);
    for c in points {
        let (dx, dy) = (c.page.x - px, c.page.y - py);
        let (ex, ey) = (c.world.x - wx, c.world.y - wy);
        sxx += dx * ex + dy * ey;
        sxy += dx * ey - dy * ex;
        norm += dx * dx + dy * dy;
    }
    if norm < 1e-12 {
        return None;
    }
    let a = sxx / norm;
    let b = sxy / norm;
    if !a.is_finite() || !b.is_finite() || (a * a + b * b) < 1e-24 {
        return None;
    }
    Some(Similarity {
        a,
        b,
        tx: wx - (a * px - b * py),
        ty: wy - (b * px + a * py),
    })
}

/// Constraints a candidate transform must satisfy to be considered plausible.
#[derive(Debug, Clone, Copy)]
pub struct FitConstraints {
    /// Inlier threshold in world units (metres).
    pub tolerance: f64,
    /// Acceptable range for the drawing scale denominator.
    pub min_scale_denominator: f64,
    pub max_scale_denominator: f64,
    pub min_inliers: usize,
    /// Minimum ratio between the minor and major spread of the inlier page
    /// points. A near-zero ratio means they are collinear, which any similarity
    /// transform can satisfy — the classic false positive is the table's own
    /// row-number column being read as drawing markers.
    pub min_spread_ratio: f64,
}

impl Default for FitConstraints {
    fn default() -> Self {
        Self {
            tolerance: 0.35,
            // Covers everything from a 1:50 detail to a 1:20 000 overview.
            min_scale_denominator: 20.0,
            max_scale_denominator: 25000.0,
            min_inliers: 4,
            min_spread_ratio: 0.05,
        }
    }
}

/// Exhaustive RANSAC over every pair of correspondences.
///
/// Control-point tables have at most a few dozen rows, so the O(n²) hypothesis
/// set is cheap and removes the non-determinism of random sampling. Candidate
/// correspondences may include wrong pairings (a page label matched to the
/// wrong row); only the largest mutually consistent set is kept, and at most
/// one candidate per row survives.
pub fn ransac_similarity(
    candidates: &[Correspondence],
    constraints: FitConstraints,
) -> Option<Fit> {
    if candidates.len() < constraints.min_inliers.max(2) {
        return None;
    }

    let mut best: Option<Fit> = None;
    for i in 0..candidates.len() {
        for j in (i + 1)..candidates.len() {
            if candidates[i].row == candidates[j].row {
                continue;
            }
            let Some(model) = fit_similarity(&[candidates[i], candidates[j]]) else {
                continue;
            };
            let denominator = model.drawing_scale_denominator();
            if !(constraints.min_scale_denominator..=constraints.max_scale_denominator)
                .contains(&denominator)
            {
                continue;
            }
            let inliers = collect_inliers(candidates, &model, constraints.tolerance);
            if inliers.len() < constraints.min_inliers {
                continue;
            }
            // Refine on the consensus set, then re-collect: a two-point seed is
            // noisy, and the refit typically pulls in a few more rows.
            let subset: Vec<Correspondence> = inliers.iter().map(|&k| candidates[k]).collect();
            let Some(refined) = fit_similarity(&subset) else {
                continue;
            };
            let inliers = collect_inliers(candidates, &refined, constraints.tolerance);
            if inliers.len() < constraints.min_inliers {
                continue;
            }
            let subset: Vec<Correspondence> = inliers.iter().map(|&k| candidates[k]).collect();
            // Collinear support is only suspect when every pairing is an
            // unverified label match; a shape match has already agreed on the
            // ordering of a whole polyline, which a chance alignment cannot.
            let unverified = subset
                .iter()
                .all(|c| c.source == CandidateSource::TextMarker);
            if unverified && spread_ratio(&subset) < constraints.min_spread_ratio {
                continue;
            }
            let (rmse, max_residual) = residuals(&subset, &refined);
            let fit = Fit {
                transform: refined,
                inliers,
                rmse,
                max_residual,
            };
            let better = match &best {
                None => true,
                Some(b) => (fit.inliers.len(), -fit.rmse) > (b.inliers.len(), -b.rmse),
            };
            if better {
                best = Some(fit);
            }
        }
    }
    best
}

/// Keeps the closest candidate per table row, so one row cannot vote twice.
fn collect_inliers(
    candidates: &[Correspondence],
    model: &Similarity,
    tolerance: f64,
) -> Vec<usize> {
    let mut best_per_row: Vec<(usize, usize, f64)> = Vec::new();
    for (idx, c) in candidates.iter().enumerate() {
        let predicted = model.apply(c.page);
        let residual = predicted.distance(&c.world);
        if residual > tolerance {
            continue;
        }
        match best_per_row.iter_mut().find(|(row, _, _)| *row == c.row) {
            Some(entry) if residual < entry.2 => *entry = (c.row, idx, residual),
            Some(_) => {}
            None => best_per_row.push((c.row, idx, residual)),
        }
    }
    let mut out: Vec<usize> = best_per_row.into_iter().map(|(_, idx, _)| idx).collect();
    out.sort_unstable();
    out
}

/// Ratio of the minor to the major principal spread of the page points, via
/// the eigenvalues of their 2x2 covariance matrix. Zero for collinear input.
fn spread_ratio(points: &[Correspondence]) -> f64 {
    if points.len() < 3 {
        return 0.0;
    }
    let n = points.len() as f64;
    let (mx, my) = points
        .iter()
        .fold((0.0, 0.0), |(ax, ay), c| (ax + c.page.x, ay + c.page.y));
    let (mx, my) = (mx / n, my / n);
    let (mut sxx, mut syy, mut sxy) = (0.0, 0.0, 0.0);
    for c in points {
        let (dx, dy) = (c.page.x - mx, c.page.y - my);
        sxx += dx * dx;
        syy += dy * dy;
        sxy += dx * dy;
    }
    let (sxx, syy, sxy) = (sxx / n, syy / n, sxy / n);
    let trace = sxx + syy;
    let det = sxx * syy - sxy * sxy;
    let disc = (trace * trace / 4.0 - det).max(0.0).sqrt();
    let major = trace / 2.0 + disc;
    let minor = trace / 2.0 - disc;
    if major <= 1e-12 {
        return 0.0;
    }
    (minor.max(0.0) / major).sqrt()
}

fn residuals(points: &[Correspondence], model: &Similarity) -> (f64, f64) {
    let mut sum = 0.0;
    let mut max: f64 = 0.0;
    for c in points {
        let d = model.apply(c.page).distance(&c.world);
        sum += d * d;
        max = max.max(d);
    }
    ((sum / points.len().max(1) as f64).sqrt(), max)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Points on a circle, so the set has genuine two-dimensional spread.
    fn synthetic(model: Similarity, n: usize) -> Vec<Correspondence> {
        (0..n)
            .map(|i| {
                let theta = i as f64 * std::f64::consts::TAU / n as f64;
                let page = Point::new(400.0 + 90.0 * theta.cos(), 300.0 + 90.0 * theta.sin());
                Correspondence {
                    page,
                    world: model.apply(page),
                    row: i,
                    source: CandidateSource::TextMarker,
                }
            })
            .collect()
    }

    #[test]
    fn recovers_exact_transform() {
        let truth = Similarity {
            a: 0.1763,
            b: 0.0421,
            tx: 8471000.0,
            ty: 4419000.0,
        };
        let fit = fit_similarity(&synthetic(truth, 6)).unwrap();
        assert!((fit.a - truth.a).abs() < 1e-9);
        assert!((fit.b - truth.b).abs() < 1e-9);
        assert!((fit.tx - truth.tx).abs() < 1e-4);
    }

    #[test]
    fn ransac_rejects_wrong_pairings() {
        let truth = Similarity {
            a: 0.1763,
            b: 0.0,
            tx: 8471000.0,
            ty: 4419000.0,
        };
        let mut pts = synthetic(truth, 8);
        // Corrupt three rows by swapping their world coordinates around.
        pts[1].world = Point::new(pts[1].world.x + 90.0, pts[1].world.y - 40.0);
        pts[4].world = Point::new(pts[4].world.x - 55.0, pts[4].world.y + 12.0);
        pts[6].world = Point::new(pts[6].world.x + 7.0, pts[6].world.y + 7.0);

        let fit = ransac_similarity(&pts, FitConstraints::default()).expect("fit");
        assert_eq!(fit.inliers.len(), 5);
        assert!(fit.rmse < 1e-6, "rmse {}", fit.rmse);
        assert!(!fit.inliers.contains(&1));
    }

    #[test]
    fn rejects_collinear_control_points() {
        // Points on a single line — the shape of a table's row-number column.
        let truth = Similarity {
            a: 0.1763,
            b: 0.0,
            tx: 8471000.0,
            ty: 4419000.0,
        };
        let pts: Vec<Correspondence> = (0..8)
            .map(|i| {
                let page = Point::new(300.0, 500.0 - i as f64 * 10.0);
                Correspondence {
                    page,
                    world: truth.apply(page),
                    row: i,
                    source: CandidateSource::TextMarker,
                }
            })
            .collect();
        assert!(ransac_similarity(&pts, FitConstraints::default()).is_none());
    }

    #[test]
    fn rejects_implausible_drawing_scale() {
        // 100 m per PDF unit is far outside any real sheet scale.
        let truth = Similarity {
            a: 100.0,
            b: 0.0,
            tx: 0.0,
            ty: 0.0,
        };
        assert!(ransac_similarity(&synthetic(truth, 5), FitConstraints::default()).is_none());
    }

    #[test]
    fn reports_drawing_scale_denominator() {
        // 1:500 → one PDF unit (1/72 inch) spans 500/72 inch = 0.1764 m.
        let s = Similarity {
            a: 0.17639,
            b: 0.0,
            tx: 0.0,
            ty: 0.0,
        };
        assert!((s.drawing_scale_denominator() - 500.0).abs() < 1.0);
    }
}
