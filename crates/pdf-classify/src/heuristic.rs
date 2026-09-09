//! Rule-based page classification from structural features.
//!
//! The rules encode what separates a map-placeable drawing from everything else
//! in a building-permit dossier. They are deliberately conservative: a page they
//! are unsure about is reported as [`Confidence::Ambiguous`] so that the caller
//! can spend a vision-model call on it rather than guessing.

use serde::{Deserialize, Serialize};

use super::features::PageFeatures;

/// What a page contains.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PageClass {
    /// A survey or cadastral drawing with turning-point coordinates: the only
    /// class that can be georeferenced from the page alone.
    TopographicSurvey,
    /// A site or master plan drawn in plan view over its surroundings.
    SitePlan,
    /// A cadastral extract or situation map showing the parcel in context.
    CadastralPlan,
    /// A building floor plan: plan view, but of an interior, so not placeable.
    FloorPlan,
    /// An elevation or section: not a plan view at all.
    ElevationOrSection,
    /// Engineering detail, schedule, or construction diagram.
    TechnicalDetail,
    /// A scanned document, certificate or photograph.
    ScannedDocument,
    /// A scan or photograph *of a technical drawing*. Its line work exists only
    /// as pixels and cannot be vectorised — but a coordinate table printed on
    /// it can still be read, and those coordinates are the parcel boundary.
    ScannedDrawing,
    /// A form, letter or table with no drawing.
    TextDocument,
    Unknown,
}

impl PageClass {
    /// Whether the page carries a technical drawing of any kind.
    ///
    /// Broader than [`PageClass::is_mappable`] and answering a different
    /// question: a floor plan is unmistakably a CAD drawing, it simply is not a
    /// drawing of terrain. Callers looking for "the drawings in this dossier"
    /// want this; callers looking for "what can go on a map" want the other.
    pub fn is_drawing(self) -> bool {
        matches!(
            self,
            PageClass::TopographicSurvey
                | PageClass::SitePlan
                | PageClass::CadastralPlan
                | PageClass::FloorPlan
                | PageClass::ElevationOrSection
                | PageClass::TechnicalDetail
                | PageClass::ScannedDrawing
        )
    }

    /// Whether this class of page is worth trying to place on a map.
    /// Whether this class of page is worth trying to place on a map.
    ///
    /// A scanned drawing counts: its geometry cannot be recovered, but the
    /// coordinate table printed on it can, and those coordinates describe the
    /// parcel directly.
    pub fn is_mappable(self) -> bool {
        matches!(
            self,
            PageClass::TopographicSurvey
                | PageClass::SitePlan
                | PageClass::CadastralPlan
                | PageClass::ScannedDrawing
        )
    }

    /// Whether the page's geometry can be recovered as vectors at all.
    ///
    /// False for a scan, whose line work is pixels. Such a page can still yield
    /// its parcel from a coordinate table, so it is mappable without being
    /// vectorisable — a distinction worth keeping, because it decides whether
    /// an empty geometry export is a failure or the expected outcome.
    pub fn is_vector(self) -> bool {
        self.is_drawing() && self != PageClass::ScannedDrawing
    }

    pub fn as_str(self) -> &'static str {
        match self {
            PageClass::TopographicSurvey => "topographic_survey",
            PageClass::SitePlan => "site_plan",
            PageClass::CadastralPlan => "cadastral_plan",
            PageClass::FloorPlan => "floor_plan",
            PageClass::ElevationOrSection => "elevation_or_section",
            PageClass::TechnicalDetail => "technical_detail",
            PageClass::ScannedDocument => "scanned_document",
            PageClass::ScannedDrawing => "scanned_drawing",
            PageClass::TextDocument => "text_document",
            PageClass::Unknown => "unknown",
        }
    }

    /// Parses a class name, tolerating the spacing and hyphenation a model or a
    /// label file might use.
    pub fn parse(s: &str) -> Option<PageClass> {
        let normalized = s.trim().to_ascii_lowercase().replace([' ', '-'], "_");
        Self::all()
            .iter()
            .copied()
            .chain(std::iter::once(PageClass::Unknown))
            .find(|c| c.as_str() == normalized)
    }

    /// Every class a model may return, for prompt construction. `Unknown` is
    /// excluded deliberately: it is what the pipeline records when nothing
    /// decided, never something to offer as an answer.
    pub fn all() -> &'static [PageClass] {
        &[
            PageClass::TopographicSurvey,
            PageClass::SitePlan,
            PageClass::CadastralPlan,
            PageClass::FloorPlan,
            PageClass::ElevationOrSection,
            PageClass::TechnicalDetail,
            PageClass::ScannedDocument,
            PageClass::ScannedDrawing,
            PageClass::TextDocument,
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_forms_a_model_might_return() {
        assert_eq!(PageClass::parse("site_plan"), Some(PageClass::SitePlan));
        assert_eq!(PageClass::parse(" Site Plan "), Some(PageClass::SitePlan));
        assert_eq!(
            PageClass::parse("elevation-or-section"),
            Some(PageClass::ElevationOrSection)
        );
        assert_eq!(PageClass::parse("blueprint"), None);
    }

    #[test]
    fn a_floor_plan_is_a_drawing_but_not_a_map() {
        assert!(PageClass::FloorPlan.is_drawing());
        assert!(!PageClass::FloorPlan.is_mappable());
        assert!(PageClass::ElevationOrSection.is_drawing());
        assert!(!PageClass::ScannedDocument.is_drawing());
        assert!(!PageClass::TextDocument.is_drawing());
    }

    #[test]
    fn only_plan_views_of_terrain_are_mappable() {
        assert!(PageClass::TopographicSurvey.is_mappable());
        assert!(PageClass::SitePlan.is_mappable());
        assert!(PageClass::CadastralPlan.is_mappable());
        for class in [
            PageClass::FloorPlan,
            PageClass::ElevationOrSection,
            PageClass::TechnicalDetail,
            PageClass::ScannedDocument,
            PageClass::TextDocument,
            PageClass::Unknown,
        ] {
            assert!(
                !class.is_mappable(),
                "{} should not be mappable",
                class.as_str()
            );
        }
    }

    fn features(path_count: usize, text_length: usize, image_coverage: f64) -> PageFeatures {
        PageFeatures {
            page_number: 1,
            path_count,
            point_count: path_count * 4,
            text_count: 0,
            text_length,
            image_count: 0,
            image_coverage,
            vector_coverage: 0.5,
            line_density: 0.0,
            distinct_colors: 1,
            layer_count: 0,
            cadastral_codes: 0,
            declared_scales: Vec::new(),
            has_control_table: false,
            control_rows: 0,
        }
    }

    #[test]
    fn a_form_is_settled_without_a_model() {
        let verdict = heuristic_label(&features(9, 2400, 0.0));
        assert_eq!(verdict.class, PageClass::TextDocument);
        assert_eq!(verdict.confidence, Confidence::Certain);
    }

    #[test]
    fn a_scan_is_recognised_but_its_subject_is_left_to_a_model() {
        // Structure can see the page is a picture; it cannot see whether the
        // picture is of a passport or of a cadastral plan, and those deserve
        // very different treatment.
        let verdict = heuristic_label(&features(14, 1, 0.55));
        assert_eq!(verdict.class, PageClass::ScannedDocument);
        assert_eq!(verdict.confidence, Confidence::Ambiguous);
    }

    #[test]
    fn a_scanned_drawing_is_mappable_but_not_vectorisable() {
        // Its parcel comes from the coordinate table, not from its line work.
        assert!(PageClass::ScannedDrawing.is_drawing());
        assert!(PageClass::ScannedDrawing.is_mappable());
        assert!(!PageClass::ScannedDrawing.is_vector());
        assert!(PageClass::CadastralPlan.is_vector());
    }

    #[test]
    fn a_drawing_is_handed_to_the_model() {
        let verdict = heuristic_label(&features(1600, 200, 0.0));
        assert_eq!(verdict.confidence, Confidence::Ambiguous);
    }

    #[test]
    fn a_coordinate_table_settles_a_page_on_its_own() {
        let mut f = features(600, 200, 0.0);
        f.has_control_table = true;
        f.control_rows = 7;
        let verdict = heuristic_label(&f);
        assert_eq!(verdict.class, PageClass::TopographicSurvey);
        assert_eq!(verdict.confidence, Confidence::Certain);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    /// The structural evidence is decisive on its own.
    Certain,
    /// A vision model would add information.
    Ambiguous,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PageVerdict {
    pub class: PageClass,
    pub confidence: Confidence,
    /// Human-readable justification, carried into reports.
    pub reason: String,
}

/// Fewest painted paths a real drawing has. Forms and letters get their rules
/// and boxes from a couple of dozen paths; the sparsest CAD sheet in the corpus
/// has several hundred, so the gap is wide and unambiguous.
const MIN_DRAWING_PATHS: usize = 120;
/// Total line length, relative to the page diagonal, above which a page is a
/// drawing regardless of how few paths it took to make. Catches sheets whose
/// exporter emits one path per feature.
const MIN_DRAWING_LINE_DENSITY: f64 = 40.0;
/// Above this raster coverage the page is a scan or a photograph, and its
/// vector layer is at most a frame.
const SCAN_IMAGE_COVERAGE: f64 = 0.35;
/// Characters of decoded text above which a page with no drawing is prose or a
/// form rather than a near-blank sheet.
const MIN_TEXT_PAGE_LENGTH: usize = 300;

/// Classifies a page from structural features alone.
///
/// The rules run cheapest-first and stop at the first decisive one. Only a
/// coordinate table, a raster-dominated page and a text page are treated as
/// certain; every drawing is handed on as ambiguous, because telling a site
/// plan from a floor plan is a visual judgement that structure cannot make.
pub fn heuristic_label(f: &PageFeatures) -> PageVerdict {
    let verdict = |class: PageClass, confidence: Confidence, reason: &str| PageVerdict {
        class,
        confidence,
        reason: reason.to_string(),
    };

    // A recoverable turning-point table is conclusive: nothing but a survey
    // sheet prints projected coordinates for its corners.
    if f.has_control_table && f.control_rows >= 3 {
        return verdict(
            PageClass::TopographicSurvey,
            Confidence::Certain,
            "turning-point coordinate table present in the text layer",
        );
    }

    let has_drawing =
        f.path_count >= MIN_DRAWING_PATHS || f.line_density >= MIN_DRAWING_LINE_DENSITY;

    if f.image_coverage >= SCAN_IMAGE_COVERAGE && !has_drawing {
        // Ambiguous, not certain: structure can see that the page is a picture
        // but not what the picture is of, and a scanned cadastral plan is worth
        // very different treatment from a scanned passport.
        return verdict(
            PageClass::ScannedDocument,
            Confidence::Ambiguous,
            "page is raster imagery; its subject cannot be told from structure",
        );
    }

    if !has_drawing {
        if f.text_length > MIN_TEXT_PAGE_LENGTH {
            return verdict(
                PageClass::TextDocument,
                Confidence::Certain,
                "substantial text and no line work beyond a form's rules",
            );
        }
        return verdict(
            PageClass::Unknown,
            Confidence::Ambiguous,
            "too little content to classify structurally",
        );
    }

    // Cadastral identifiers only appear on drawings that sit in the cadastre's
    // own coordinate world, which is exactly the mappable set.
    if f.cadastral_codes >= 2 {
        return verdict(
            PageClass::CadastralPlan,
            Confidence::Ambiguous,
            "several cadastral parcel codes alongside substantial line work",
        );
    }

    verdict(
        PageClass::Unknown,
        Confidence::Ambiguous,
        "a drawing, but its type cannot be told from structure alone",
    )
}
