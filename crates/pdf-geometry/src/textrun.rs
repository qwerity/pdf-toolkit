//! Reassembly of glyph runs into readable words.
//!
//! CAD exporters emit a separate `Tj` for nearly every glyph cluster, so a
//! coordinate such as `8465209.0514` arrives as two runs. Anything that matches
//! on text — the control-point table, the declared scale, keyword-based page
//! classification — needs those runs merged back into words first.

use crate::model::TextItem;

/// Thresholds for deciding that two runs belong to the same word, expressed as
/// multiples of the font size.
#[derive(Debug, Clone, Copy)]
pub struct MergeOptions {
    /// Maximum gap between the end of one run and the start of the next.
    pub max_gap: f64,
    /// Gap above which a space is inserted at the join.
    pub space_gap: f64,
    /// Maximum perpendicular offset between baselines.
    pub max_baseline_offset: f64,
    /// Maximum ratio between the two font sizes, either way round.
    pub max_size_ratio: f64,
    /// Maximum baseline angle difference, in radians.
    pub max_angle_delta: f64,
}

impl Default for MergeOptions {
    fn default() -> Self {
        Self {
            max_gap: 0.55,
            space_gap: 0.22,
            max_baseline_offset: 0.35,
            max_size_ratio: 1.4,
            max_angle_delta: 0.02,
        }
    }
}

/// Merges runs that sit on a common baseline into single items, preserving the
/// leftmost origin, the maximum size, and the shared angle.
pub fn merge_runs(items: Vec<TextItem>, options: MergeOptions) -> Vec<TextItem> {
    if items.len() < 2 {
        return items;
    }
    let mut indexed: Vec<TextItem> = items;
    indexed.sort_by(|a, b| {
        let ka = sort_key(a);
        let kb = sort_key(b);
        ka.partial_cmp(&kb).unwrap_or(std::cmp::Ordering::Equal)
    });

    let mut out: Vec<TextItem> = Vec::with_capacity(indexed.len());
    for item in indexed {
        let Some(previous) = out.last_mut() else {
            out.push(item);
            continue;
        };
        match join_gap(previous, &item, options) {
            Some(gap) => {
                if gap > options.space_gap * previous.size {
                    previous.text.push(' ');
                }
                previous.text.push_str(&item.text);
                previous.size = previous.size.max(item.size);
            }
            None => out.push(item),
        }
    }
    out
}

/// Orders runs so that same-line neighbours are adjacent: by angle bucket,
/// then down the page, then along the baseline.
fn sort_key(t: &TextItem) -> (i64, i64, i64) {
    let (cos, sin) = (t.angle.cos(), t.angle.sin());
    let across = -t.origin.x * sin + t.origin.y * cos;
    let along = t.origin.x * cos + t.origin.y * sin;
    (
        (t.angle * 100.0).round() as i64,
        -(across * 10.0).round() as i64,
        (along * 100.0).round() as i64,
    )
}

/// Returns the gap, in user-space units, if `next` continues `previous`.
fn join_gap(previous: &TextItem, next: &TextItem, options: MergeOptions) -> Option<f64> {
    if (previous.angle - next.angle).abs() > options.max_angle_delta {
        return None;
    }
    let (small, large) = if previous.size < next.size {
        (previous.size, next.size)
    } else {
        (next.size, previous.size)
    };
    if small <= 0.0 || large / small > options.max_size_ratio {
        return None;
    }

    let (cos, sin) = (previous.angle.cos(), previous.angle.sin());
    let across = |p: crate::model::Point| -p.x * sin + p.y * cos;
    let along = |p: crate::model::Point| p.x * cos + p.y * sin;
    if (across(next.origin) - across(previous.origin)).abs()
        > options.max_baseline_offset * previous.size
    {
        return None;
    }

    // The interpreter advances by an average glyph width, which is accurate
    // enough for the short runs CAD exporters produce.
    let estimated_end =
        along(previous.origin) + previous.text.chars().count() as f64 * 0.5 * previous.size;
    let gap = along(next.origin) - estimated_end;
    (gap > -0.5 * previous.size && gap < options.max_gap * previous.size).then_some(gap.max(0.0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Point, Rgb};

    fn run(text: &str, x: f64, y: f64) -> TextItem {
        TextItem {
            text: text.to_string(),
            origin: Point::new(x, y),
            size: 5.7,
            angle: 0.0,
            color: Rgb::BLACK,
        }
    }

    #[test]
    fn joins_a_split_coordinate() {
        let merged = merge_runs(
            vec![run("8465209.", 302.9, 192.2), run("0514", 325.1, 192.2)],
            MergeOptions::default(),
        );
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].text, "8465209.0514");
    }

    #[test]
    fn keeps_separate_table_cells_apart() {
        let merged = merge_runs(
            vec![run("0514", 325.1, 192.2), run("4423117.", 342.5, 192.2)],
            MergeOptions::default(),
        );
        assert_eq!(merged.len(), 2);
    }

    #[test]
    fn keeps_the_row_number_apart_from_the_first_cell() {
        let merged = merge_runs(
            vec![run("1", 287.9, 192.2), run("8465209.", 302.9, 192.2)],
            MergeOptions::default(),
        );
        assert_eq!(merged.len(), 2);
    }

    #[test]
    fn does_not_join_across_lines() {
        let merged = merge_runs(
            vec![run("8465209.", 302.9, 192.2), run("0514", 325.1, 175.0)],
            MergeOptions::default(),
        );
        assert_eq!(merged.len(), 2);
    }

    #[test]
    fn inserts_a_space_for_a_word_gap() {
        let merged = merge_runs(
            vec![run("ab", 100.0, 50.0), run("cd", 108.5, 50.0)],
            MergeOptions::default(),
        );
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].text, "ab cd");
    }
}
