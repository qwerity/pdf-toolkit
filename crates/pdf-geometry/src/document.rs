//! Document-level entry point: opens a PDF and extracts page content.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use lopdf::{Document as LoDocument, ObjectId};

use super::interpreter::{Interpreter, Limits};
use crate::model::PageContent;

/// An opened PDF plus its page index.
pub struct PdfDocument {
    inner: LoDocument,
    pages: Vec<ObjectId>,
    path: PathBuf,
}

impl PdfDocument {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let inner = LoDocument::load(&path)
            .with_context(|| format!("failed to parse {}", path.display()))?;
        let pages = inner.get_pages().into_values().collect();
        Ok(Self { inner, pages, path })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Stem of the source file, used as the document identifier in outputs.
    pub fn id(&self) -> String {
        self.path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "document".to_string())
    }

    pub fn page_count(&self) -> usize {
        self.pages.len()
    }

    /// Extracts geometry and text from a 1-based page number.
    pub fn page_content(&self, page_number: usize) -> Result<PageContent> {
        self.page_content_with(page_number, Limits::default())
    }

    pub fn page_content_with(&self, page_number: usize, limits: Limits) -> Result<PageContent> {
        let id = *self
            .pages
            .get(page_number.wrapping_sub(1))
            .with_context(|| format!("page {page_number} out of range"))?;
        Interpreter::new(&self.inner, limits)
            .run_page(id, page_number)
            .with_context(|| {
                format!(
                    "failed to interpret page {page_number} of {}",
                    self.path.display()
                )
            })
    }

    /// Extracts every page, in order.
    pub fn all_pages(&self) -> Vec<Result<PageContent>> {
        (1..=self.page_count())
            .map(|n| self.page_content(n))
            .collect()
    }
}
