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
    PdfPage, PdfPageObjectCommon, PdfPageObjectsCommon, PdfPageRenderRotation, PdfPathFillMode,
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
        let Ok(bounds) = object.bounds() else {
            continue;
        };
        let rect = Rect {
            left: bounds.left().value,
            bottom: bounds.bottom().value,
            right: bounds.right().value,
            top: bounds.top().value,
        };
        let filled = path
            .fill_mode()
            .is_ok_and(|mode| mode != PdfPathFillMode::None);
        let stroked = path.is_stroked().unwrap_or(false);
        if !filled && !stroked {
            continue;
        }

        if let Some(rule) = as_rule(&rect) {
            graphics.rules.push(rule);
            continue;
        }
        if stroked {
            // An outlined box frames a cell or a text box: its edges are rules.
            graphics.rules.extend(edges(&rect));
        }
        if filled
            && let Ok(color) = object.fill_color()
            && color.alpha() > 0
        {
            graphics.fills.push(Fill {
                rect,
                rgb: (color.red(), color.green(), color.blue()),
            });
        }
    }

    graphics
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
