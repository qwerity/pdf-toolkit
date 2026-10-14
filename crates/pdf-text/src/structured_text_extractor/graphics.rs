//! What a page draws besides its text: the rules that frame table cells and
//! the filled boxes that colour them.
//!
//! Tables are recognised from these, not from how their text lines up. On the
//! pages this was measured on (the `data/text` corpus), every table that text
//! alignment got wrong was drawn: a browser prints an HTML table's borders as
//! thin filled rectangles, a typeset use table frames every cell, and a
//! colour-coded zoning key is a set of filled chips. Alignment cannot tell a
//! numbered list from a table, cannot place a check mark in a column whose
//! header is two lines high, and cannot see colour at all; the drawing can.

use pdfium_render::prelude::{
    PdfMatrix, PdfPage, PdfPageObjectCommon, PdfPageObjectsCommon, PdfPagePathObject,
    PdfPageRenderRotation, PdfPathFillMode, PdfPathSegmentType, PdfPathSegments, PdfRect,
};

/// Thicker than this, a filled rectangle is a box, not a rule. Browser-printed
/// borders measure 0.75–1.5 pt; 2.5 leaves room for heavier typeset rules.
const RULE_MAX_THICKNESS: f32 = 2.5;

/// Shorter than this, a thin mark is a glyph fragment or a tick, not a rule.
const RULE_MIN_LENGTH: f32 = 4.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Rect {
    pub left: f32,
    pub bottom: f32,
    pub right: f32,
    pub top: f32,
}

impl From<PdfRect> for Rect {
    fn from(rect: PdfRect) -> Self {
        Rect {
            left: rect.left().value,
            bottom: rect.bottom().value,
            right: rect.right().value,
            top: rect.top().value,
        }
    }
}

impl Rect {
    pub fn width(&self) -> f32 {
        self.right - self.left
    }

    pub fn height(&self) -> f32 {
        self.top - self.bottom
    }

    pub fn area(&self) -> f32 {
        self.width() * self.height()
    }

    pub fn contains(&self, x: f32, y: f32, tolerance: f32) -> bool {
        x >= self.left - tolerance
            && x <= self.right + tolerance
            && y >= self.bottom - tolerance
            && y <= self.top + tolerance
    }

    pub fn contains_rect(&self, other: &Rect, tolerance: f32) -> bool {
        other.left >= self.left - tolerance
            && other.right <= self.right + tolerance
            && other.bottom >= self.bottom - tolerance
            && other.top <= self.top + tolerance
    }

    pub fn union(&self, other: &Rect) -> Rect {
        Rect {
            left: self.left.min(other.left),
            bottom: self.bottom.min(other.bottom),
            right: self.right.max(other.right),
            top: self.top.max(other.top),
        }
    }
}

/// A straight rule: `pos` is its y when horizontal and its x when vertical;
/// `start..end` is its extent along its own axis.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Rule {
    pub horizontal: bool,
    pub pos: f32,
    pub start: f32,
    pub end: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Fill {
    pub rect: Rect,
    pub rgb: (u8, u8, u8),
}

#[derive(Debug, Default)]
pub(super) struct PageGraphics {
    pub rules: Vec<Rule>,
    pub fills: Vec<Fill>,
}

/// The rules and fills drawn directly on the page.
///
/// A path that draws several rectangles yields the rules among them:
/// Acrobat's export of a spreadsheet draws a whole row of column rules as one
/// path, and its bounds alone are one box over all of them (St. Johns
/// County's use table, whose grid was lost).
///
/// Paths inside Form XObjects are not read: their bounds are in the form's own
/// coordinate space, and a table drawn that way falls back to text alignment.
/// A page carrying `/Rotate` yields nothing, since its text is mapped to visual
/// space and its drawing would have to be mapped the same way first.
pub(super) fn collect(page: &PdfPage<'_>) -> PageGraphics {
    let mut graphics = PageGraphics::default();
    if !matches!(
        page.rotation().unwrap_or(PdfPageRenderRotation::None),
        PdfPageRenderRotation::None
    ) {
        return graphics;
    }

    for object in page.objects().iter() {
        let Some(path) = object.as_path_object() else {
            continue;
        };
        let filled = path
            .fill_mode()
            .is_ok_and(|mode| mode != PdfPathFillMode::None);
        let stroked = path.is_stroked().unwrap_or(false);
        if !filled && !stroked {
            continue;
        }
        let Ok(bounds) = object.bounds() else {
            continue;
        };
        let rgb = object
            .fill_color()
            .ok()
            .filter(|color| filled && color.alpha() > 0)
            .map(|color| (color.red(), color.green(), color.blue()));

        // A path of several rectangles: each thin one is a rule and each
        // outlined one frames a cell. What else it fills is read by its
        // extent, as a single shape's is: boxes of one fill set side by side
        // (a column of panels and their headers) are one panel.
        let Some(boxes) = boxes(path) else {
            let rect = Rect::from(bounds.to_rect());
            add_shape(&mut graphics, rect, stroked, rgb);
            continue;
        };
        let mut rest: Option<Rect> = None;
        for rect in boxes {
            if let Some(rule) = as_rule(&rect) {
                graphics.rules.push(rule);
            } else {
                if stroked {
                    graphics.rules.extend(edges(&rect));
                }
                rest = Some(rest.map_or(rect, |r| r.union(&rect)));
            }
        }
        if let (Some(rect), Some(rgb)) = (rest, rgb) {
            graphics.fills.push(Fill { rect, rgb });
        }
    }

    graphics
}

/// One shape: a thin one is a rule; an outlined one frames a cell or a text
/// box, its edges rules; a filled one is a fill.
fn add_shape(graphics: &mut PageGraphics, rect: Rect, stroked: bool, rgb: Option<(u8, u8, u8)>) {
    if let Some(rule) = as_rule(&rect) {
        graphics.rules.push(rule);
        return;
    }
    if stroked {
        graphics.rules.extend(edges(&rect));
    }
    if let Some(rgb) = rgb {
        graphics.fills.push(Fill { rect, rgb });
    }
}

/// The rectangles of a path made of two or more, in page space; `None` for a
/// path of one shape, or one with any subpath that is not an axis-aligned
/// rectangle (a curve, a diagonal, an open line), which is read by its bounds
/// as before.
fn boxes(path: &PdfPagePathObject<'_>) -> Option<Vec<Rect>> {
    let matrix = path.matrix().unwrap_or(PdfMatrix::IDENTITY);
    let mut subpaths: Vec<Vec<(f32, f32)>> = Vec::new();
    for segment in path.segments().transform(matrix).iter() {
        match segment.segment_type() {
            PdfPathSegmentType::MoveTo => subpaths.push(Vec::new()),
            PdfPathSegmentType::LineTo => {}
            _ => return None,
        }
        let (x, y) = segment.point();
        subpaths.last_mut()?.push((x.value, y.value));
    }
    if subpaths.len() < 2 {
        return None;
    }
    subpaths.iter().map(|points| as_box(points)).collect()
}

/// Corners closer than this (points) are one corner, and an edge moving less
/// than this across its axis is straight.
const CORNER_SNAP: f32 = 0.5;

/// A closed axis-aligned quadrilateral, its first corner possibly repeated.
fn as_box(points: &[(f32, f32)]) -> Option<Rect> {
    let near = |a: (f32, f32), b: (f32, f32)| {
        (a.0 - b.0).abs() <= CORNER_SNAP && (a.1 - b.1).abs() <= CORNER_SNAP
    };
    let corners = match points {
        [first, .., last] if points.len() == 5 && near(*first, *last) => &points[..4],
        _ if points.len() == 4 => points,
        _ => return None,
    };
    let axis_aligned = (0..4).all(|i| {
        let (a, b) = (corners[i], corners[(i + 1) % 4]);
        (a.0 - b.0).abs() <= CORNER_SNAP || (a.1 - b.1).abs() <= CORNER_SNAP
    });
    if !axis_aligned {
        return None;
    }
    let (xs, ys) = (corners.iter().map(|p| p.0), corners.iter().map(|p| p.1));
    Some(Rect {
        left: xs.clone().fold(f32::INFINITY, f32::min),
        right: xs.fold(f32::NEG_INFINITY, f32::max),
        bottom: ys.clone().fold(f32::INFINITY, f32::min),
        top: ys.fold(f32::NEG_INFINITY, f32::max),
    })
}

/// A thin, long rectangle is a rule along its long axis.
pub(super) fn as_rule(rect: &Rect) -> Option<Rule> {
    let (w, h) = (rect.width(), rect.height());
    if h <= RULE_MAX_THICKNESS && w >= RULE_MIN_LENGTH && w > h {
        Some(Rule {
            horizontal: true,
            pos: (rect.top + rect.bottom) / 2.0,
            start: rect.left,
            end: rect.right,
        })
    } else if w <= RULE_MAX_THICKNESS && h >= RULE_MIN_LENGTH && h > w {
        Some(Rule {
            horizontal: false,
            pos: (rect.left + rect.right) / 2.0,
            start: rect.bottom,
            end: rect.top,
        })
    } else {
        None
    }
}

fn edges(rect: &Rect) -> [Rule; 4] {
    let h = |pos| Rule {
        horizontal: true,
        pos,
        start: rect.left,
        end: rect.right,
    };
    let v = |pos| Rule {
        horizontal: false,
        pos,
        start: rect.bottom,
        end: rect.top,
    };
    [h(rect.top), h(rect.bottom), v(rect.left), v(rect.right)]
}

/// A colour's plain name, or `None` for white, black and greys: those shade
/// zebra rows and header bars far more often than they carry meaning.
pub(super) fn colour_name((r, g, b): (u8, u8, u8)) -> Option<&'static str> {
    let (r, g, b) = (
        f32::from(r) / 255.0,
        f32::from(g) / 255.0,
        f32::from(b) / 255.0,
    );
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let chroma = max - min;
    if max < 0.2 || chroma / max < 0.25 {
        return None;
    }
    let hue = if max == r {
        60.0 * ((g - b) / chroma).rem_euclid(6.0)
    } else if max == g {
        60.0 * ((b - r) / chroma + 2.0)
    } else {
        60.0 * ((r - g) / chroma + 4.0)
    };
    Some(match hue {
        h if !(15.0..345.0).contains(&h) => "red",
        h if h < 40.0 && max < 0.55 => "brown",
        h if h < 40.0 => "orange",
        h if h < 70.0 => "yellow",
        h if h < 170.0 => "green",
        h if h < 255.0 => "blue",
        h if h < 300.0 => "purple",
        _ => "pink",
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(left: f32, bottom: f32, right: f32, top: f32) -> Rect {
        Rect {
            left,
            bottom,
            right,
            top,
        }
    }

    #[test]
    fn thin_rectangles_are_rules_along_their_long_axis() {
        let h = as_rule(&rect(10.0, 99.5, 200.0, 100.5)).expect("horizontal rule");
        assert!(h.horizontal);
        assert!((h.pos - 100.0).abs() < 1e-3);
        assert_eq!((h.start, h.end), (10.0, 200.0));

        let v = as_rule(&rect(49.25, 10.0, 50.75, 90.0)).expect("vertical rule");
        assert!(!v.horizontal);
        assert!((v.pos - 50.0).abs() < 1e-3);

        assert!(
            as_rule(&rect(0.0, 0.0, 40.0, 12.0)).is_none(),
            "a box is not a rule"
        );
        assert!(
            as_rule(&rect(0.0, 0.0, 2.0, 2.0)).is_none(),
            "a dot is not a rule"
        );
    }

    #[test]
    fn a_subpath_is_a_box_only_when_closed_and_axis_aligned() {
        // A column rule of St. Johns County's use table: four corners, the
        // first repeated by the close.
        let bar = [
            (194.0, 622.9),
            (194.6, 622.9),
            (194.6, 503.0),
            (194.0, 503.0),
            (194.0, 622.9),
        ];
        let rect = as_box(&bar).expect("a box");
        assert_eq!((rect.left, rect.right), (194.0, 194.6));
        assert!(as_rule(&rect).is_some_and(|rule| !rule.horizontal));

        assert!(as_box(&bar[..4]).is_some(), "an unrepeated first corner");
        let diamond = [(0.0, 5.0), (5.0, 10.0), (10.0, 5.0), (5.0, 0.0)];
        assert!(as_box(&diamond).is_none(), "a turned square");
        assert!(as_box(&bar[..2]).is_none(), "an open line");
    }

    #[test]
    fn colours_are_named_and_greys_are_not() {
        // The chips of the AIM Norman plan's zoning key.
        assert_eq!(colour_name((106, 168, 79)), Some("green"));
        assert_eq!(colour_name((241, 194, 50)), Some("yellow"));
        assert_eq!(colour_name((204, 0, 0)), Some("red"));
        assert_eq!(colour_name((217, 217, 217)), None, "zebra grey");
        assert_eq!(colour_name((255, 255, 255)), None);
        assert_eq!(colour_name((0, 0, 0)), None);
        assert_eq!(colour_name((153, 170, 212)), Some("blue"));
    }
}
