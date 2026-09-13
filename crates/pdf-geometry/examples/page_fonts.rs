//! The fonts one page uses and how its text is encoded — what to look at
//! when the interpreter recovers no text from a sheet: whether the fonts
//! carry a `ToUnicode` map, and what the raw `Tj` operands look like.
//!
//! ```sh
//! cargo run -p pdf-geometry --example page_fonts -- input.pdf 13
//! ```

use lopdf::{Document, Object};

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let (Some(path), Some(page)) = (args.get(1), args.get(2)) else {
        anyhow::bail!("usage: page_fonts <input.pdf> <page>");
    };
    let document = Document::load(path)?;
    let page: u32 = page.parse()?;
    let id = *document
        .get_pages()
        .get(&page)
        .ok_or_else(|| anyhow::anyhow!("page {page} out of range"))?;

    let describe = |font: &lopdf::Dictionary, key: &[u8]| -> String {
        font.get(key)
            .map(|o| format!("{o:?}").chars().take(60).collect())
            .unwrap_or_else(|_| "-".into())
    };
    for (name, font) in document.get_page_fonts(id)? {
        println!(
            "{}: Subtype {} Encoding {} ToUnicode {} DescendantFonts {}",
            String::from_utf8_lossy(&name),
            describe(font, b"Subtype"),
            describe(font, b"Encoding"),
            describe(font, b"ToUnicode"),
            describe(font, b"DescendantFonts"),
        );
    }

    let content = lopdf::content::Content::decode(&document.get_page_content(id))?;
    let shown = content
        .operations
        .iter()
        .filter(|op| matches!(op.operator.as_str(), "Tj" | "TJ" | "Tf"))
        .take(12);
    for op in shown {
        let operands: Vec<String> = op
            .operands
            .iter()
            .map(|o| match o {
                Object::String(bytes, _) => format!("<{}>", hex(bytes)),
                other => format!("{other:?}"),
            })
            .collect();
        println!("  {} {}", op.operator, operands.join(" "));
    }
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .take(40)
        .map(|b| format!("{b:02x}"))
        .collect::<String>()
}
