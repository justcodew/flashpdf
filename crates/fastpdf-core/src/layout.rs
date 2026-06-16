/// Layout analysis: cluster chars → spans → lines → blocks.
///
/// Uses geometric proximity and font/size/color matching to group characters
/// into semantically meaningful text structures compatible with PyMuPDF output.
use crate::parser::content_stream::{CharInfo, TextBlock, TextLine, TextSpan};
use smallvec::SmallVec;

// ─── Layout parameters (tunable) ───

/// Maximum horizontal gap between chars in the same span (as fraction of font size).
const SPAN_GAP_FACTOR: f64 = 0.3;
/// Maximum vertical distance between spans in the same line (as fraction of font size).
const LINE_VERT_FACTOR: f64 = 0.5;
/// Minimum vertical gap between lines to start a new block (as fraction of font size).
const BLOCK_GAP_FACTOR: f64 = 1.5;
/// Minimum gap between columns as fraction of page width.
const COLUMN_GAP_FACTOR: f64 = 0.05;

/// Cluster extracted characters into spans, lines, and blocks.
///
/// Input: flat list of CharInfo from content stream scanning.
/// Output: hierarchical TextBlock → TextLine → TextSpan structure.
pub fn cluster_chars(chars: &[CharInfo], font: &str, font_size: f64, color: u32) -> Vec<TextBlock> {
    if chars.is_empty() {
        return Vec::new();
    }

    // Step 0: Detect columns and split characters
    let columns = detect_columns(chars);

    if columns.len() <= 1 {
        // Single column: process normally
        let spans = build_spans(chars, font, font_size, color);
        let lines = build_lines(spans, font_size);
        return build_blocks(lines, font_size);
    }

    // Multi-column: process each column independently, then merge
    let mut all_blocks = Vec::new();
    for col_chars in columns {
        let spans = build_spans(&col_chars, font, font_size, color);
        let lines = build_lines(spans, font_size);
        all_blocks.extend(build_blocks(lines, font_size));
    }
    all_blocks
}

/// Detect column boundaries by analyzing X coordinate distribution.
/// Returns characters split into columns (left to right).
fn detect_columns(chars: &[CharInfo]) -> Vec<Vec<CharInfo>> {
    if chars.len() < 20 {
        return vec![chars.to_vec()];
    }

    // Use median-based bounds to filter outliers (formulas, rotated text, etc.)
    let mut x_positions: Vec<f64> = chars.iter().map(|c| c.bbox[0]).collect();
    x_positions.sort_by(|a, b| a.partial_cmp(b).unwrap());

    // Use 10th and 90th percentiles as bounds (more aggressive filtering)
    let p10_idx = (x_positions.len() as f64 * 0.10) as usize;
    let p90_idx = (x_positions.len() as f64 * 0.90) as usize;
    let x_min = x_positions[p10_idx];
    let x_max = x_positions[p90_idx];
    let page_width = x_max - x_min;

    if page_width < 50.0 {
        return vec![chars.to_vec()];
    }

    // Build histogram of X positions (left edge of each char)
    let num_bins = 200; // More bins for finer resolution
    let bin_width = page_width / num_bins as f64;
    let mut histogram = vec![0u32; num_bins];

    for c in chars {
        let x = c.bbox[0];
        if x < x_min || x > x_max {
            continue; // Skip outliers
        }
        let bin = ((x - x_min) / bin_width) as usize;
        let bin = bin.min(num_bins - 1);
        histogram[bin] += 1;
    }

    // Smooth histogram with larger window
    let mut smoothed = vec![0u32; num_bins];
    for i in 2..num_bins - 2 {
        smoothed[i] = histogram[i - 2] / 8
            + histogram[i - 1] / 4
            + histogram[i] / 4
            + histogram[i + 1] / 4
            + histogram[i + 2] / 8;
    }
    smoothed[0] = histogram[0];
    smoothed[1] = histogram[1];
    smoothed[num_bins - 2] = histogram[num_bins - 2];
    smoothed[num_bins - 1] = histogram[num_bins - 1];

    // Find column boundaries: look for bins with very few characters
    let avg = smoothed.iter().sum::<u32>() as f64 / num_bins as f64;
    let threshold = (avg * 0.2) as u32; // 20% of average = "empty" bin
    let min_gap_bins = 3; // Minimum gap width in bins

    let mut gaps = Vec::new();
    let mut gap_start = None;

    for i in 0..num_bins {
        if smoothed[i] <= threshold {
            if gap_start.is_none() {
                gap_start = Some(i);
            }
        } else {
            if let Some(start) = gap_start {
                if i - start >= min_gap_bins {
                    // Found a significant gap
                    let gap_center = (start + i) / 2;
                    gaps.push(x_min + gap_center as f64 * bin_width);
                }
                gap_start = None;
            }
        }
    }

    if gaps.is_empty() {
        return vec![chars.to_vec()];
    }

    // Split characters at column boundaries
    let mut columns: Vec<Vec<CharInfo>> = vec![Vec::new(); gaps.len() + 1];

    for c in chars {
        let x = c.bbox[0];
        let mut col = 0;
        for (i, &boundary) in gaps.iter().enumerate() {
            if x > boundary {
                col = i + 1;
            }
        }
        columns[col].push(c.clone());
    }

    // Filter out empty columns
    let result: Vec<Vec<CharInfo>> = columns.into_iter().filter(|col| !col.is_empty()).collect();

    result
}

/// Build spans from consecutive characters with similar position.
fn build_spans(chars: &[CharInfo], font: &str, font_size: f64, color: u32) -> Vec<TextSpan> {
    if chars.is_empty() {
        return Vec::new();
    }

    let mut spans = Vec::new();
    let mut current_chars: SmallVec<[CharInfo; 16]> = SmallVec::new();
    current_chars.push(chars[0].clone());
    let max_gap = font_size * SPAN_GAP_FACTOR;

    for i in 1..chars.len() {
        let prev = &chars[i - 1];
        let curr = &chars[i];

        // Same line check: vertical distance within threshold
        let vert_dist = (curr.bbox[1] - prev.bbox[1]).abs();
        let horiz_gap = curr.bbox[0] - prev.bbox[2]; // gap between prev right and curr left

        if vert_dist < font_size * LINE_VERT_FACTOR && horiz_gap < max_gap && horiz_gap > -font_size * 0.5 {
            // Same span
            current_chars.push(curr.clone());
        } else {
            // Flush current span and start new one
            spans.push(make_span(current_chars, font, font_size, color));
            current_chars = SmallVec::new();
            current_chars.push(curr.clone());
        }
    }

    spans.push(make_span(current_chars, font, font_size, color));
    spans
}

fn make_span(chars: SmallVec<[CharInfo; 16]>, font: &str, font_size: f64, color: u32) -> TextSpan {
    let text: String = chars.iter().map(|c| c.c).collect();
    let bbox = compute_bbox(&chars);
    TextSpan {
        text,
        font: font.to_string(),
        size: font_size,
        color,
        bbox,
        chars: chars.into_vec(),
    }
}

/// Build lines from spans with similar vertical position.
/// Also splits lines when there's a large horizontal gap (column boundary).
fn build_lines(spans: Vec<TextSpan>, font_size: f64) -> Vec<TextLine> {
    if spans.is_empty() {
        return Vec::new();
    }

    // Sort spans by vertical position (y), then by horizontal position (x)
    let mut sorted = spans;
    sorted.sort_by(|a, b| {
        let ya = a.bbox[1];
        let yb = b.bbox[1];
        ya.partial_cmp(&yb)
            .unwrap()
            .then_with(|| a.bbox[0].partial_cmp(&b.bbox[0]).unwrap())
    });

    let mut lines: Vec<TextLine> = Vec::new();
    let mut current_spans: Vec<TextSpan> = vec![sorted[0].clone()];
    let line_threshold = font_size * LINE_VERT_FACTOR;
    // Column gap threshold: if span starts this far left of previous span end, it's a new column
    let col_gap_threshold = -font_size * 10.0; // Negative = span starts left of previous end

    for i in 1..sorted.len() {
        let prev_y = current_spans.last().unwrap().bbox[1];
        let curr_y = sorted[i].bbox[1];
        let prev_x2 = current_spans.last().unwrap().bbox[2]; // right edge of last span
        let curr_x = sorted[i].bbox[0]; // left edge of current span
        let h_gap = curr_x - prev_x2; // negative = overlap/backward jump

        let same_line = (curr_y - prev_y).abs() < line_threshold && h_gap > col_gap_threshold;

        if same_line {
            current_spans.push(sorted[i].clone());
        } else {
            lines.push(make_line(current_spans));
            current_spans = vec![sorted[i].clone()];
        }
    }

    lines.push(make_line(current_spans));
    lines
}

fn make_line(spans: Vec<TextSpan>) -> TextLine {
    // Sort spans within line by x position
    let mut sorted = spans;
    sorted.sort_by(|a, b| a.bbox[0].partial_cmp(&b.bbox[0]).unwrap());
    let bbox = compute_bbox_from_spans(&sorted);
    TextLine { bbox, spans: sorted }
}

/// Build blocks from lines with large vertical gaps.
fn build_blocks(lines: Vec<TextLine>, font_size: f64) -> Vec<TextBlock> {
    if lines.is_empty() {
        return Vec::new();
    }

    let block_threshold = font_size * BLOCK_GAP_FACTOR;
    let mut blocks: Vec<TextBlock> = Vec::new();
    let mut current_lines: Vec<TextLine> = vec![lines[0].clone()];

    for i in 1..lines.len() {
        let prev_bottom = current_lines.last().unwrap().bbox[3];
        let curr_top = lines[i].bbox[1];
        let gap = curr_top - prev_bottom;

        if gap > block_threshold {
            blocks.push(make_block(current_lines));
            current_lines = vec![lines[i].clone()];
        } else {
            current_lines.push(lines[i].clone());
        }
    }

    blocks.push(make_block(current_lines));
    blocks
}

fn make_block(lines: Vec<TextLine>) -> TextBlock {
    let bbox = compute_bbox_from_lines(&lines);
    TextBlock { bbox, lines }
}

// ─── BBox helpers ───

fn compute_bbox(chars: &[CharInfo]) -> [f64; 4] {
    if chars.is_empty() {
        return [0.0, 0.0, 0.0, 0.0];
    }
    let mut x0 = f64::MAX;
    let mut y0 = f64::MAX;
    let mut x1 = f64::MIN;
    let mut y1 = f64::MIN;
    for c in chars {
        x0 = x0.min(c.bbox[0]);
        y0 = y0.min(c.bbox[1]);
        x1 = x1.max(c.bbox[2]);
        y1 = y1.max(c.bbox[3]);
    }
    [x0, y0, x1, y1]
}

fn compute_bbox_from_spans(spans: &[TextSpan]) -> [f64; 4] {
    if spans.is_empty() {
        return [0.0, 0.0, 0.0, 0.0];
    }
    let mut x0 = f64::MAX;
    let mut y0 = f64::MAX;
    let mut x1 = f64::MIN;
    let mut y1 = f64::MIN;
    for s in spans {
        x0 = x0.min(s.bbox[0]);
        y0 = y0.min(s.bbox[1]);
        x1 = x1.max(s.bbox[2]);
        y1 = y1.max(s.bbox[3]);
    }
    [x0, y0, x1, y1]
}

fn compute_bbox_from_lines(lines: &[TextLine]) -> [f64; 4] {
    if lines.is_empty() {
        return [0.0, 0.0, 0.0, 0.0];
    }
    let mut x0 = f64::MAX;
    let mut y0 = f64::MAX;
    let mut x1 = f64::MIN;
    let mut y1 = f64::MIN;
    for l in lines {
        x0 = x0.min(l.bbox[0]);
        y0 = y0.min(l.bbox[1]);
        x1 = x1.max(l.bbox[2]);
        y1 = y1.max(l.bbox[3]);
    }
    [x0, y0, x1, y1]
}

// ─── Tests ───

#[cfg(test)]
mod tests {
    use super::*;

    fn make_char(c: char, x: f64, y: f64, w: f64, h: f64) -> CharInfo {
        CharInfo { c, bbox: [x, y, x + w, y + h] }
    }

    #[test]
    fn test_single_span() {
        let chars = vec![
            make_char('H', 100.0, 700.0, 7.0, 12.0),
            make_char('i', 107.0, 700.0, 5.0, 12.0),
        ];
        let blocks = cluster_chars(&chars, "Helvetica", 12.0, 0);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].lines.len(), 1);
        assert_eq!(blocks[0].lines[0].spans.len(), 1);
        assert_eq!(blocks[0].lines[0].spans[0].text, "Hi");
    }

    #[test]
    fn test_two_lines() {
        let chars = vec![
            make_char('A', 100.0, 700.0, 7.0, 12.0),
            make_char('B', 100.0, 680.0, 7.0, 12.0),
        ];
        let blocks = cluster_chars(&chars, "Helvetica", 12.0, 0);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].lines.len(), 2);
    }

    #[test]
    fn test_two_blocks() {
        let chars = vec![
            make_char('A', 100.0, 700.0, 7.0, 12.0),
            // Large vertical gap → new block
            make_char('B', 100.0, 650.0, 7.0, 12.0),
        ];
        let blocks = cluster_chars(&chars, "Helvetica", 12.0, 0);
        assert_eq!(blocks.len(), 2);
    }

    #[test]
    fn test_empty_input() {
        let chars = vec![];
        let blocks = cluster_chars(&chars, "Helvetica", 12.0, 0);
        assert!(blocks.is_empty());
    }

    #[test]
    fn test_span_gap_break() {
        // Two words with a large gap → two spans
        let chars = vec![
            make_char('H', 100.0, 700.0, 7.0, 12.0),
            make_char('i', 107.0, 700.0, 5.0, 12.0),
            // gap
            make_char('W', 150.0, 700.0, 8.0, 12.0),
            make_char('o', 158.0, 700.0, 6.0, 12.0),
        ];
        let blocks = cluster_chars(&chars, "Helvetica", 12.0, 0);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].lines.len(), 1);
        assert_eq!(blocks[0].lines[0].spans.len(), 2);
        assert_eq!(blocks[0].lines[0].spans[0].text, "Hi");
        assert_eq!(blocks[0].lines[0].spans[1].text, "Wo");
    }

    #[test]
    fn test_two_columns() {
        // Two-column layout: interleaved characters
        // Need enough chars (>=20) for detect_columns to trigger
        let mut chars = Vec::new();
        // Left column at x=50, right column at x=300
        for i in 0..10 {
            let y = 700.0 - i as f64 * 20.0;
            // Left column: "LeftX"
            chars.push(make_char('L', 50.0, y, 7.0, 12.0));
            chars.push(make_char('e', 57.0, y, 5.0, 12.0));
            chars.push(make_char('f', 62.0, y, 5.0, 12.0));
            chars.push(make_char('t', 67.0, y, 5.0, 12.0));
            // Right column: "RightX"
            chars.push(make_char('R', 300.0, y, 7.0, 12.0));
            chars.push(make_char('i', 307.0, y, 5.0, 12.0));
            chars.push(make_char('g', 312.0, y, 5.0, 12.0));
            chars.push(make_char('h', 317.0, y, 5.0, 12.0));
            chars.push(make_char('t', 322.0, y, 5.0, 12.0));
        }
        let blocks = cluster_chars(&chars, "Helvetica", 12.0, 0);
        // Collect all span text
        let mut all_text = String::new();
        for block in &blocks {
            for line in &block.lines {
                for span in &line.spans {
                    all_text.push_str(&span.text);
                    all_text.push(' ');
                }
            }
        }
        // Left and right column text should not be interleaved
        assert!(all_text.contains("Left"), "Missing left column text: {}", all_text);
        assert!(all_text.contains("Right"), "Missing right column text: {}", all_text);
    }
}
