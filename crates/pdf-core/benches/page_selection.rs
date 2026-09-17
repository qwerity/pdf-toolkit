//! Run with `cargo bench -p pdf-core --bench page_selection`.
use pdf_core::page_selection::PageSelection;
use std::{hint::black_box, time::Instant};

fn main() {
    let repeated = vec!["1-65535"; 256].join(",");
    for (name, input) in [
        ("typical selection", "1,3,5-7"),
        ("largest range", "1-65535"),
        ("repeated ranges rejected", repeated.as_str()),
    ] {
        let iterations = 10_000;
        let start = Instant::now();
        for _ in 0..iterations {
            let result = black_box(PageSelection::parse(black_box(input)));
            assert_eq!(result.is_err(), name == "repeated ranges rejected");
        }
        println!(
            "{name}: {} ns/parse",
            start.elapsed().as_nanos() / iterations
        );
    }
}
