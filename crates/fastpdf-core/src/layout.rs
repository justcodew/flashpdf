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
/// Cluster extracted characters into spans, lines, and blocks.
///
/// Input: flat list of CharInfo from content stream scanning.
/// Output: hierarchical TextBlock → TextLine → TextSpan structure.
pub fn cluster_chars(chars: &[CharInfo], font: &str, font_size: f64, color: u32) -> Vec<TextBlock> {
    if chars.is_empty() {
        return Vec::new();
    }

    // Step 0: Build spans first (before column detection)
    let spans = build_spans(chars, font, font_size, color);

    // Step 1: Detect columns at the span level
    let columns = detect_columns_from_spans(&spans);

    let blocks = if columns.len() <= 1 {
        // Single column: process normally
        let lines = build_lines(spans, font_size);
        build_blocks(lines, font_size)
    } else {
        // Multi-column: process each column independently, then merge
        let mut all_blocks = Vec::new();
        for col_spans in columns {
            let lines = build_lines(col_spans, font_size);
            all_blocks.extend(build_blocks(lines, font_size));
        }
        all_blocks
    };

    // Step 2: Merge hyphenated words across lines
    merge_hyphenated_lines(blocks)
}

/// Detect column boundaries at the span level.
/// Uses a smoothed density histogram of span left-edge X positions.
/// Finds the two highest peaks and splits at the valley between them.
/// This handles cases where formula content fills the gap between columns.
fn detect_columns_from_spans(spans: &[TextSpan]) -> Vec<Vec<TextSpan>> {
    if spans.len() < 6 {
        return vec![spans.to_vec()];
    }

    let x_positions: Vec<f64> = spans.iter().map(|s| s.bbox[0]).collect();

    let x_min = x_positions.iter().cloned().fold(f64::MAX, f64::min);
    let x_max = x_positions.iter().cloned().fold(f64::MIN, f64::max);
    let x_range = x_max - x_min;

    if x_range < 100.0 {
        return vec![spans.to_vec()];
    }

    // Build histogram with ~15px bins
    let num_bins = ((x_range / 15.0) as usize).max(20).min(200);
    let bin_width = x_range / num_bins as f64;
    let mut histogram = vec![0u32; num_bins];

    for &x in &x_positions {
        let bin = ((x - x_min) / bin_width) as usize;
        let bin = bin.min(num_bins - 1);
        histogram[bin] += 1;
    }

    // Smooth with radius 5 to reduce noise
    let smooth_radius = 5usize.min(num_bins / 4);
    let mut smoothed = vec![0.0f64; num_bins];
    for i in 0..num_bins {
        let lo = i.saturating_sub(smooth_radius);
        let hi = (i + smooth_radius + 1).min(num_bins);
        for j in lo..hi {
            smoothed[i] += histogram[j] as f64;
        }
        smoothed[i] /= (hi - lo) as f64;
    }

    // Find local maxima (bins higher than both neighbors)
    let mut peaks: Vec<(usize, f64)> = Vec::new();
    for i in 1..num_bins - 1 {
        if smoothed[i] > smoothed[i - 1] && smoothed[i] > smoothed[i + 1] {
            peaks.push((i, smoothed[i]));
        }
    }

    // Also consider edge bins as peaks if they're high
    if smoothed[0] > smoothed[1] {
        peaks.push((0, smoothed[0]));
    }
    if smoothed[num_bins - 1] > smoothed[num_bins - 2] {
        peaks.push((num_bins - 1, smoothed[num_bins - 1]));
    }

    // Sort peaks by height (descending)
    peaks.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());

    // Take the two highest peaks that are far enough apart (>= 10 bins)
    if peaks.len() < 2 {
        return vec![spans.to_vec()];
    }

    let mut peak1 = peaks[0].0;
    let mut peak2 = peaks[1].0;

    // If the top two peaks are too close, find the next best peak that's far enough
    if peak2.abs_diff(peak1) < 10 {
        for &(p, _v) in peaks.iter().skip(2) {
            if p.abs_diff(peak1) >= 10 {
                peak2 = p;
                break;
            }
        }
    }

    // Ensure peak1 < peak2
    if peak1 > peak2 {
        std::mem::swap(&mut peak1, &mut peak2);
    }

    // Peaks must be far enough apart (at least 10 bins ≈ 150px)
    if peak2.abs_diff(peak1) < 10 {
        return vec![spans.to_vec()];
    }

    // Find the valley (minimum density) between the two peaks
    let mut valley = peak1;
    let mut valley_density = smoothed[peak1];
    for i in peak1..=peak2 {
        if smoothed[i] < valley_density {
            valley_density = smoothed[i];
            valley = i;
        }
    }

    // Valley must be meaningfully lower than both peaks.
    // Use a relaxed threshold since formula content can fill the gap between columns.
    let min_peak = smoothed[peak1].min(smoothed[peak2]);
    if valley_density > min_peak * 0.85 {
        return vec![spans.to_vec()];
    }

    let boundary = x_min + (valley as f64 + 0.5) * bin_width;

    // Split spans at the boundary
    let mut left = Vec::new();
    let mut right = Vec::new();

    for s in spans {
        if s.bbox[0] < boundary {
            left.push(s.clone());
        } else {
            right.push(s.clone());
        }
    }

    // Both columns must have at least 2 spans
    if left.len() < 2 || right.len() < 2 {
        return vec![spans.to_vec()];
    }

    vec![left, right]
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

        if vert_dist < font_size * LINE_VERT_FACTOR
            && horiz_gap < max_gap
            && horiz_gap > -font_size * 0.5
        {
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
    // Column gap thresholds:
    // - Backward jump: span starts far left of previous span end (right→left column wrap)
    // - Forward jump: span starts far right of previous span end (left→right column)
    let col_gap_backward = -font_size * 10.0;
    let col_gap_forward = font_size * 5.0; // Large forward jump = column boundary

    for i in 1..sorted.len() {
        let prev_y = current_spans.last().unwrap().bbox[1];
        let curr_y = sorted[i].bbox[1];
        let prev_x2 = current_spans.last().unwrap().bbox[2]; // right edge of last span
        let curr_x = sorted[i].bbox[0]; // left edge of current span
        let h_gap = curr_x - prev_x2; // negative = overlap/backward jump

        let same_line = (curr_y - prev_y).abs() < line_threshold
            && h_gap > col_gap_backward
            && h_gap < col_gap_forward;

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
    TextLine {
        bbox,
        spans: sorted,
    }
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

/// Merge hyphenated words across lines.
/// If a line ends with '-' and the next line starts with a lowercase letter,
/// merge the two lines by removing the hyphen and combining the text.
/// Runs repeatedly until no more merges are possible (handles consecutive hyphens).
fn merge_hyphenated_lines(blocks: Vec<TextBlock>) -> Vec<TextBlock> {
    let mut result = Vec::with_capacity(blocks.len());

    for block in blocks {
        if block.lines.len() < 2 {
            result.push(block);
            continue;
        }

        // Run merge repeatedly until stable
        let mut lines = block.lines;
        loop {
            let (merged, count) = merge_hyphens_pass(lines);
            lines = merged;
            if count == 0 {
                break;
            }
        }

        let bbox = compute_bbox_from_lines(&lines);
        result.push(TextBlock { bbox, lines });
    }

    result
}

/// Single pass of hyphenation merge. Returns (merged lines, number of merges).
fn merge_hyphens_pass(lines: Vec<TextLine>) -> (Vec<TextLine>, usize) {
    let mut merged_lines: Vec<TextLine> = Vec::new();
    let mut merge_count = 0;
    let mut i = 0;

    while i < lines.len() {
        if i + 1 < lines.len() {
            let curr_text = line_text(&lines[i]);
            let next_text = line_text(&lines[i + 1]);

            // Check if current line ends with hyphen and next starts with lowercase
            if curr_text.ends_with('-') {
                let next_first = next_text.chars().next();
                if let Some(c) = next_first {
                    if c.is_ascii_lowercase() {
                        // Merge: remove hyphen from current, combine with next
                        let merged_line = merge_two_lines(&lines[i], &lines[i + 1]);
                        merged_lines.push(merged_line);
                        merge_count += 1;
                        i += 2;
                        continue;
                    }
                }
            }
        }

        merged_lines.push(lines[i].clone());
        i += 1;
    }

    (merged_lines, merge_count)
}

/// Get the text content of a line by concatenating its spans.
fn line_text(line: &TextLine) -> String {
    let mut text = String::new();
    for span in &line.spans {
        text.push_str(&span.text);
    }
    text
}

/// Merge two lines by removing the trailing hyphen from the first line
/// and combining all spans.
fn merge_two_lines(line1: &TextLine, line2: &TextLine) -> TextLine {
    let mut merged_spans: Vec<TextSpan> = Vec::new();

    // Process spans from line1, removing trailing hyphen from the last span
    for (i, span) in line1.spans.iter().enumerate() {
        if i == line1.spans.len() - 1 {
            // Last span in line1: remove trailing hyphen
            let mut text = span.text.clone();
            if text.ends_with('-') {
                text.pop();
            }
            merged_spans.push(TextSpan {
                text,
                font: span.font.clone(),
                size: span.size,
                color: span.color,
                bbox: span.bbox,
                chars: span.chars.clone(),
            });
        } else {
            merged_spans.push(span.clone());
        }
    }

    // Add all spans from line2
    merged_spans.extend(line2.spans.iter().cloned());

    let bbox = compute_bbox_from_spans(&merged_spans);
    TextLine {
        bbox,
        spans: merged_spans,
    }
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
        CharInfo {
            c,
            bbox: [x, y, x + w, y + h],
        }
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
        assert!(
            all_text.contains("Left"),
            "Missing left column text: {}",
            all_text
        );
        assert!(
            all_text.contains("Right"),
            "Missing right column text: {}",
            all_text
        );
    }

    #[test]
    fn test_hyphenation_merge() {
        // Two lines in same block, with hyphen at end of first line
        // Line 1 at y=500, Line 2 at y=515 (15px apart, > line_threshold=6, < block_threshold=18)
        let chars = vec![
            make_char('c', 100.0, 500.0, 7.0, 12.0),
            make_char('o', 107.0, 500.0, 6.0, 12.0),
            make_char('m', 113.0, 500.0, 8.0, 12.0),
            make_char('p', 121.0, 500.0, 6.0, 12.0),
            make_char('r', 127.0, 500.0, 5.0, 12.0),
            make_char('e', 132.0, 500.0, 5.0, 12.0),
            make_char('-', 137.0, 500.0, 5.0, 12.0),
            // Next line (y=515, diff=15 > line_threshold=6, < block_threshold=18)
            make_char('h', 100.0, 515.0, 6.0, 12.0),
            make_char('e', 106.0, 515.0, 5.0, 12.0),
            make_char('n', 111.0, 515.0, 6.0, 12.0),
            make_char('s', 117.0, 515.0, 5.0, 12.0),
            make_char('i', 122.0, 515.0, 4.0, 12.0),
            make_char('v', 126.0, 515.0, 6.0, 12.0),
            make_char('e', 132.0, 515.0, 5.0, 12.0),
        ];
        let blocks = cluster_chars(&chars, "Helvetica", 12.0, 0);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].lines.len(), 1); // Merged into single line
        let text = line_text(&blocks[0].lines[0]);
        assert_eq!(text, "comprehensive");
    }
}
