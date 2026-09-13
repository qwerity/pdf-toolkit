//! Page classification: deciding which pages carry a CAD drawing that could be
//! placed on a map, and which are forms, scans, floor plans or elevations.

pub mod features;
pub mod heuristic;
pub mod sheet;

pub use features::PageFeatures;
pub use heuristic::{Confidence, PageClass, PageVerdict, heuristic_label};
pub use sheet::{SheetRole, SheetTitle};
