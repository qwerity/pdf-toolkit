//! One plan sheet read into the model's frame: its fills sorted by element
//! type and converted to metres about the grid origin.
//!
//! A CAD exporter draws each element type with the fill of its pen set, so a
//! wall, the building envelope and a room floor are told apart by colour —
//! exact, and without a model call. The storeys of a set are aligned on the
//! printed grid: the bubble of the named column axis gives the origin's x,
//! the bubble of the named row axis its y, and every sheet is read in that
//! frame so the storeys stack.

use anyhow::{Result, anyhow};
use geo::{Area, Polygon};

use pdf_geometry::model::{BBox, PageContent, PaintMode, Point};

use crate::geom;
use crate::params::{AxisParams, Params};

/// A terrace hugs the envelope: this close, and no closer in area to nothing.
const TERRACE_TOUCH_M: f64 = 0.05;
/// Terraces of the reference set are 5–12 m²; the bound keeps furniture-sized
/// and site-sized envelope-coloured fills out.
const TERRACE_AREA_M2: std::ops::RangeInclusive<f64> = 5.0..=12.0;
/// A fill smaller than this (m²) is a pen artefact, not an element.
const MIN_FILL_AREA_M2: f64 = 0.003;

/// How the grid origin was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OriginSource {
    /// Both axis labels sit inside drawn circles; the circle centres are used.
    AxisBubbles,
    /// No circle around a label; its estimated text centre is used instead.
    AxisText,
}

/// One sheet's fills in metres, plus how it was placed in the frame.
#[derive(Debug, Clone)]
pub struct Sheet {
    pub page: usize,
    /// Grid origin in page units.
    pub origin: Point,
    pub origin_source: OriginSource,
    pub walls: Vec<Polygon<f64>>,
    pub envelopes: Vec<Polygon<f64>>,
    pub rooms: Vec<Polygon<f64>>,
}

impl Sheet {
    /// Reads a page's fills into the frame. Fails only when the grid origin
    /// cannot be found: without it the sheet cannot be aligned to its
    /// siblings and a model built from it would stack wrongly.
    pub fn read(content: &PageContent, params: &Params) -> Result<Self> {
        let (origin, origin_source) = axis_origin(content, &params.axis)?;
        let mm_per_point = params.mm_per_point();
        let to_world = |p: &Point| -> (f64, f64) {
            (
                (p.x - origin.x) * mm_per_point / 1000.0,
                (p.y - origin.y) * mm_per_point / 1000.0,
            )
        };

        let colours = &params.colours;
        let mut sheet = Sheet {
            page: content.page_number,
            origin,
            origin_source,
            walls: Vec::new(),
            envelopes: Vec::new(),
            rooms: Vec::new(),
        };
        for path in &content.paths {
            if !matches!(path.mode, PaintMode::Fill | PaintMode::FillStroke) {
                continue;
            }
            let fill = [path.fill.r, path.fill.g, path.fill.b];
            let bucket = if colours.wall.matches(fill, colours.tolerance) {
                &mut sheet.walls
            } else if colours.envelope.matches(fill, colours.tolerance) {
                &mut sheet.envelopes
            } else if colours.room.matches(fill, colours.tolerance) {
                &mut sheet.rooms
            } else {
                continue;
            };
            for subpath in &path.subpaths {
                let ring: Vec<(f64, f64)> = subpath.points.iter().map(to_world).collect();
                // Furniture-sized fills are the majority; the overlay pass is per ring.
                if geom::ring_area(&ring) <= MIN_FILL_AREA_M2 {
                    continue;
                }
                bucket.extend(
                    geom::polygon_from_ring(&ring)
                        .into_iter()
                        .filter(|p| p.unsigned_area() > MIN_FILL_AREA_M2),
                );
            }
        }
        Ok(sheet)
    }

    /// The building box: the largest envelope-coloured fill.
    pub fn envelope(&self) -> Option<&Polygon<f64>> {
        self.envelopes
            .iter()
            .max_by(|a, b| a.unsigned_area().total_cmp(&b.unsigned_area()))
    }

    /// Envelope-coloured areas hugging the box without overlapping it: the
    /// balconies and terraces.
    pub fn side_terraces(&self, envelope: &Polygon<f64>) -> Vec<Polygon<f64>> {
        self.envelopes
            .iter()
            .filter(|p| !std::ptr::eq(*p, envelope))
            .filter(|p| TERRACE_AREA_M2.contains(&p.unsigned_area()))
            .filter(|p| geom::intersection_area(p, envelope) <= 0.5)
            .filter(|p| geom::polygon_distance(p, envelope) < TERRACE_TOUCH_M)
            .cloned()
            .collect()
    }
}

/// A drawn circle: an axis bubble, when a grid label sits inside it.
#[derive(Debug, Clone, Copy)]
struct Circle {
    centre: Point,
    radius: f64,
}

/// Smallest and largest bubble radius worth considering, page units.
const BUBBLE_RADIUS: std::ops::RangeInclusive<f64> = 3.0..=40.0;

fn circles(content: &PageContent) -> Vec<Circle> {
    let mut out = Vec::new();
    for path in &content.paths {
        for subpath in &path.subpaths {
            if subpath.points.len() < 8 {
                continue;
            }
            let bbox: BBox = subpath.bbox();
            let (w, h) = (bbox.width(), bbox.height());
            let radius = (w + h) / 4.0;
            if !BUBBLE_RADIUS.contains(&radius) || (w - h).abs() > 0.15 * w.max(h) {
                continue;
            }
            let centre = bbox.center();
            let round = subpath
                .points
                .iter()
                .all(|p| (p.distance(&centre) - radius).abs() < 0.15 * radius);
            if round {
                out.push(Circle { centre, radius });
            }
        }
    }
    out
}

/// Where a label's glyphs are centred: the run origin advanced by half the
/// label's estimated width, lifted by a third of the size.
fn label_centre(origin: Point, size: f64, angle: f64, chars: usize) -> Point {
    const AVERAGE_GLYPH_WIDTH: f64 = 0.6;
    let (dx, dy) = (AVERAGE_GLYPH_WIDTH * size * chars as f64 / 2.0, 0.35 * size);
    let (sin, cos) = angle.sin_cos();
    Point::new(
        origin.x + dx * cos - dy * sin,
        origin.y + dx * sin + dy * cos,
    )
}

/// One occurrence of an axis label: its centre, size, and the bubble drawn
/// around it if any.
#[derive(Debug, Clone, Copy)]
struct LabelHit {
    centre: Point,
    size: f64,
    bubble: Option<Circle>,
}

fn label_hits(
    content: &PageContent,
    label: &str,
    font_size: Option<f64>,
    circles: &[Circle],
) -> Vec<LabelHit> {
    let mut hits = Vec::new();
    for item in &content.texts {
        for token in item.tokens() {
            if token.text != label {
                continue;
            }
            if let Some(wanted) = font_size
                && (token.size - wanted).abs() > 0.8
            {
                continue;
            }
            let centre = label_centre(token.origin, token.size, token.angle, label.chars().count());
            let bubble = circles
                .iter()
                .filter(|c| c.centre.distance(&centre) < c.radius)
                .min_by(|a, b| a.radius.total_cmp(&b.radius))
                .copied();
            hits.push(LabelHit {
                centre,
                size: token.size,
                bubble,
            });
        }
    }
    hits
}

/// A grid bubble: a single short label drawn inside a circle.
#[derive(Debug, Clone)]
pub struct AxisBubble {
    pub label: String,
    /// The circle's centre, page units.
    pub centre: Point,
    pub size: f64,
}

/// Every short label (a digit or two, or one letter) drawn inside a circle:
/// the grid bubbles, which is how [`crate::detect`] learns the grid's
/// labels without being told them.
pub fn axis_bubbles(content: &PageContent) -> Vec<AxisBubble> {
    let circles = circles(content);
    let mut out = Vec::new();
    for item in &content.texts {
        for token in item.tokens() {
            let chars = token.text.chars().count();
            let is_label = (chars <= 2 && token.text.chars().all(|c| c.is_ascii_digit()))
                || (chars == 1 && token.text.chars().all(char::is_alphabetic));
            if !is_label {
                continue;
            }
            let centre = label_centre(token.origin, token.size, token.angle, chars);
            if let Some(circle) = circles
                .iter()
                .filter(|c| c.centre.distance(&centre) < c.radius)
                .min_by(|a, b| a.radius.total_cmp(&b.radius))
            {
                out.push(AxisBubble {
                    label: token.text.to_string(),
                    centre: circle.centre,
                    size: token.size,
                });
            }
        }
    }
    out
}

/// The grid intersection the model is drawn about.
///
/// The column axis is the label's bottom-most bubble (grids are lettered
/// along the bottom edge, and a section mark can reuse the digit higher up);
/// the row axis is the label's left-most one. Bubbles beat bare text, and a
/// larger font beats a smaller one — a zone stamp's room number is set
/// smaller than a grid bubble.
fn axis_origin(content: &PageContent, axis: &AxisParams) -> Result<(Point, OriginSource)> {
    let circles = circles(content);
    let column = label_hits(content, &axis.column_label, axis.font_size, &circles);
    let row = label_hits(content, &axis.row_label, axis.font_size, &circles);
    if column.is_empty() || row.is_empty() {
        return Err(anyhow!(
            "page {}: grid labels '{}' and '{}' not both found in the text layer",
            content.page_number,
            axis.column_label,
            axis.row_label
        ));
    }

    let bubbled = |hits: &[LabelHit]| hits.iter().any(|h| h.bubble.is_some());
    let source = if bubbled(&column) && bubbled(&row) {
        OriginSource::AxisBubbles
    } else {
        OriginSource::AxisText
    };
    let position = |h: &LabelHit| h.bubble.map_or(h.centre, |c| c.centre);
    let rank = |h: &LabelHit| (h.bubble.is_some(), (h.size * 10.0).round() as i64);
    let best_column = column
        .iter()
        .max_by(|a, b| {
            rank(a)
                .cmp(&rank(b))
                .then_with(|| position(b).y.total_cmp(&position(a).y))
        })
        .expect("non-empty");
    let best_row = row
        .iter()
        .max_by(|a, b| {
            rank(a)
                .cmp(&rank(b))
                .then_with(|| position(b).x.total_cmp(&position(a).x))
        })
        .expect("non-empty");
    Ok((
        Point::new(position(best_column).x, position(best_row).y),
        source,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pdf_geometry::model::{Rgb, SubPath, TextItem, VectorPath};

    fn circle_path(cx: f64, cy: f64, r: f64) -> VectorPath {
        let points = (0..24)
            .map(|i| {
                let a = f64::from(i) * std::f64::consts::TAU / 24.0;
                Point::new(cx + r * a.cos(), cy + r * a.sin())
            })
            .collect();
        VectorPath {
            subpaths: vec![SubPath {
                points,
                closed: true,
            }],
            mode: PaintMode::Stroke,
            stroke: Rgb::BLACK,
            fill: Rgb::BLACK,
            width: 0.5,
            layer: None,
        }
    }

    fn text(t: &str, x: f64, y: f64, size: f64) -> TextItem {
        TextItem {
            text: t.into(),
            origin: Point::new(x, y),
            size,
            angle: 0.0,
            color: Rgb::BLACK,
        }
    }

    fn page(paths: Vec<VectorPath>, texts: Vec<TextItem>) -> PageContent {
        PageContent {
            page_number: 7,
            media_box: BBox {
                min_x: 0.0,
                min_y: 0.0,
                max_x: 1190.0,
                max_y: 842.0,
            },
            paths,
            texts,
            image_count: 0,
            image_coverage: 0.0,
        }
    }

    fn axis() -> AxisParams {
        AxisParams {
            column_label: "1".into(),
            row_label: "Ա".into(),
            font_size: None,
        }
    }

    #[test]
    fn the_origin_is_the_bubble_centres_not_the_text_origins() {
        // The bottom "1" bubble at (400, 176); a smaller "1" higher up in a
        // zone stamp; the row bubble "Ա" at (237, 334).
        let content = page(
            vec![
                circle_path(400.0, 176.0, 11.0),
                circle_path(237.0, 334.0, 11.0),
                circle_path(600.0, 500.0, 9.0),
            ],
            vec![
                text("1", 396.1, 171.5, 11.66),
                text("1", 597.0, 496.5, 9.72),
                text("Ա", 232.6, 330.2, 11.66),
            ],
        );
        let (origin, source) = axis_origin(&content, &axis()).expect("found");
        assert_eq!(source, OriginSource::AxisBubbles);
        assert!((origin.x - 400.0).abs() < 1e-9 && (origin.y - 334.0).abs() < 1e-9);
    }

    #[test]
    fn without_bubbles_the_text_centres_stand_in_and_say_so() {
        let content = page(
            Vec::new(),
            vec![
                text("1", 396.1, 171.5, 11.66),
                text("Ա", 232.6, 330.2, 11.66),
            ],
        );
        let (origin, source) = axis_origin(&content, &axis()).expect("found");
        assert_eq!(source, OriginSource::AxisText);
        assert!((origin.x - 399.6).abs() < 0.1, "{origin:?}");
        assert!((origin.y - 334.3).abs() < 0.1, "{origin:?}");
    }

    #[test]
    fn a_missing_label_is_a_named_failure() {
        let content = page(Vec::new(), vec![text("1", 396.1, 171.5, 11.66)]);
        let error = axis_origin(&content, &axis()).unwrap_err().to_string();
        assert!(error.contains("Ա"), "{error}");
    }
}
