//! Recovering the labels printed on a drawing — parcel codes, plot numbers —
//! as text attached to a position on the map.
//!
//! These matter: a cadastral code is the identifier that ties a polygon to a
//! registry, so a parcel boundary without one is much less useful. They are also
//! the hardest thing on the sheet to read, because CAD exports routinely convert
//! them to glyph outlines — the contour of each digit as a path, with no text
//! behind it.
//!
//! Two sources, each used for what it is good at:
//!
//! * **Where** a label sits comes from the geometry. The glyph outlines of one
//!   label form a tight cluster of small marks, and the cluster's centre is
//!   exact.
//! * **What** it says comes from the page's text layer when the exporter left
//!   it readable, and otherwise from a vision model, which is asked only to
//!   read — never to place.

use serde::{Deserialize, Serialize};

use pdf_geometry::model::{BBox, PageContent, Point, VectorPath};

/// Where a label's text came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LabelSource {
    /// Read from the page's text layer.
    TextLayer,
    /// Read by a vision model from the rendered page.
    Vlm { model: String },
}

/// A label recovered from the drawing, positioned in page space.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MapLabel {
    pub text: String,
    /// Centre of the label, in page units.
    pub position: Point,
    pub source: LabelSource,
}

/// A cluster of small marks that together look like one printed label.
///
/// Produced from the geometry the exporter filters out, so its centre is exact
/// even when nothing on the page can be read.
#[derive(Debug, Clone)]
pub struct LabelCluster {
    pub centre: Point,
    pub bbox: BBox,
    /// Number of glyph contours in the cluster.
    pub marks: usize,
}

/// Tunables for finding label clusters.
#[derive(Debug, Clone, Copy)]
pub struct ClusterOptions {
    /// Marks larger than this across, in page units, are drawing rather than
    /// lettering.
    pub max_mark_extent: f64,
    /// Marks whose bounding boxes are within this distance, in page units, are
    /// part of the same label. Roughly the gap between adjacent digits.
    pub join_distance: f64,
    /// Fewest marks a cluster needs. A single contour is more often a stray
    /// tick than a label.
    pub min_marks: usize,
    /// Largest a finished cluster may be across, in page units. Hatching is
    /// made of the same small marks as lettering and single-linkage will chain
    /// a whole hatched room into one blob; a label is a few centimetres of
    /// paper and never approaches this.
    pub max_cluster_extent: f64,
}

impl Default for ClusterOptions {
    fn default() -> Self {
        Self {
            max_mark_extent: 12.0,
            join_distance: 3.0,
            min_marks: 3,
            max_cluster_extent: 60.0,
        }
    }
}

/// Groups the page's small marks into candidate labels.
///
/// Single-linkage clustering on bounding-box proximity: glyphs of one label
/// nearly touch, while separate labels sit metres apart on the sheet.
pub fn find_clusters(
    paths: &[VectorPath],
    exclude: &[BBox],
    options: ClusterOptions,
) -> Vec<LabelCluster> {
    let mut marks: Vec<BBox> = Vec::new();
    for path in paths {
        for sub in &path.subpaths {
            let bbox = sub.bbox();
            if bbox.is_empty() {
                continue;
            }
            if bbox.width().hypot(bbox.height()) > options.max_mark_extent {
                continue;
            }
            if exclude.iter().any(|ex| bbox.overlap_ratio(ex) > 0.5) {
                continue;
            }
            marks.push(bbox);
        }
    }
    if marks.is_empty() {
        return Vec::new();
    }

    let mut owner: Vec<usize> = (0..marks.len()).collect();
    fn root(owner: &mut [usize], mut i: usize) -> usize {
        while owner[i] != i {
            owner[i] = owner[owner[i]];
            i = owner[i];
        }
        i
    }

    // Marks are bucketed by position so only nearby pairs are compared; a page
    // can hold hundreds of thousands of them.
    let cell = options.join_distance.max(1.0) * 4.0;
    let mut grid: std::collections::HashMap<(i64, i64), Vec<usize>> =
        std::collections::HashMap::new();
    for (index, bbox) in marks.iter().enumerate() {
        let c = bbox.center();
        grid.entry(((c.x / cell) as i64, (c.y / cell) as i64))
            .or_default()
            .push(index);
    }
    for (index, bbox) in marks.iter().enumerate() {
        let c = bbox.center();
        let (cx, cy) = ((c.x / cell) as i64, (c.y / cell) as i64);
        for dx in -1..=1 {
            for dy in -1..=1 {
                let Some(bucket) = grid.get(&(cx + dx, cy + dy)) else {
                    continue;
                };
                for &other in bucket {
                    if other <= index || !near(bbox, &marks[other], options.join_distance) {
                        continue;
                    }
                    let (a, b) = (root(&mut owner, index), root(&mut owner, other));
                    if a != b {
                        owner[a] = b;
                    }
                }
            }
        }
    }

    let mut groups: std::collections::HashMap<usize, (BBox, usize)> =
        std::collections::HashMap::new();
    for (index, bbox) in marks.iter().enumerate() {
        let key = root(&mut owner, index);
        let entry = groups.entry(key).or_insert((BBox::EMPTY, 0));
        entry.0.union(bbox);
        entry.1 += 1;
    }

    let mut out: Vec<LabelCluster> = groups
        .into_values()
        .filter(|(bbox, marks)| {
            *marks >= options.min_marks
                && bbox.width().hypot(bbox.height()) <= options.max_cluster_extent
        })
        .map(|(bbox, marks)| LabelCluster {
            centre: bbox.center(),
            bbox,
            marks,
        })
        .collect();
    // Reading order: down the page, then across.
    out.sort_by(|a, b| {
        b.centre
            .y
            .partial_cmp(&a.centre.y)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(
                a.centre
                    .x
                    .partial_cmp(&b.centre.x)
                    .unwrap_or(std::cmp::Ordering::Equal),
            )
    });
    out
}

/// Whether two boxes are close enough to belong to the same label.
fn near(a: &BBox, b: &BBox, distance: f64) -> bool {
    let dx = (a.min_x - b.max_x).max(b.min_x - a.max_x).max(0.0);
    let dy = (a.min_y - b.max_y).max(b.min_y - a.max_y).max(0.0);
    dx <= distance && dy <= distance
}

/// Labels the page's text layer already exposes, which needs no model at all.
pub fn from_text_layer(page: &PageContent, exclude: &[BBox]) -> Vec<MapLabel> {
    let re = label_regex();
    page.texts
        .iter()
        .flat_map(|t| t.tokens())
        .filter(|t| re.is_match(t.text))
        .filter(|t| !exclude.iter().any(|ex| ex.contains(t.origin)))
        .map(|t| MapLabel {
            text: t.text.to_string(),
            position: t.origin,
            source: LabelSource::TextLayer,
        })
        .collect()
}

/// A parcel label: a short zero-padded number, or a full cadastral code.
fn label_regex() -> &'static regex::Regex {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| {
        regex::Regex::new(r"^(\d{2}-\d{3}-\d{4}-\d{4}|0\d{2,3})$").expect("static regex")
    })
}

/// Snaps model-read labels onto the cluster centres found in the geometry.
///
/// A model reads digits far better than it places them, so its position is used
/// only to choose which cluster a reading belongs to; the position written out
/// is the cluster's own. A reading with no cluster near it is dropped rather
/// than placed on the model's estimate.
pub fn snap_to_clusters(
    readings: &[(String, Point)],
    clusters: &[LabelCluster],
    max_distance: f64,
    source: LabelSource,
) -> Vec<MapLabel> {
    let mut taken = vec![false; clusters.len()];
    let mut out = Vec::new();
    for (text, estimate) in readings {
        let best = clusters
            .iter()
            .enumerate()
            .filter(|(i, _)| !taken[*i])
            .map(|(i, c)| (i, c.centre.distance(estimate)))
            .min_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
        let Some((index, distance)) = best else { break };
        if distance > max_distance {
            continue;
        }
        taken[index] = true;
        out.push(MapLabel {
            text: text.clone(),
            position: clusters[index].centre,
            source: source.clone(),
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use pdf_geometry::model::{PaintMode, Rgb, SubPath};

    fn mark(x: f64, y: f64, w: f64, h: f64) -> VectorPath {
        VectorPath {
            subpaths: vec![SubPath {
                points: vec![
                    Point::new(x, y),
                    Point::new(x + w, y),
                    Point::new(x + w, y + h),
                    Point::new(x, y + h),
                ],
                closed: true,
            }],
            mode: PaintMode::Stroke,
            stroke: Rgb::BLACK,
            fill: Rgb::BLACK,
            width: 0.3,
            layer: None,
        }
    }

    #[test]
    fn groups_adjacent_glyphs_into_one_label() {
        // Four digits side by side, then another label well away from them.
        let mut paths: Vec<VectorPath> = (0..4)
            .map(|i| mark(100.0 + i as f64 * 4.0, 50.0, 3.0, 5.0))
            .collect();
        paths.extend((0..4).map(|i| mark(300.0 + i as f64 * 4.0, 200.0, 3.0, 5.0)));
        let clusters = find_clusters(&paths, &[], ClusterOptions::default());
        assert_eq!(clusters.len(), 2);
        assert!(clusters.iter().all(|c| c.marks == 4));
        // Ordered down the page, so the higher label comes first.
        assert!((clusters[0].centre.y - 202.5).abs() < 0.01);
        assert!((clusters[0].centre.x - 307.5).abs() < 0.01);
        assert!((clusters[1].centre.x - 107.5).abs() < 0.01);
    }

    #[test]
    fn ignores_marks_too_large_to_be_lettering() {
        let paths = vec![mark(0.0, 0.0, 200.0, 150.0)];
        assert!(find_clusters(&paths, &[], ClusterOptions::default()).is_empty());
    }

    #[test]
    fn discards_a_cluster_that_grew_into_hatching() {
        // A run of marks chained across the page: single-linkage will merge
        // them, and the result is a hatched area, not a label.
        let paths: Vec<VectorPath> = (0..60)
            .map(|i| mark(i as f64 * 4.0, 0.0, 3.0, 3.0))
            .collect();
        assert!(find_clusters(&paths, &[], ClusterOptions::default()).is_empty());
    }

    #[test]
    fn ignores_a_lone_tick() {
        let paths = vec![mark(10.0, 10.0, 2.0, 2.0)];
        assert!(find_clusters(&paths, &[], ClusterOptions::default()).is_empty());
    }

    #[test]
    fn a_reading_is_placed_on_its_cluster_not_its_estimate() {
        let clusters = vec![LabelCluster {
            centre: Point::new(100.0, 50.0),
            bbox: BBox {
                min_x: 94.0,
                min_y: 47.0,
                max_x: 106.0,
                max_y: 53.0,
            },
            marks: 4,
        }];
        // The model's estimate is 8 units off; the output must use the cluster.
        let readings = vec![("0092".to_string(), Point::new(106.0, 56.0))];
        let out = snap_to_clusters(&readings, &clusters, 20.0, LabelSource::TextLayer);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].position, Point::new(100.0, 50.0));
    }

    #[test]
    fn a_reading_with_no_cluster_nearby_is_dropped() {
        let clusters = vec![LabelCluster {
            centre: Point::new(100.0, 50.0),
            bbox: BBox {
                min_x: 94.0,
                min_y: 47.0,
                max_x: 106.0,
                max_y: 53.0,
            },
            marks: 4,
        }];
        let readings = vec![("0092".to_string(), Point::new(900.0, 700.0))];
        assert!(snap_to_clusters(&readings, &clusters, 20.0, LabelSource::TextLayer).is_empty());
    }
}
