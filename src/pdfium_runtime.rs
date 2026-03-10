use crate::error::{PdfCutterError, Result};
use pdfium_render::prelude::*;
use std::path::{Path, PathBuf};

/// Binds to a usable pdfium library and returns a reusable runtime handle.
pub fn create_pdfium() -> Result<Pdfium> {
    if let Some(candidate) = executable_directory_candidate()
        && let Some(pdfium) = bind_to_library_path(&candidate)
    {
        return Ok(pdfium);
    }

    if let Ok(bindings) = Pdfium::bind_to_system_library() {
        return Ok(Pdfium::new(bindings));
    }

    for candidate in fallback_library_paths() {
        if let Some(pdfium) = bind_to_library_path(&candidate) {
            return Ok(pdfium);
        }
    }

    Err(PdfCutterError::PdfiumBinding)
}

fn executable_directory_candidate() -> Option<PathBuf> {
    std::env::current_exe().ok().and_then(|path| {
        path.parent()
            .map(|directory| directory.join(Pdfium::pdfium_platform_library_name()))
    })
}

fn bind_to_library_path(path: &Path) -> Option<Pdfium> {
    Pdfium::bind_to_library(path.to_string_lossy().as_ref())
        .ok()
        .map(Pdfium::new)
}

fn fallback_library_paths() -> [PathBuf; 3] {
    [
        Pdfium::pdfium_platform_library_name_at_path("./"),
        Pdfium::pdfium_platform_library_name_at_path("./dist/"),
        Pdfium::pdfium_platform_library_name_at_path("./bin/"),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fallback_library_paths_include_dist() {
        let paths = fallback_library_paths();

        assert_eq!(paths[0], Pdfium::pdfium_platform_library_name_at_path("./"));
        assert_eq!(
            paths[1],
            Pdfium::pdfium_platform_library_name_at_path("./dist/")
        );
        assert_eq!(
            paths[2],
            Pdfium::pdfium_platform_library_name_at_path("./bin/")
        );
    }
}
