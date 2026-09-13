//! Cheap structural features computed from a page's extracted content.
//!
//! These are what makes the tool affordable: a 30-page dossier is mostly forms,
//! scans and floor plans, and features alone settle most of those without
//! spending a model call. Only the pages that remain ambiguous are rendered and
//! sent to a vision model.

use regex::Regex;
use serde::{Deserialize, Serialize};

use pdf_geo::control::table_candidates_from_text_layer;
use pdf_geometry::model::{PageContent, PaintMode};

use crate::sheet::{self, SheetRole};

/// Structural summary of one page.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PageFeatures {
    pub page_number: usize,
    pub path_count: usize,
    pub point_count: usize,
    pub text_count: usize,
    /// Characters of decoded text, a proxy for how document-like the page is.
    pub text_length: usize,
    pub image_count: usize,
    /// Fraction of the page covered by raster images.
    pub image_coverage: f64,
    /// Fraction of the page area spanned by vector geometry.
    pub vector_coverage: f64,
    /// Total drawn line length divided by the page diagonal.
    pub line_density: f64,
    /// Distinct stroke colours, excluding near-white.
    pub distinct_colors: usize,
    /// Distinct optional-content group names, i.e. CAD layers.
    pub layer_count: usize,
    /// Cadastral parcel identifiers such as `10-001-0123-0789`.
    pub cadastral_codes: usize,
    /// Sheet scales printed on the page, e.g. `500` for `1:500`.
    pub declared_scales: Vec<f64>,
    /// Whether a turning-point table was recoverable from the text layer.
    pub has_control_table: bool,
    pub control_rows: usize,
    /// The sheet name printed in the title block, when the page has one.
    pub sheet_title: Option<String>,
    /// What that name says the sheet is.
    pub sheet_role: Option<SheetRole>,
    /// A level the title names (`+3,60 նիշով` → 3.6), metres.
    pub sheet_level_m: Option<f64>,
    /// Distinct level marks printed on the page (`+7,200`, `±0,000`).
    pub level_marks: usize,
    /// Painted fills, as opposed to strokes: a CAD plan is made of them,
    /// an elevation of lines and hatch.
    pub filled_paths: usize,
}

fn cadastral_regex() -> &'static Regex {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\d{2}-\d{3}-\d{4}-\d{4}").expect("static regex"))
}

/// Stroke colours lighter than this are construction lines, not content.
const WHITE_CUTOFF: f64 = 0.93;

impl PageFeatures {
    pub fn compute(page: &PageContent) -> Self {
        let page_area = page.media_box.area().max(1.0);
        let diagonal = page
            .media_box
            .width()
            .hypot(page.media_box.height())
            .max(1.0);

        let mut colors: Vec<String> = Vec::new();
        let mut layers: Vec<&str> = Vec::new();
        let mut length = 0.0;
        let mut points = 0usize;
        let mut filled = 0usize;
        for path in &page.paths {
            length += path.total_length();
            points += path.point_count();
            if matches!(path.mode, PaintMode::Fill | PaintMode::FillStroke) {
                filled += 1;
            }
            if path.stroke.luminance() <= WHITE_CUTOFF {
                let hex = path.stroke.to_hex();
                if !colors.contains(&hex) {
                    colors.push(hex);
                }
            }
            if let Some(layer) = path.layer.as_deref()
                && !layers.contains(&layer)
            {
                layers.push(layer);
            }
        }

        let text = page.concatenated_text();
        let tables = table_candidates_from_text_layer(page);
        let title = sheet::sheet_title(page);
        Self {
            page_number: page.page_number,
            path_count: page.paths.len(),
            point_count: points,
            text_count: page.texts.len(),
            text_length: text.chars().count(),
            image_count: page.image_count,
            image_coverage: page.image_coverage,
            vector_coverage: (page.vector_bbox().area() / page_area).min(1.0),
            line_density: length / diagonal,
            distinct_colors: colors.len(),
            layer_count: layers.len(),
            cadastral_codes: cadastral_regex().find_iter(&text).count(),
            declared_scales: pdf_geo::control::declared_scale_denominators(page),
            has_control_table: !tables.is_empty(),
            control_rows: tables.first().map(|t| t.rows.len()).unwrap_or(0),
            sheet_role: title.as_ref().and_then(|t| t.role),
            sheet_level_m: title.as_ref().and_then(|t| t.level_m),
            sheet_title: title.map(|t| t.title),
            level_marks: sheet::level_marks(page).len(),
            filled_paths: filled,
        }
    }
}
