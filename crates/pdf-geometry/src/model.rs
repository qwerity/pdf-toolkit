//! Geometric primitives shared by the content-stream interpreter and every
//! downstream consumer (classification, georeferencing, GeoJSON emission).

use serde::{Deserialize, Serialize};

/// A point in PDF user space (origin bottom-left, units of 1/72 inch).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

impl Point {
    pub const fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }

    pub fn distance(&self, other: &Point) -> f64 {
        ((self.x - other.x).powi(2) + (self.y - other.y).powi(2)).sqrt()
    }
}

/// A 2D affine transform stored in PDF order `[a b c d e f]`, mapping
/// `(x, y) -> (a·x + c·y + e, b·x + d·y + f)`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Matrix {
    pub a: f64,
    pub b: f64,
    pub c: f64,
    pub d: f64,
    pub e: f64,
    pub f: f64,
}

impl Default for Matrix {
    fn default() -> Self {
        Self::IDENTITY
    }
}

impl Matrix {
    pub const IDENTITY: Matrix = Matrix {
        a: 1.0,
        b: 0.0,
        c: 0.0,
        d: 1.0,
        e: 0.0,
        f: 0.0,
    };

    pub const fn new(a: f64, b: f64, c: f64, d: f64, e: f64, f: f64) -> Self {
        Self { a, b, c, d, e, f }
    }

    pub const fn translate(tx: f64, ty: f64) -> Self {
        Self::new(1.0, 0.0, 0.0, 1.0, tx, ty)
    }

    pub const fn scale(sx: f64, sy: f64) -> Self {
        Self::new(sx, 0.0, 0.0, sy, 0.0, 0.0)
    }

    /// Returns `self` applied *before* `outer` (i.e. `self × outer` in PDF's
    /// row-vector convention, which is how `cm` composes onto the CTM).
    pub fn then(&self, outer: &Matrix) -> Matrix {
        Matrix {
            a: self.a * outer.a + self.b * outer.c,
            b: self.a * outer.b + self.b * outer.d,
            c: self.c * outer.a + self.d * outer.c,
            d: self.c * outer.b + self.d * outer.d,
            e: self.e * outer.a + self.f * outer.c + outer.e,
            f: self.e * outer.b + self.f * outer.d + outer.f,
        }
    }

    pub fn apply(&self, p: Point) -> Point {
        Point {
            x: self.a * p.x + self.c * p.y + self.e,
            y: self.b * p.x + self.d * p.y + self.f,
        }
    }

    /// Mean linear scale factor, used to convert line widths and font sizes
    /// from the local space into user space.
    pub fn mean_scale(&self) -> f64 {
        let sx = (self.a * self.a + self.b * self.b).sqrt();
        let sy = (self.c * self.c + self.d * self.d).sqrt();
        ((sx * sy).abs()).sqrt()
    }
}

/// An axis-aligned bounding box; `None`-equivalent state is represented by an
/// empty box whose `min` exceeds its `max`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct BBox {
    pub min_x: f64,
    pub min_y: f64,
    pub max_x: f64,
    pub max_y: f64,
}

impl BBox {
    pub const EMPTY: BBox = BBox {
        min_x: f64::INFINITY,
        min_y: f64::INFINITY,
        max_x: f64::NEG_INFINITY,
        max_y: f64::NEG_INFINITY,
    };

    pub fn is_empty(&self) -> bool {
        self.min_x > self.max_x || self.min_y > self.max_y
    }

    pub fn extend(&mut self, p: Point) {
        self.min_x = self.min_x.min(p.x);
        self.min_y = self.min_y.min(p.y);
        self.max_x = self.max_x.max(p.x);
        self.max_y = self.max_y.max(p.y);
    }

    pub fn union(&mut self, other: &BBox) {
        if other.is_empty() {
            return;
        }
        self.extend(Point::new(other.min_x, other.min_y));
        self.extend(Point::new(other.max_x, other.max_y));
    }

    pub fn width(&self) -> f64 {
        if self.is_empty() {
            0.0
        } else {
            self.max_x - self.min_x
        }
    }

    pub fn height(&self) -> f64 {
        if self.is_empty() {
            0.0
        } else {
            self.max_y - self.min_y
        }
    }

    pub fn area(&self) -> f64 {
        self.width() * self.height()
    }

    pub fn center(&self) -> Point {
        Point::new(
            (self.min_x + self.max_x) / 2.0,
            (self.min_y + self.max_y) / 2.0,
        )
    }

    pub fn contains(&self, p: Point) -> bool {
        p.x >= self.min_x && p.x <= self.max_x && p.y >= self.min_y && p.y <= self.max_y
    }

    /// Fraction of `self` that lies inside `other`, measured per axis.
    ///
    /// A ratio of areas would be useless here: an axis-aligned rule line — the
    /// most common thing a caller wants to test — has zero height, so its area
    /// and its intersection area are both zero. Each axis is therefore scored
    /// separately, with a degenerate axis counting as fully contained when its
    /// coordinate falls inside `other`'s range.
    pub fn overlap_ratio(&self, other: &BBox) -> f64 {
        const EPSILON: f64 = 1e-9;
        if self.is_empty() || other.is_empty() {
            return 0.0;
        }
        let axis = |min: f64, max: f64, other_min: f64, other_max: f64| -> f64 {
            let span = max - min;
            if span <= EPSILON {
                return f64::from(min >= other_min - EPSILON && min <= other_max + EPSILON);
            }
            (max.min(other_max) - min.max(other_min)).max(0.0) / span
        };
        axis(self.min_x, self.max_x, other.min_x, other.max_x)
            * axis(self.min_y, self.max_y, other.min_y, other.max_y)
    }
}

#[cfg(test)]
mod bbox_tests {
    use super::*;

    fn boxed(min_x: f64, min_y: f64, max_x: f64, max_y: f64) -> BBox {
        BBox {
            min_x,
            min_y,
            max_x,
            max_y,
        }
    }

    #[test]
    fn a_horizontal_rule_inside_a_box_is_fully_contained() {
        let table = boxed(0.0, 0.0, 100.0, 50.0);
        let rule = boxed(10.0, 25.0, 90.0, 25.0);
        assert!((rule.overlap_ratio(&table) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn a_rule_outside_the_box_scores_zero() {
        let table = boxed(0.0, 0.0, 100.0, 50.0);
        let rule = boxed(10.0, 80.0, 90.0, 80.0);
        assert_eq!(rule.overlap_ratio(&table), 0.0);
    }

    #[test]
    fn half_overlap_scores_half() {
        let a = boxed(0.0, 0.0, 100.0, 10.0);
        let b = boxed(50.0, -5.0, 200.0, 15.0);
        assert!((a.overlap_ratio(&b) - 0.5).abs() < 1e-9);
    }
}

/// RGB in the 0..=1 range, resolved from whichever PDF colour space was active.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Rgb {
    pub r: f64,
    pub g: f64,
    pub b: f64,
}

impl Rgb {
    pub const BLACK: Rgb = Rgb {
        r: 0.0,
        g: 0.0,
        b: 0.0,
    };

    pub fn to_hex(self) -> String {
        let q = |v: f64| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
        format!("#{:02x}{:02x}{:02x}", q(self.r), q(self.g), q(self.b))
    }

    /// Perceptual luminance, used to drop near-white construction lines.
    pub fn luminance(self) -> f64 {
        0.2126 * self.r + 0.7152 * self.g + 0.0722 * self.b
    }
}

/// How a path was painted. Determines whether it becomes a GeoJSON
/// `LineString` or `Polygon`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PaintMode {
    Stroke,
    Fill,
    FillStroke,
}

/// A single connected run of points, already flattened to line segments in
/// user space.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubPath {
    pub points: Vec<Point>,
    pub closed: bool,
}

impl SubPath {
    pub fn length(&self) -> f64 {
        self.points
            .windows(2)
            .map(|w| w[0].distance(&w[1]))
            .sum::<f64>()
            + if self.closed && self.points.len() > 2 {
                self.points[self.points.len() - 1].distance(&self.points[0])
            } else {
                0.0
            }
    }

    /// Signed shoelace area; the sign encodes winding direction.
    pub fn signed_area(&self) -> f64 {
        let n = self.points.len();
        if n < 3 {
            return 0.0;
        }
        let mut sum = 0.0;
        for i in 0..n {
            let p = self.points[i];
            let q = self.points[(i + 1) % n];
            sum += p.x * q.y - q.x * p.y;
        }
        sum / 2.0
    }

    pub fn bbox(&self) -> BBox {
        let mut b = BBox::EMPTY;
        for p in &self.points {
            b.extend(*p);
        }
        b
    }
}

/// A painted path: one or more subpaths sharing a graphics state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VectorPath {
    pub subpaths: Vec<SubPath>,
    pub mode: PaintMode,
    pub stroke: Rgb,
    pub fill: Rgb,
    /// Stroke width in user-space units (already scaled by the CTM).
    pub width: f64,
    /// Optional-content group name (`/OC`) in effect when the path was painted;
    /// CAD exports use these as layer names.
    pub layer: Option<String>,
}

impl VectorPath {
    pub fn bbox(&self) -> BBox {
        let mut b = BBox::EMPTY;
        for sp in &self.subpaths {
            b.union(&sp.bbox());
        }
        b
    }

    pub fn total_length(&self) -> f64 {
        self.subpaths.iter().map(SubPath::length).sum()
    }

    pub fn point_count(&self) -> usize {
        self.subpaths.iter().map(|s| s.points.len()).sum()
    }
}

/// A whitespace-delimited token inside a [`TextItem`], with its position
/// estimated from the run's origin.
#[derive(Debug, Clone, Copy)]
pub struct TextToken<'a> {
    pub text: &'a str,
    pub origin: Point,
    pub size: f64,
    pub angle: f64,
}

/// A decoded text run positioned in user space.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextItem {
    pub text: String,
    /// Baseline origin of the run.
    pub origin: Point,
    /// Effective font size in user-space units.
    pub size: f64,
    /// Baseline direction in radians, CCW from the +x axis.
    pub angle: f64,
    pub color: Rgb,
}

/// Everything the interpreter recovered from one page.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PageContent {
    pub page_number: usize,
    /// Page box in user-space units after cropping, as `[width, height]`.
    pub media_box: BBox,
    pub paths: Vec<VectorPath>,
    pub texts: Vec<TextItem>,
    /// Number of raster images drawn, and the fraction of the page they cover.
    pub image_count: usize,
    pub image_coverage: f64,
}

impl TextItem {
    /// Splits the run into whitespace-delimited tokens, advancing the origin by
    /// an average glyph width per character.
    ///
    /// Table cells are frequently emitted as a single run whose columns are
    /// separated only by spacing, so matching on whole runs would miss them.
    pub fn tokens(&self) -> Vec<TextToken<'_>> {
        const AVERAGE_GLYPH_WIDTH: f64 = 0.5;
        let (cos, sin) = (self.angle.cos(), self.angle.sin());
        let mut out = Vec::new();
        let mut offset = 0usize;
        for token in self.text.split_whitespace() {
            // `split_whitespace` discards separators, so recover each token's
            // character offset by searching forward from the previous one.
            let byte_index = self.text[offset..]
                .find(token)
                .map(|i| offset + i)
                .unwrap_or(offset);
            offset = byte_index + token.len();
            let chars_before = self.text[..byte_index].chars().count() as f64;
            let advance = chars_before * AVERAGE_GLYPH_WIDTH * self.size;
            out.push(TextToken {
                text: token,
                origin: Point::new(self.origin.x + advance * cos, self.origin.y + advance * sin),
                size: self.size,
                angle: self.angle,
            });
        }
        out
    }
}

impl PageContent {
    pub fn concatenated_text(&self) -> String {
        self.texts
            .iter()
            .map(|t| t.text.as_str())
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Bounding box of all painted vector geometry.
    pub fn vector_bbox(&self) -> BBox {
        let mut b = BBox::EMPTY;
        for p in &self.paths {
            b.union(&p.bbox());
        }
        b
    }
}
