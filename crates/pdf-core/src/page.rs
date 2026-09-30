//! Page plumbing shared by everything that opens a document.
//!
//! These are the conversions between this workspace's page model — one-based
//! and `u16`-bounded — and pdfium's zero-based `i32` indices, plus the two
//! calls every crate makes before it can do anything else. They live here
//! rather than beside the code that inspects a page because the extractors,
//! the cutter and the renderer all need them, and routing that through the
//! inspection crate would make document analysis a dependency of reading a
//! page's size.

use std::num::NonZeroU16;
use std::path::Path;

use pdfium_render::prelude::{
    PdfDocument, PdfPage, PdfPageAnnotationCommon, PdfPageObject, PdfPageObjectCommon,
    PdfPageObjectsCommon, PdfPagePathObject, PdfPageText, PdfPages, PdfPathSegmentType,
    PdfPathSegments, PdfRect, Pdfium,
};

use crate::error::{PdfError, Result};
use crate::text_quality::sanitize_extracted_text;

/// The part of the page a viewer shows, in the page's own (unrotated) user
/// space: the crop box, else the media box, else `0..width` × `0..height`.
///
/// A page box need not start at the origin — CAD exports centre it,
/// `[-595 -421 595 421]` — and every position pdfium reports is in that same
/// user space. `PdfPage::page_size()` is always anchored at the origin, so a
/// test against it drops everything left of or below `(0, 0)`: three
/// quarters of such a page.
pub fn visible_rect(page: &PdfPage<'_>) -> PdfRect {
    let boundaries = page.boundaries();
    match boundaries.crop().or_else(|_| boundaries.media()) {
        Ok(found) => {
            let (l, r) = (found.bounds.left().value, found.bounds.right().value);
            let (b, t) = (found.bounds.bottom().value, found.bounds.top().value);
            PdfRect::new_from_values(b.min(t), l.min(r), b.max(t), l.max(r))
        }
        Err(_) => page.page_size(),
    }
}

/// The page's text layer: everything inside [`visible_rect`], in document
/// order, sanitized — the one definition every extractor and the routing
/// signals read, so they judge the same text.
///
/// Use this, never `PdfPageText::all()` (clippy forbids it): that bounds the
/// text by `page_size()`, which silently truncates a page whose box is not
/// at the origin (see [`visible_rect`]).
pub fn visible_text(page: &PdfPage<'_>, text: &PdfPageText<'_>) -> String {
    sanitize_extracted_text(&text.inside_rect(visible_rect(page)))
}

/// Number of pages addressable by the workspace's `u16`-based page model.
/// Documents beyond `u16::MAX` pages (or negative pdfium error counts)
/// are rejected instead of silently truncated.
pub fn page_count_u16(pages: &PdfPages<'_>) -> Result<u16> {
    let page_count = pages.len();
    u16::try_from(page_count).map_err(|_| PdfError::UnsupportedPageCount { actual: page_count })
}

/// Converts a one-based page number to a pdfium zero-based page index.
pub fn page_index(page_number: NonZeroU16) -> i32 {
    i32::from(page_number.get()) - 1
}

/// Converts a zero-based destination index (bounded by the workspace's `u16`
/// page model) to a pdfium page index.
pub fn destination_page_index(index: u16) -> i32 {
    i32::from(index)
}

pub fn open_document<'pdfium>(
    pdfium: &'pdfium Pdfium,
    pdf_path: &Path,
) -> Result<PdfDocument<'pdfium>> {
    pdfium
        .load_pdf_from_file(pdf_path, None)
        .map_err(|source| PdfError::OpenPdf {
            path: pdf_path.to_path_buf(),
            source,
        })
}

/// A page's size in whole pixels at `scale`.
pub fn page_dimensions_px(page: &PdfPage<'_>, scale: f32) -> (u32, u32) {
    (
        scale_points_to_pixels(page.width().value, scale),
        scale_points_to_pixels(page.height().value, scale),
    )
}

fn scale_points_to_pixels(points: f32, scale: f32) -> u32 {
    (points * scale).round() as u32
}

/// The page's images: how many there are (inside Form XObjects too), and the
/// share of the page they cover, 0 to 1. Shared by page inspection's routing
/// and by structured extraction's test for a scanned page, so both judge a
/// page by one measure.
///
/// An image inside a Form XObject has bounds in the form's own space, so its
/// coverage is its share of the form's drawn extent (both in form space) times
/// the area the form occupies on the page. A scanner that wraps each page's
/// scan — and its hidden OCR text — in one form yields the whole page.
///
/// Annotations count too: a viewer paints each one's appearance over the page,
/// and a page can carry its whole content that way (a chart pasted as a
/// markup box on an otherwise empty page). An annotation's images are
/// measured like a form's, against the annotation's rectangle on the page.
pub fn image_stats(page: &PdfPage<'_>) -> (u32, f32) {
    let page_area = page.width().value * page.height().value;
    let mut count: u32 = 0;
    let mut area: f64 = 0.0;
    for object in page.objects().iter() {
        if object.as_image_object().is_some() {
            count += 1;
            area += bounded_area(&object, page_area);
        } else if let Some(form) = object.as_x_object_form_object() {
            let (nested, share) = nested_images(form);
            count += nested;
            if nested > 0 {
                area += bounded_area(&object, page_area) * share;
            }
        }
    }
    for annotation in page.annotations().iter() {
        let (nested, share) = nested_images(annotation.objects());
        count += nested;
        if nested > 0
            && let Ok(rect) = annotation.bounds()
        {
            area += rect_area(&rect, page_area) * share;
        }
    }
    let coverage = if page_area > 0.0 {
        (area / page_area as f64).min(1.0) as f32
    } else {
        0.0
    };
    (count, coverage)
}

/// A page's vector line work: every path object (inside Form XObjects and
/// annotation appearances too), and how many of them curve or run diagonally.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DrawingStats {
    /// Path objects: a drawing's line work, a table's rules, outlined text.
    pub paths: usize,
    /// Paths with a Bézier segment or a line that is neither horizontal nor
    /// vertical — counted up to the `enough_curved` the caller asked for. A
    /// table's rules and a page's frames are rectilinear; the roads of a map,
    /// a diagram's arrows and a chart's arcs are not.
    pub curved_paths: usize,
}

/// A line segment moving more than this (points) along both axes is diagonal.
const DIAGONAL_MIN_STEP: f32 = 0.5;

/// Measures the page's vector line work, for page inspection's routing: a
/// page whose content is drawn rather than written is invisible to the text
/// layer. Classifying a path means walking its segments, so once
/// `enough_curved` curved paths are found the rest are only counted.
pub fn drawing_stats(page: &PdfPage<'_>, enough_curved: usize) -> DrawingStats {
    let mut stats = DrawingStats::default();
    collect_drawing_stats(page.objects().iter(), enough_curved, &mut stats);
    for annotation in page.annotations().iter() {
        collect_drawing_stats(annotation.objects().iter(), enough_curved, &mut stats);
    }
    stats
}

fn collect_drawing_stats<'a>(
    objects: impl IntoIterator<Item = PdfPageObject<'a>>,
    enough_curved: usize,
    stats: &mut DrawingStats,
) {
    for object in objects {
        if let Some(path) = object.as_path_object() {
            stats.paths += 1;
            if stats.curved_paths < enough_curved && path_curves(path) {
                stats.curved_paths += 1;
            }
        } else if let Some(form) = object.as_x_object_form_object() {
            collect_drawing_stats(form.iter(), enough_curved, stats);
        }
    }
}

fn path_curves(path: &PdfPagePathObject<'_>) -> bool {
    let mut last: Option<(f32, f32)> = None;
    for segment in path.segments().iter() {
        let segment_type = segment.segment_type();
        if segment_type == PdfPathSegmentType::BezierTo {
            return true;
        }
        let (x, y) = segment.point();
        let point = (x.value, y.value);
        if segment_type == PdfPathSegmentType::LineTo
            && let Some((last_x, last_y)) = last
            && (point.0 - last_x).abs() > DIAGONAL_MIN_STEP
            && (point.1 - last_y).abs() > DIAGONAL_MIN_STEP
        {
            return true;
        }
        last = Some(point);
    }
    false
}

/// Images drawn inside a Form XObject or an annotation appearance: how many
/// (nested forms included), and the share, 0 to 1, of the container's drawn
/// extent they cover — measured in the container's own space, where the
/// images' bounds and everything else it draws directly agree. The share is
/// only measured when there are images, so a form of text or line work costs
/// one walk.
fn nested_images<'a, T>(objects: &'a T) -> (u32, f64)
where
    T: PdfPageObjectsCommon<'a>,
{
    let count = count_images(objects.iter());
    if count == 0 {
        return (0, 0.0);
    }
    let mut image_area = 0.0_f64;
    let mut extent: Option<(f32, f32, f32, f32)> = None;
    for object in objects.iter() {
        let Ok(bounds) = object.bounds() else {
            continue;
        };
        let (left, bottom, right, top) = (
            bounds.left().value.min(bounds.right().value),
            bounds.bottom().value.min(bounds.top().value),
            bounds.left().value.max(bounds.right().value),
            bounds.bottom().value.max(bounds.top().value),
        );
        if object.as_image_object().is_some() {
            image_area += f64::from(right - left) * f64::from(top - bottom);
        }
        extent = Some(match extent {
            None => (left, bottom, right, top),
            Some((l, b, r, t)) => (l.min(left), b.min(bottom), r.max(right), t.max(top)),
        });
    }
    let share = extent.map_or(0.0, |(l, b, r, t)| {
        let extent_area = f64::from(r - l) * f64::from(t - b);
        if extent_area > 0.0 {
            (image_area / extent_area).min(1.0)
        } else {
            0.0
        }
    });
    (count, share)
}

fn count_images<'a>(objects: impl IntoIterator<Item = PdfPageObject<'a>>) -> u32 {
    objects
        .into_iter()
        .map(|object| {
            if object.as_image_object().is_some() {
                1
            } else {
                object
                    .as_x_object_form_object()
                    .map_or(0, |form| count_images(form.iter()))
            }
        })
        .sum()
}

/// The object's page-space bounding-box area, or 0 for a degenerate or
/// implausibly huge box (e.g. a malformed XObject).
fn bounded_area(object: &PdfPageObject<'_>, page_area: f32) -> f64 {
    object
        .bounds()
        .map_or(0.0, |bounds| rect_area(&bounds.to_rect(), page_area))
}

/// A page-space rectangle's area, or 0 when it is implausibly huge.
fn rect_area(rect: &PdfRect, page_area: f32) -> f64 {
    let w = (rect.right().value - rect.left().value).abs() as f64;
    let h = (rect.top().value - rect.bottom().value).abs() as f64;
    let max_dim = page_area.sqrt() as f64 * 4.0;
    if w < max_dim && h < max_dim {
        w * h
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page(number: u16) -> NonZeroU16 {
        NonZeroU16::new(number).expect("non-zero page number")
    }

    #[test]
    fn one_based_page_numbers_become_zero_based_indices() {
        assert_eq!(page_index(page(1)), 0);
        assert_eq!(page_index(page(17)), 16);
        assert_eq!(page_index(page(u16::MAX)), i32::from(u16::MAX) - 1);
    }

    #[test]
    fn destination_indices_are_already_zero_based() {
        assert_eq!(destination_page_index(0), 0);
        assert_eq!(destination_page_index(41), 41);
    }

    #[test]
    fn pixel_dimensions_round_rather_than_truncate() {
        assert_eq!(scale_points_to_pixels(595.276, 1.0), 595);
        assert_eq!(scale_points_to_pixels(595.276, 2.0), 1191);
        assert_eq!(scale_points_to_pixels(841.89, 2.0), 1684);
    }
}
