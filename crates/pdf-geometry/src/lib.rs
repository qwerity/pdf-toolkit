//! The vector geometry a CAD exporter actually wrote.
//!
//! A content-stream interpreter walks every page, recursing through form
//! XObjects, tracking the CTM, colour space, line width and optional-content
//! (CAD layer) state, and flattening Bézier curves. What comes out is the real
//! geometry — not a trace of a rendering — plus text runs with their positions,
//! decoded through each font's `ToUnicode` CMap.
//!
//! This is the half of PDF reading that PDFium cannot give you: `pdf-core`
//! binds a renderer and a text layer, and neither exposes the path a drawing is
//! made of. The two parsers coexist deliberately, each doing what the other
//! cannot.

pub mod cmap;
pub mod document;
pub mod interpreter;
pub mod model;
pub mod textrun;

pub use document::PdfDocument;
pub use interpreter::{Interpreter, Limits, matrix_from, page_resources};
pub use model::{BBox, Matrix, PageContent, PaintMode, Point, Rgb, SubPath, TextItem, VectorPath};
