//! Inventory of one page's fills and text sizes — what to look at when a
//! drawing set comes from another office and `model`'s colour detection
//! needs checking: which fill colours the page uses, how many fills of
//! each, and the font sizes its text runs are set in.
//!
//! ```sh
//! cargo run -p pdf-geometry --example page_fills -- input.pdf 13
//! ```

use std::collections::BTreeMap;

use pdf_geometry::{PaintMode, PdfDocument};

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let (Some(path), Some(page)) = (args.get(1), args.get(2)) else {
        anyhow::bail!("usage: page_fills <input.pdf> <page>");
    };
    let document = PdfDocument::open(path)?;
    let content = document.page_content(page.parse()?)?;
    println!(
        "media box {:?}, {} paths, {} text runs",
        content.media_box,
        content.paths.len(),
        content.texts.len()
    );

    let mut fills: BTreeMap<String, (usize, f64)> = BTreeMap::new();
    for path in &content.paths {
        if !matches!(path.mode, PaintMode::Fill | PaintMode::FillStroke) {
            continue;
        }
        let key = format!("{:.3},{:.3},{:.3}", path.fill.r, path.fill.g, path.fill.b);
        let entry = fills.entry(key).or_default();
        for subpath in &path.subpaths {
            entry.0 += 1;
            entry.1 = entry.1.max(subpath.signed_area().abs());
        }
    }
    for (colour, (count, largest)) in fills {
        println!("fill {colour}: {count} subpaths, largest {largest:.1} pt²");
    }

    let mut sizes: BTreeMap<String, usize> = BTreeMap::new();
    for text in &content.texts {
        *sizes.entry(format!("{:.1}", text.size)).or_default() += 1;
    }
    println!("text sizes: {sizes:?}");
    Ok(())
}
