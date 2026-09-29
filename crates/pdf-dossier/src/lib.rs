//! Whole-dossier annotation: what every page is, what it says, and how the
//! pages interconnect.
//!
//! Building-permit dossiers arrive as one PDF holding many documents — an
//! application form, ownership certificates, identity scans, handwritten
//! consents, a drawings album. [`build_dossier`] turns one such PDF into a
//! single annotation (`docs/annotation-schema.md`): per-page labels and text
//! routing, decoded QR payloads, entities, groups, a derived graph, and the
//! CAD/georeference results from `pdf-vector`.
//!
//! The build is deterministic and offline. What a binary cannot read — scans,
//! Armenian handwriting, mis-encoded text layers — is *routed*, not guessed:
//! the page carries a `vlm` work order naming why it needs a vision pass. A
//! second run with `--answers` merges what that pass returned; an answered
//! page's semantic fields are taken from the answer wholesale, because the
//! answer came from something that looked at the page.

pub mod access;
pub mod answers;
mod build;
pub mod checks;
pub mod entities;
pub mod facts;
pub mod indicators;
pub mod legacy;
pub mod numbers;
pub mod qr;
pub mod report;
pub mod request;
pub mod text_signals;
pub mod trust;

pub use answers::Answers;
pub use build::{
    DossierAnalysis, DossierOptions, analyze, assemble, build_dossier, pages_awaiting_vision,
    seed_vision_reasons,
};
pub use report::DossierReport;
