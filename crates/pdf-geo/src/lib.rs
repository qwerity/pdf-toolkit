pub mod control;
pub mod crs;
pub mod dimensions;
pub mod fit;
pub mod geojson;
pub mod labels;
pub mod local;
pub mod pipeline;
pub mod topology;

pub use control::{ControlRow, ControlTable, TableSource};
pub use crs::{LonLat, Projected, ProjectedCrs};
pub use fit::{CandidateSource, Correspondence, Fit, FitConstraints, Similarity};
pub use geojson::{ExportOptions, ExportSummary, Georeference};
pub use pipeline::{
    GeoreferenceFailure, GeoreferenceResult, georeference_from_text_layer, georeference_page,
};
