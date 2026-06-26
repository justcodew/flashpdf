/// Layout analysis: cluster chars → spans → lines → blocks.
///
/// Uses geometric proximity and font/size/color matching to group characters
/// into semantically meaningful text structures compatible with PyMuPDF output.
use crate::parser::content_stream::{CharInfo, TextBlock, TextLine, TextSpan};
use smallvec::SmallVec;
use std::cmp::Ordering;

// ─── Layout parameters (tunable) ───

/// Maximum horizontal gap between chars in the same span (as fraction of font size).
const SPAN_GAP_FACTOR: f64 = 0.3;
/// Maximum vertical distance between spans in the same line (as fraction of font size).
const LINE_VERT_FACTOR: f64 = 0.5;
/// Minimum vertical gap between lines to start a new block (as fraction of font size).
const BLOCK_GAP_FACTOR: f64 = 1.5;
/// Cluster extracted characters into spans, lines, and blocks.
///
/// Input: flat list of CharInfo from content stream scanning. Each CharInfo
/// carries its own font size (from the Tf operator at emit time); the global
/// `font_size` param is a page-level fallback used for thresholds.
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
/// Uses a sweep-line approach: project span X-intervals, find the largest
/// empty X-gap, and (if it's wide enough relative to the page) treat it
/// as the column boundary.
///
/// Why sweep-line over histogram+peaks: the histogram approach finds the
/// two densest X positions, but in real documents those can both be in
/// the SAME column (e.g. body-text peak + paragraph-indent peak, or two
/// peaks within the left column separated by a figure). The sweep-line
/// finds an actually-empty vertical band, which only exists between two
/// distinct columns.
fn detect_columns_from_spans(spans: &[TextSpan]) -> Vec<Vec<TextSpan>> {
    if spans.len() < 6 {
        return vec![spans.to_vec()];
    }

    let x_min = spans.iter().map(|s| s.bbox[0]).fold(f64::MAX, f64::min);
    let x_max = spans.iter().map(|s| s.bbox[2]).fold(f64::MIN, f64::max);
    let x_range = x_max - x_min;

    if x_range < 100.0 {
        return vec![spans.to_vec()];
    }

    // Sort intervals by start; sweep tracking the running max end.
    let mut ivs: Vec<(f64, f64)> = spans.iter().map(|s| (s.bbox[0], s.bbox[2])).collect();
    ivs.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());

    let mut max_gap = 0.0;
    let mut max_gap_pos = 0.0;
    let mut cur_end = ivs[0].1;
    for &(s, e) in ivs.iter().skip(1) {
        if s > cur_end {
            let g = s - cur_end;
            if g > max_gap {
                max_gap = g;
                max_gap_pos = cur_end + g * 0.5;
            }
        }
        cur_end = cur_end.max(e);
    }

    // Gap must be at least 4% of x range to be a real column boundary.
    // 4% of a 500px page = 20px, a typical inter-column gutter width.
    if max_gap < x_range * 0.04 {
        return vec![spans.to_vec()];
    }

    // Split spans by left edge.
    let mut left = Vec::new();
    let mut right = Vec::new();
    for s in spans {
        if s.bbox[0] < max_gap_pos {
            left.push(s.clone());
        } else {
            right.push(s.clone());
        }
    }

    if left.len() < 2 || right.len() < 2 {
        return vec![spans.to_vec()];
    }

    vec![left, right]
}

/// Build spans from consecutive characters with similar position.
/// `font_size` is the page-level size used for threshold basis (matches
/// historical behavior); per-char sizes remain available on CharInfo.
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

    // NOTE: Do NOT pre-sort spans here.
    // PDF content streams emit text objects in reading order for well-formed
    // documents (title → authors → left column → right column). Pre-sorting
    // by (y, x) destroys this order and merges spans from different columns
    // into single blocks when detect_columns_from_spans fails. This mirrors
    // MuPDF's approach in stext-device.c, which trusts content stream order
    // and uses pen-delta heuristics (column-gap detection below) to split.
    // Column gap thresholds (computed per-iteration from the running span size):
    // - Backward jump: span starts far left of previous span end (right→left column wrap)
    // - Forward jump: span starts far right of previous span end (left→right column)
    let line_threshold = font_size * LINE_VERT_FACTOR;
    // Column gap thresholds:
    // - Backward jump: span starts far left of previous span end (right→left column wrap)
    // - Forward jump: span starts far right of previous span end (left→right column)
    let col_gap_backward = -font_size * 10.0;
    let col_gap_forward = font_size * 5.0;

    let mut lines: Vec<TextLine> = Vec::new();
    let mut current_spans: Vec<TextSpan> = vec![spans[0].clone()];

    for i in 1..spans.len() {
        let prev_y = current_spans.last().unwrap().bbox[1];
        let curr_y = spans[i].bbox[1];
        let prev_x2 = current_spans.last().unwrap().bbox[2];
        let curr_x = spans[i].bbox[0];
        let h_gap = curr_x - prev_x2;

        let same_line = (curr_y - prev_y).abs() < line_threshold
            && h_gap > col_gap_backward
            && h_gap < col_gap_forward;

        if same_line {
            current_spans.push(spans[i].clone());
        } else {
            lines.push(make_line(current_spans));
            current_spans = vec![spans[i].clone()];
        }
    }

    lines.push(make_line(current_spans));
    lines
}

fn make_line(spans: Vec<TextSpan>) -> TextLine {
    // Sort spans within line by x position
    let mut sorted = spans;
    sorted.sort_by(|a, b| a.bbox[0].partial_cmp(&b.bbox[0]).unwrap());

    // Insert space between adjacent spans when:
    //   (a) there's a visual gap larger than 0.15 * min_size (MuPDF SPACE_DIST),
    // OR
    //   (b) the two spans differ significantly in size (sub/superscript
    //       transition like "...Briegel1 Hendrik..." where the "1" is at
    //       7pt and "Hendrik" at 10pt — geometrically tight but visually
    //       a word boundary).
    // Without this, "Hello" + "World" on the same line with separate Tj
    // operators concatenates to "HelloWorld", and superscripts glue to
    // the following word ("1Hendrik").
    if sorted.len() > 1 {
        for i in 1..sorted.len() {
            if sorted[i].text.starts_with(' ') {
                continue;
            }
            let prev = &sorted[i - 1];
            let curr = &sorted[i];
            let gap = curr.bbox[0] - prev.bbox[2];
            let min_size = curr.size.min(prev.size).max(1.0);
            let max_size = curr.size.max(prev.size).max(1.0);
            // (a) MuPDF-style gap trigger, but BANDED: insert a space only when
            // the gap is in the word-boundary range (0.25–0.6 em). Below 0.25 em
            // chars are kerned together or part of a tight cluster like the
            // Roman numeral "II" (two separate Tj operators with a small gap);
            // above 0.6 em the gap is a tab/heading alignment, and PyMuPDF
            // does not insert a space there (it emits "I.Introduction" for
            // section-number tabs, not "I. Introduction").
            let gap_triggers = gap > min_size * 0.25 && gap < min_size * 0.6;
            // (b) Size-change transition: size differs by > 25% AND prev does
            // not already end with the smaller char (so we don't separate a
            // superscript from its anchor like "Briegel" + "1").
            let size_ratio = (max_size - min_size) / max_size;
            let prev_ends_small = prev
                .text
                .chars()
                .last()
                .map(|c| c.is_numeric())
                .unwrap_or(false);
            let size_triggers = size_ratio > 0.25 && !prev_ends_small;
            if gap_triggers || size_triggers {
                sorted[i].text.insert(0, ' ');
            }
        }
    }

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
        let prev = current_lines.last().unwrap();
        let curr = &lines[i];
        // Vertical whitespace between two lines (PDF y-up: bbox[3] > bbox[1]).
        // Direction-agnostic so it works with content stream order, which emits
        // lines top-to-bottom visually (y descending).
        let gap = curr.bbox[1].max(prev.bbox[1]) - curr.bbox[3].min(prev.bbox[3]);

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
    // Hyphenation merge is disabled: the heuristic (line ends with '-' and
    // next starts with lowercase) removes hyphens from compound words like
    // "measurement-based" that happen to break at the hyphen, producing
    // "measurementbased". PyMuPDF preserves the hyphen when joining, so
    // matching their behavior means leaving the text alone.
    let _ = merge_hyphens_pass;
    blocks
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

// ─── Reading order (recursive XY-cut) ───

/// Minimum empty gap (as fraction of page dimension) required to split a band/column.
const READING_MIN_GAP_FRAC: f64 = 0.015;

/// Sort `blocks` into visual reading order using recursive XY-cut (Nagy).
///
/// First tries horizontal cuts (separate title band from 2-col body), then
/// vertical cuts (separate left/right columns); falls back to (y_top, x_left)
/// sort when no significant gap is found. Free function over the final block
/// list — does not affect span/line/block clustering.
///
/// Coordinate convention: PDF y-up (origin at bottom-left, y grows upward),
/// so visually-higher blocks have larger y and sort earlier.
pub fn reading_order_sort(blocks: Vec<TextBlock>, page_rect: [f64; 4]) -> Vec<TextBlock> {
    reading_order_sort_with_diagnostics(blocks, page_rect).0
}

/// Same as `reading_order_sort` but also returns the number of blocks dropped
/// by the out-of-page margin filter. Used by the diagnostics layer to surface
/// "N blocks were dropped because their bbox poked outside the page" — the
/// caller can then investigate (often a sign of mis-clustered vector graphics
/// or rotated text whose AABB doesn't fit the page rect).
pub fn reading_order_sort_with_diagnostics(
    blocks: Vec<TextBlock>,
    page_rect: [f64; 4],
) -> (Vec<TextBlock>, usize) {
    if blocks.len() <= 1 {
        return (blocks, 0);
    }
    // Defensive filter: drop blocks whose bbox is far outside the page rect.
    // Two categories of "outside":
    //   (a) WAY outside (millions): Type 3 glyphs / vector graphics mis-clustered
    //       as text — these would obliterate every gap the XY-cut looks for.
    //       Allow 2x page dimension of slack.
    //   (b) Moderately outside (~10%+ overflow on one edge): typically rotated
    //       sidebar watermarks like the arXiv banner whose bbox is the bounding
    //       box of rotated text and extends far beyond the page. These are
    //       noise for layout purposes — drop them so they don't displace
    //       legitimate body blocks in the reading order.
    let page_w = (page_rect[2] - page_rect[0]).abs().max(1.0);
    let page_h = (page_rect[3] - page_rect[1]).abs().max(1.0);
    let slack_x = page_w * 2.0;
    let slack_y = page_h * 2.0;
    let margin_x = page_w * 0.1;
    let margin_y = page_h * 0.1;
    let before = blocks.len();
    let blocks: Vec<TextBlock> = blocks
        .into_iter()
        .filter(|b| {
            let x0 = b.bbox[0].min(b.bbox[2]);
            let x1 = b.bbox[0].max(b.bbox[2]);
            let y0 = b.bbox[1].min(b.bbox[3]);
            let y1 = b.bbox[1].max(b.bbox[3]);
            // Reject if either edge is far outside the page rect.
            x0 >= page_rect[0] - slack_x
                && x1 <= page_rect[2] + slack_x
                && y0 >= page_rect[1] - slack_y
                && y1 <= page_rect[3] + slack_y
                && x1 <= page_rect[2] + margin_x
                && x0 >= page_rect[0] - margin_x
                && y1 <= page_rect[3] + margin_y
                && y0 >= page_rect[1] - margin_y
        })
        .collect();
    let dropped = before - blocks.len();
    if blocks.len() <= 1 {
        return (blocks, dropped);
    }
    (xy_cut(blocks, page_rect), dropped)
}

fn xy_cut(mut blocks: Vec<TextBlock>, rect: [f64; 4]) -> Vec<TextBlock> {
    if blocks.len() <= 1 {
        return blocks;
    }
    // 1. Horizontal cut: separates title band from 2-col body.
    // split_by_axis returns (center<pos, center>=pos). In PDF y-up coords the
    // higher-y half (center>=pos) is visually ABOVE → must be returned first.
    // Anti-recursion guard: largest_gap filters out wide blocks (>70% of
    // rect extent) when computing the gap, but split_by_axis partitions ALL
    // blocks. If the gap exists only in the narrow subset, one half ends up
    // empty and the other holds the full set — recursing on the full set
    // re-finds the same gap, same split, infinite recursion → stack overflow
    // → SIGBUS. Guard: only recurse when both halves are non-empty;
    // otherwise fall through to the next cut or the sort fallback.
    if let Some(g) = largest_gap(&blocks, 1, rect) {
        if g.gap >= READING_MIN_GAP_FRAC * (rect[3] - rect[1]) {
            let (bot_blocks, top_blocks) = split_by_axis(blocks, 1, g.pos);
            if !top_blocks.is_empty() && !bot_blocks.is_empty() {
                let mut out = xy_cut(top_blocks, [rect[0], g.pos, rect[2], rect[3]]);
                out.extend(xy_cut(bot_blocks, [rect[0], rect[1], rect[2], g.pos]));
                return out;
            }
            // One side empty → recombine and try the next cut.
            let mut merged = top_blocks;
            merged.extend(bot_blocks);
            blocks = merged;
        }
    }
    // 2. Vertical cut: separates columns. Left column reads first.
    if let Some(g) = largest_gap(&blocks, 0, rect) {
        if g.gap >= READING_MIN_GAP_FRAC * (rect[2] - rect[0]) {
            let (l, r) = split_by_axis(blocks, 0, g.pos);
            if !l.is_empty() && !r.is_empty() {
                let mut out = xy_cut(l, [rect[0], rect[1], g.pos, rect[3]]);
                out.extend(xy_cut(r, [g.pos, rect[1], rect[2], rect[3]]));
                return out;
            }
            let mut merged = l;
            merged.extend(r);
            blocks = merged;
        }
    }
    // 3. Fallback: sort by (y_top DESC [higher y = visually above], x_left ASC).
    // Use bbox[3] (top edge in PDF y-up coords) rather than bbox[1] (bottom)
    // so that tall blocks (e.g. an Abstract spanning most of the page) are
    // ordered by where they START at the top of the page, not where their
    // bottom ends. Otherwise short blocks above a tall block's bottom edge
    // (e.g. an arXiv watermark at y=232 vs the Abstract's bottom at y=162)
    // would be sorted before the tall block despite being visually below its
    // top.
    blocks.sort_by(|a, b| {
        b.bbox[3]
            .partial_cmp(&a.bbox[3])
            .unwrap_or(Ordering::Equal)
            .then_with(|| a.bbox[0].partial_cmp(&b.bbox[0]).unwrap_or(Ordering::Equal))
    });
    blocks
}

struct Gap {
    pos: f64,
    gap: f64,
}

/// Trait shared by TextBlock and TextSpan so the XY-cut gap/split helpers can
/// be generic over either granularity.
trait BBox2D {
    fn bbox(&self) -> [f64; 4];
}

impl BBox2D for TextBlock {
    fn bbox(&self) -> [f64; 4] {
        self.bbox
    }
}

impl BBox2D for TextSpan {
    fn bbox(&self) -> [f64; 4] {
        self.bbox
    }
}

/// Project item bboxes onto `axis` (0=x, 1=y) and find the largest empty gap
/// via a sweep line over sorted intervals. Returns None if no gap exists.
///
/// Items whose extent along `axis` exceeds 70% of the page rect's extent on
/// that axis are excluded from the sweep: a full-width page number or a
/// full-height sidebar bridges legitimate gaps (column gutter, title/body
/// separation) and would otherwise hide them from the cut detection. The
/// excluded items are still subject to the cut returned by the caller — only
/// the GAP DETECTION ignores them.
fn largest_gap<T: BBox2D>(items: &[T], axis: usize, rect: [f64; 4]) -> Option<Gap> {
    let axis_extent = (rect[axis + 2] - rect[axis]).abs().max(1.0);
    let wide_threshold = axis_extent * 0.70;
    let mut ivs: Vec<(f64, f64)> = items
        .iter()
        .filter_map(|b| {
            let bb = b.bbox();
            let extent = (bb[axis + 2] - bb[axis]).abs();
            if extent > wide_threshold {
                None
            } else {
                Some((bb[axis], bb[axis + 2]))
            }
        })
        .collect();
    if ivs.is_empty() {
        return None;
    }
    ivs.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(Ordering::Equal));
    let mut max_gap = 0.0;
    let mut max_pos = 0.0;
    let mut cur_end = ivs[0].1;
    for &(s, e) in ivs.iter().skip(1) {
        if s > cur_end {
            let g = s - cur_end;
            if g > max_gap {
                max_gap = g;
                max_pos = cur_end + g * 0.5;
            }
        }
        cur_end = cur_end.max(e);
    }
    if max_gap <= 0.0 {
        None
    } else {
        Some(Gap {
            pos: max_pos,
            gap: max_gap,
        })
    }
}

/// Split items into two vectors by their center along `axis` relative to `pos`.
/// Center-based (rather than bbox-overlap) so wide items straddling the cut
/// (e.g. a full-width title over the column boundary) still get assigned
/// deterministically to the band where most of their content sits.
fn split_by_axis<T: BBox2D>(items: Vec<T>, axis: usize, pos: f64) -> (Vec<T>, Vec<T>) {
    let (mut lo, mut hi) = (Vec::new(), Vec::new());
    for it in items {
        let bb = it.bbox();
        let c = (bb[axis] + bb[axis + 2]) * 0.5;
        if c < pos {
            lo.push(it);
        } else {
            hi.push(it);
        }
    }
    (lo, hi)
}

// ─── Tests ───

#[cfg(test)]
mod tests {
    use super::*;

    fn make_char(c: char, x: f64, y: f64, w: f64, h: f64) -> CharInfo {
        CharInfo {
            c,
            bbox: [x, y, x + w, y + h],
            size: h,
            rotated: false,
        }
    }

    /// Build a single-block TextBlock with one span/line containing `text`,
    /// positioned at bbox (x0,y0,x1,y1). Used for reading-order tests.
    fn make_block(text: &str, x0: f64, y0: f64, x1: f64, y1: f64) -> TextBlock {
        let span = TextSpan {
            text: text.to_string(),
            font: "Helvetica".to_string(),
            size: 12.0,
            color: 0,
            bbox: [x0, y0, x1, y1],
            chars: Vec::new(),
        };
        let line = TextLine {
            bbox: [x0, y0, x1, y1],
            spans: vec![span],
        };
        TextBlock {
            bbox: [x0, y0, x1, y1],
            lines: vec![line],
        }
    }

    #[test]
    fn test_reading_order_title_and_two_columns() {
        // US-letter page. Title spans full width at top; two columns below.
        let page = [0.0, 0.0, 612.0, 792.0];
        let title = make_block("TITLE", 80.0, 740.0, 532.0, 760.0);
        let left1 = make_block("L1", 80.0, 700.0, 290.0, 720.0);
        let left2 = make_block("L2", 80.0, 670.0, 290.0, 690.0);
        let right1 = make_block("R1", 322.0, 700.0, 532.0, 720.0);
        let right2 = make_block("R2", 322.0, 670.0, 532.0, 690.0);

        // Shuffle input order to prove sort actually reorders.
        let blocks = vec![right1, left1, left2, title, right2];
        let out = reading_order_sort(blocks, page);

        fn txt(b: &TextBlock) -> &str {
            &b.lines[0].spans[0].text
        }
        assert_eq!(out.len(), 5);
        assert_eq!(txt(&out[0]), "TITLE");
        assert_eq!(txt(&out[1]), "L1");
        assert_eq!(txt(&out[2]), "L2");
        assert_eq!(txt(&out[3]), "R1");
        assert_eq!(txt(&out[4]), "R2");
    }

    #[test]
    fn test_reading_order_single_column_falls_back_to_yx() {
        // No significant horizontal or vertical gap → yx fallback sort.
        // Two stacked blocks supplied out of order (bottom block first).
        let page = [0.0, 0.0, 612.0, 792.0];
        let top = make_block("TOP", 100.0, 700.0, 200.0, 720.0);
        let bot = make_block("BOT", 100.0, 680.0, 200.0, 700.0);
        let out = reading_order_sort(vec![bot.clone(), top.clone()], page);
        // Higher y comes first in PDF coords (y grows upward here).
        assert_eq!(&out[0].lines[0].spans[0].text, "TOP");
        assert_eq!(&out[1].lines[0].spans[0].text, "BOT");
    }

    #[test]
    fn test_reading_order_empty_and_single() {
        let page = [0.0, 0.0, 612.0, 792.0];
        let empty: Vec<TextBlock> = Vec::new();
        assert!(reading_order_sort(empty, page).is_empty());

        let single = make_block("X", 100.0, 700.0, 200.0, 720.0);
        let out = reading_order_sort(vec![single.clone()], page);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].lines[0].spans[0].text, "X");
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
        // Two chars 12px apart vertically — within normal line spacing
        // (line height ≈ 1.2 × font_size = 14.4 at 12pt), so same block.
        let chars = vec![
            make_char('A', 100.0, 700.0, 7.0, 12.0),
            make_char('B', 100.0, 688.0, 7.0, 12.0),
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
        // Gap between "Hi" and "Wo" is 38pt = 3.17em at 12pt — far above
        // the 0.6em upper bound of the word-boundary band. PyMuPDF treats
        // gaps this large as heading/tab alignment and does NOT insert a
        // space, so we don't either.
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
        // Hyphenation merge is intentionally disabled (see merge_hyphenated_lines).
        // Two lines with hyphen at end of first line remain separate lines.
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
        // Hyphenation merge disabled: lines stay separate.
        assert_eq!(blocks[0].lines.len(), 2);
        assert_eq!(line_text(&blocks[0].lines[0]), "compre-");
        assert_eq!(line_text(&blocks[0].lines[1]), "hensive");
    }
}
