use crate::error::{PdfError, Result};
use pdfium_render::prelude::*;
use std::path::{Path, PathBuf};

/// Binds to a usable pdfium library and returns a reusable runtime handle.
///
/// The search order is the executable's own directory, the system library,
/// then `./` and `./dist/`. A caller that already knows where the library is
/// should say so with [`create_pdfium_at`] rather than arranging for one of
/// these to match.
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

    Err(PdfError::PdfiumBinding)
}

/// Binds to the pdfium library at a known location.
///
/// `location` may be the library file itself or the directory holding it.
///
/// This exists so that a caller which knows the layout does not have to
/// reproduce the search above to be found by it. The workspace's own tests are
/// the case in point: cargo runs an integration test with the working
/// directory set to the *package* root, so the relative `./dist/` arm cannot
/// see the library that `build.sh` puts at the workspace root, and without
/// this they would rebuild the platform-specific path themselves — leaving two
/// places that decide where pdfium lives.
pub fn create_pdfium_at(location: &Path) -> Result<Pdfium> {
    let library = if location.is_dir() {
        location.join(Pdfium::pdfium_platform_library_name())
    } else {
        location.to_path_buf()
    };

    bind_to_library_path(&library).ok_or(PdfError::PdfiumBinding)
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

fn fallback_library_paths() -> [PathBuf; 2] {
    [
        Pdfium::pdfium_platform_library_name_at_path("./"),
        Pdfium::pdfium_platform_library_name_at_path("./dist/"),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_location_that_holds_no_library_is_refused_rather_than_searched_for() {
        let dir = tempfile::tempdir().expect("temp dir");

        assert!(
            matches!(create_pdfium_at(dir.path()), Err(PdfError::PdfiumBinding)),
            "an explicit location must not silently fall back to the search"
        );
        assert!(matches!(
            create_pdfium_at(&dir.path().join("libpdfium.dylib")),
            Err(PdfError::PdfiumBinding)
        ));
    }
}
