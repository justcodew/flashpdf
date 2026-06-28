/// Content stream parser — extracts text and image references from PDF page content.
///
/// Uses a state machine driven by memchr-based operator scanning.
/// Handles BT/ET text blocks, Tj/TJ/T'/T" operators, Td/TD/Tm matrix,
/// font selection (Tf), graphics state (q/Q/cm), and image capture (Do).
use crate::font::FontInfo;
use crate::types::ObjectId;
use fast_float2;
use smallvec::SmallVec;
use std::collections::HashMap;

/// Maximum recursion depth for Form XObjects.
const MAX_FORM_DEPTH: usize = 3;

// ─── Public output types ───

/// A single extracted character with its position and font info.
#[derive(Debug, Clone)]
pub struct CharInfo {
    /// The Unicode character (or U+FFFD for undecodable bytes)
    pub c: char,
    /// Bounding box: (x0, y0, x1, y1) in page coordinates
    pub bbox: [f64; 4],
    /// Font size at emit time (per-char, from Tf operator)
    pub size: f64,
    /// True if the char was emitted under a non-axis-aligned text matrix
    /// (rotated/sheared text — e.g. arXiv watermarks, vertical chart axis
    /// labels). Layout treats these as noise by default because XY-cut
    /// can't mix horizontal and vertical blocks; users can opt in via
    /// `ExtractOptions::include_rotated` to keep them.
    pub rotated: bool,
}

/// A span of characters sharing the same font/size/color.
#[derive(Debug, Clone)]
pub struct TextSpan {
    pub text: String,
    pub font: String,
    pub size: f64,
    pub color: u32,
    pub bbox: [f64; 4],
    pub chars: Vec<CharInfo>,
    /// fitz-compatible flag bitmask (italic=2, serif=4, mono=8, bold=16).
    /// Populated from FontInfo.flags of the span's font. 0 when the font
    /// couldn't be classified (e.g. no /FontDescriptor and an unknown name).
    pub flags: u32,
}

/// A line of text (one or more vertically-aligned spans).
#[derive(Debug, Clone)]
pub struct TextLine {
    pub bbox: [f64; 4],
    pub spans: Vec<TextSpan>,
}

/// A text block (paragraph-level grouping).
#[derive(Debug, Clone)]
pub struct TextBlock {
    pub bbox: [f64; 4],
    pub lines: Vec<TextLine>,
}

/// An image reference captured from a `Do` operator.
#[derive(Debug, Clone)]
pub struct ImageRef {
    pub name: String,
    pub bbox: [f64; 4],
    pub obj_ref: Option<ObjectId>,
}

/// All extracted content from a content stream.
#[derive(Debug, Clone, Default)]
pub struct ContentResult {
    pub chars: Vec<CharInfo>,
    pub images: Vec<ImageRef>,
    /// Chars emitted under a /Type3 font (glyph defined by drawing operators,
    /// not an outline). Tracked for diagnostics — Type 3 positioning may be
    /// inaccurate and glyphs without /ToUnicode are unreadable.
    pub type3_char_count: usize,
    /// Bytes that could not be mapped to Unicode (emitted as U+FFFD).
    /// Indicates missing /ToUnicode or /Encoding on the font.
    pub undecoded_byte_count: usize,
    /// Number of inline images encountered (BI/ID/EI operators, PDF spec §8.9.7).
    /// Inline images are pixel data embedded directly in the content stream
    /// (no /XObject reference). Tracked for diagnostics — they're added to
    /// `images` with `name = "inline"`, but `is_scanned` detection also
    /// relies on this counter to flag inline-only scanned pages.
    pub inline_image_count: usize,
}

// ─── Operator stack value ───

#[derive(Debug, Clone)]
enum Operand {
    Real(f64),
    Int(i64),
    Str(Vec<u8>),
    Name(Vec<u8>),
    Array(Vec<Operand>),
}

impl Operand {
    fn as_f64(&self) -> f64 {
        match self {
            Operand::Real(f) => *f,
            Operand::Int(n) => *n as f64,
            _ => 0.0,
        }
    }

    fn as_i64(&self) -> i64 {
        match self {
            Operand::Int(n) => *n,
            Operand::Real(f) => *f as i64,
            _ => 0,
        }
    }

    fn as_name(&self) -> &[u8] {
        match self {
            Operand::Name(n) => n,
            _ => b"",
        }
    }

    fn as_str(&self) -> &[u8] {
        match self {
            Operand::Str(s) => s,
            _ => &[],
        }
    }
}

// ─── Text matrix state ───

/// A 3x3 affine matrix (a b c d e f) for coordinate transforms.
#[derive(Debug, Clone, Copy)]
struct Matrix {
    a: f64,
    b: f64,
    c: f64,
    d: f64,
    e: f64,
    f: f64,
}

impl Matrix {
    fn identity() -> Self {
        Self {
            a: 1.0,
            b: 0.0,
            c: 0.0,
            d: 1.0,
            e: 0.0,
            f: 0.0,
        }
    }

    fn new(a: f64, b: f64, c: f64, d: f64, e: f64, f: f64) -> Self {
        Self { a, b, c, d, e, f }
    }

    /// Apply this matrix to a point (x, y).
    fn apply(&self, x: f64, y: f64) -> (f64, f64) {
        (
            self.a * x + self.c * y + self.e,
            self.b * x + self.d * y + self.f,
        )
    }

    /// Multiply: self * other
    fn mul(&self, other: &Matrix) -> Matrix {
        Matrix {
            a: self.a * other.a + self.c * other.b,
            b: self.b * other.a + self.d * other.b,
            c: self.a * other.c + self.c * other.d,
            d: self.b * other.c + self.d * other.d,
            e: self.a * other.e + self.c * other.f + self.e,
            f: self.b * other.e + self.d * other.f + self.f,
        }
    }
}

// ─── Graphics state ───

struct TextState {
    /// Current transformation matrix (page-level)
    ctm: Matrix,
    /// Text matrix
    tm: Matrix,
    /// Text line matrix
    tlm: Matrix,
    /// Current font name (from Tf operator)
    font_name: Vec<u8>,
    /// Current font size
    font_size: f64,
    /// Horizontal scaling (Tz)
    h_scale: f64,
    /// Character spacing (Tc)
    char_spacing: f64,
    /// Word spacing (Tw)
    word_spacing: f64,
    /// Leading (TL)
    leading: f64,
    /// Text rendering mode (Tr)
    render_mode: u8,
    /// Fill color (simplified as u32)
    fill_color: u32,
    /// Graphics state stack (for q/Q)
    gs_stack: Vec<GraphicsState>,
}

struct GraphicsState {
    ctm: Matrix,
}

impl TextState {
    fn new() -> Self {
        Self {
            ctm: Matrix::identity(),
            tm: Matrix::identity(),
            tlm: Matrix::identity(),
            font_name: Vec::new(),
            font_size: 12.0,
            h_scale: 1.0,
            char_spacing: 0.0,
            word_spacing: 0.0,
            leading: 0.0,
            render_mode: 0,
            fill_color: 0,
            gs_stack: Vec::new(),
        }
    }

    fn save_gs(&mut self) {
        self.gs_stack.push(GraphicsState { ctm: self.ctm });
    }

    fn restore_gs(&mut self) {
        if let Some(gs) = self.gs_stack.pop() {
            self.ctm = gs.ctm;
        }
    }
}

// ─── Content stream scanner ───

/// Scan a content stream and extract text characters and image references.
pub fn scan_content_stream(data: &[u8]) -> ContentResult {
    let mut result = ContentResult::default();
    let mut state = TextState::new();
    let mut operands: SmallVec<[Operand; 8]> = SmallVec::new();
    let mut pos = 0;

    while pos < data.len() {
        skip_ws_and_comments(data, &mut pos);
        if pos >= data.len() {
            break;
        }

        let b = data[pos];

        // Check if this is an operator (letter) or operand
        if is_operator_start(b) {
            let op_start = pos;
            while pos < data.len() && is_operator_char(data[pos]) {
                pos += 1;
            }
            let op = &data[op_start..pos];

            // BI = Begin Inline Image — handle as a special case (image data
            // between ID and EI is binary and can't go through normal loop).
            if op == b"BI" {
                parse_inline_image(data, &mut pos, &state, &mut result);
                operands.clear();
                continue;
            }

            execute_operator(op, &operands, &mut state, &mut result, &HashMap::new());
            operands.clear();
        } else {
            // Parse operand
            if let Some(operand) = parse_operand(data, &mut pos) {
                operands.push(operand);
            }
        }
    }

    result
}

/// Scan a content stream with font-aware character decoding.
pub fn scan_content_stream_with_fonts(
    data: &[u8],
    fonts: &HashMap<String, FontInfo>,
) -> ContentResult {
    scan_content_stream_full(data, fonts, &HashMap::new(), 0)
}

/// Context for content stream scanning with Form XObject support.
pub struct StreamContext<'a> {
    pub fonts: &'a HashMap<String, FontInfo>,
    /// Map from XObject name to (content stream data, is_form, form_matrix)
    pub xobjects: &'a HashMap<String, XObjectData>,
}

/// Data for an XObject (Form or Image).
pub enum XObjectData {
    Form {
        data: Vec<u8>,
        matrix: [f64; 6],
        bbox: [f64; 4],
        /// Fonts from the Form XObject's own /Resources (merged during recursion)
        fonts: HashMap<String, FontInfo>,
    },
    Image,
}

/// Scan with full context: fonts + XObjects + recursion depth.
pub fn scan_content_stream_full(
    data: &[u8],
    fonts: &HashMap<String, FontInfo>,
    xobjects: &HashMap<String, XObjectData>,
    depth: usize,
) -> ContentResult {
    let mut result = ContentResult::default();
    let mut state = TextState::new();
    let mut operands: SmallVec<[Operand; 8]> = SmallVec::new();
    let mut pos = 0;

    while pos < data.len() {
        skip_ws_and_comments(data, &mut pos);
        if pos >= data.len() {
            break;
        }

        let b = data[pos];

        if is_operator_start(b) {
            let op_start = pos;
            while pos < data.len() && is_operator_char(data[pos]) {
                pos += 1;
            }
            let op = &data[op_start..pos];

            // BI = Begin Inline Image — special case: consume the entire
            // BI/ID/EI block in one go. The image data between ID and EI is
            // arbitrary binary (may contain operator-looking bytes), so we
            // can't feed it through the normal operator loop.
            if op == b"BI" {
                parse_inline_image(data, &mut pos, &state, &mut result);
                operands.clear();
                continue;
            }

            execute_operator_full(
                op,
                &operands,
                &mut state,
                &mut result,
                fonts,
                xobjects,
                depth,
            );
            operands.clear();
        } else {
            if let Some(operand) = parse_operand(data, &mut pos) {
                operands.push(operand);
            }
        }
    }

    result
}

/// Parse an inline image starting just after the `BI` operator.
///
/// Grammar (PDF spec §8.9.7):
/// ```text
/// BI <key-value-pairs> ID <single-whitespace> <binary-data> EI
/// ```
///
/// - Keys are abbreviated names (/W, /H, /BPC, /CS, /F, ...) but for
///   our purposes we don't decode them — we only capture existence + bbox.
/// - After `ID`, exactly ONE whitespace byte precedes the image data.
/// - Image data is binary and may contain bytes that look like `EI`.
///   The spec requires looking for whitespace + "EI" + whitespace to
///   disambiguate. We use a 2-byte-lookahead scan: detect `EI` only when
///   preceded by whitespace and followed by whitespace or EOF.
///
/// On success, pushes an `ImageRef { name: "inline", ... }` into `result.images`,
/// bumps `result.inline_image_count`, and advances `*pos` past the EI.
/// On malformed input (no ID found, no EI terminator), advances conservatively
/// to avoid an infinite loop but does not push an ImageRef.
fn parse_inline_image(data: &[u8], pos: &mut usize, state: &TextState, result: &mut ContentResult) {
    // 1. Skip key-value pairs until we hit the ID operator.
    //    Keys are /Name, values are names/numbers/arrays. We don't care about
    //    their content — just find the ID operator that separates params
    //    from image data.
    let mut id_pos = *pos;
    let mut found_id = false;
    while id_pos + 1 < data.len() {
        // Look for "ID" as a standalone operator: must be preceded by
        // whitespace (or BOF after BI) and followed by whitespace.
        if data[id_pos] == b'I' && data[id_pos + 1] == b'D' {
            let prev_ok = id_pos == 0
                || matches!(
                    data[id_pos - 1],
                    b' ' | b'\t' | b'\n' | b'\r' | b'\x00' | b'/'
                );
            let next_ok = id_pos + 2 >= data.len()
                || matches!(data[id_pos + 2], b' ' | b'\t' | b'\n' | b'\r' | b'\x00');
            if prev_ok && next_ok {
                found_id = true;
                break;
            }
        }
        id_pos += 1;
    }
    if !found_id {
        // Malformed — bail without producing an image, but advance to avoid
        // an infinite loop in the caller.
        *pos = data.len();
        return;
    }

    // 2. Skip the ID operator + exactly ONE whitespace byte (PDF spec requirement).
    let mut data_start = id_pos + 2;
    if data_start < data.len() && matches!(data[data_start], b' ' | b'\t' | b'\n' | b'\r' | b'\x00')
    {
        data_start += 1;
    }

    // 3. Scan for the EI terminator. The classic ambiguity: image bytes can
    //    contain "EI" sequences. The spec's resolution: EI must be recognized
    //    only when preceded by whitespace and followed by whitespace/EOF.
    //    This is the same heuristic MuPDF uses.
    let mut ei_pos = data_start;
    let mut found_ei = false;
    while ei_pos + 1 < data.len() {
        if data[ei_pos] == b'E' && data[ei_pos + 1] == b'I' {
            let prev_ws = ei_pos > data_start
                && matches!(data[ei_pos - 1], b' ' | b'\t' | b'\n' | b'\r' | b'\x00');
            let next_ws = ei_pos + 2 >= data.len()
                || matches!(data[ei_pos + 2], b' ' | b'\t' | b'\n' | b'\r' | b'\x00');
            if prev_ws && next_ws {
                found_ei = true;
                break;
            }
        }
        ei_pos += 1;
    }

    if !found_ei {
        // No terminator — malformed. Bail.
        *pos = data.len();
        return;
    }

    // 4. Build an ImageRef. The image occupies the unit square [0,0,1,1] in
    //    content-stream space (same convention as image XObjects), so we
    //    map it through the current CTM to get device-space bbox. Same
    //    approach as the Do-operator image case.
    let (x0, y0) = state.ctm.apply(0.0, 0.0);
    let (x1, y1) = state.ctm.apply(1.0, 1.0);
    result.images.push(ImageRef {
        name: "inline".to_string(),
        bbox: [x0.min(x1), y0.min(y1), x0.max(x1), y0.max(y1)],
        obj_ref: None,
    });
    result.inline_image_count += 1;

    // 5. Advance past the EI operator.
    *pos = ei_pos + 2;
}

fn skip_ws_and_comments(data: &[u8], pos: &mut usize) {
    while *pos < data.len() {
        match data[*pos] {
            b' ' | b'\t' | b'\n' | b'\r' | b'\x00' => *pos += 1,
            b'%' => {
                // Comment: skip to end of line
                *pos += 1;
                while *pos < data.len() && data[*pos] != b'\n' && data[*pos] != b'\r' {
                    *pos += 1;
                }
            }
            _ => break,
        }
    }
}

fn is_operator_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'\'' || b == b'"' || b == b'*'
}

fn is_operator_char(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'\'' || b == b'"' || b == b'*'
}

fn parse_operand(data: &[u8], pos: &mut usize) -> Option<Operand> {
    let b = data[*pos];
    match b {
        b'0'..=b'9' | b'+' | b'-' | b'.' => parse_number(data, pos),
        b'(' => parse_string_literal(data, pos),
        b'<' => {
            if *pos + 1 < data.len() && data[*pos + 1] == b'<' {
                // Dict — skip for content streams (not expected as operand)
                *pos += 2;
                None
            } else {
                parse_hex_string(data, pos)
            }
        }
        b'/' => parse_name(data, pos),
        b'[' => parse_array(data, pos),
        b']' => {
            *pos += 1;
            None
        }
        _ => {
            *pos += 1;
            None
        }
    }
}

fn parse_number(data: &[u8], pos: &mut usize) -> Option<Operand> {
    let start = *pos;
    let mut has_dot = false;
    let mut has_exp = false;

    if *pos < data.len() && (data[*pos] == b'+' || data[*pos] == b'-') {
        *pos += 1;
    }
    while *pos < data.len() && data[*pos].is_ascii_digit() {
        *pos += 1;
    }
    if *pos < data.len() && data[*pos] == b'.' {
        has_dot = true;
        *pos += 1;
        while *pos < data.len() && data[*pos].is_ascii_digit() {
            *pos += 1;
        }
    }
    if *pos < data.len() && (data[*pos] == b'e' || data[*pos] == b'E') {
        has_exp = true;
        *pos += 1;
        if *pos < data.len() && (data[*pos] == b'+' || data[*pos] == b'-') {
            *pos += 1;
        }
        while *pos < data.len() && data[*pos].is_ascii_digit() {
            *pos += 1;
        }
    }

    let s = std::str::from_utf8(&data[start..*pos]).ok()?;
    if has_dot || has_exp {
        fast_float2::parse(s).ok().map(Operand::Real)
    } else {
        s.parse::<i64>().ok().map(Operand::Int)
    }
}

fn parse_string_literal(data: &[u8], pos: &mut usize) -> Option<Operand> {
    *pos += 1; // skip '('
    let mut result = Vec::new();
    let mut depth = 1i32;

    while *pos < data.len() && depth > 0 {
        let b = data[*pos];
        *pos += 1;
        match b {
            b'(' => {
                depth += 1;
                result.push(b);
            }
            b')' => {
                depth -= 1;
                if depth > 0 {
                    result.push(b);
                }
            }
            b'\\' => {
                if *pos < data.len() {
                    let esc = data[*pos];
                    *pos += 1;
                    match esc {
                        b'n' => result.push(b'\n'),
                        b'r' => result.push(b'\r'),
                        b't' => result.push(b'\t'),
                        b'\\' => result.push(b'\\'),
                        b'(' => result.push(b'('),
                        b')' => result.push(b')'),
                        b'0'..=b'9' => {
                            // Octal escape (up to 3 digits)
                            let mut val = esc - b'0';
                            for _ in 0..2 {
                                if *pos < data.len() && (b'0'..=b'7').contains(&data[*pos]) {
                                    val = val.wrapping_mul(8) + (data[*pos] - b'0');
                                    *pos += 1;
                                } else {
                                    break;
                                }
                            }
                            result.push(val);
                        }
                        _ => result.push(esc),
                    }
                }
            }
            _ => result.push(b),
        }
    }

    Some(Operand::Str(result))
}

fn parse_hex_string(data: &[u8], pos: &mut usize) -> Option<Operand> {
    *pos += 1; // skip '<'
    let mut hex_chars = Vec::new();

    while *pos < data.len() {
        let b = data[*pos];
        if b == b'>' {
            *pos += 1;
            break;
        }
        if b.is_ascii_hexdigit() {
            hex_chars.push(b);
        }
        *pos += 1;
    }

    // Pad odd-length hex strings
    if hex_chars.len() % 2 != 0 {
        hex_chars.push(b'0');
    }

    let mut result = Vec::with_capacity(hex_chars.len() / 2);
    for chunk in hex_chars.chunks(2) {
        let hi = hex_val(chunk[0]);
        let lo = hex_val(chunk[1]);
        result.push((hi << 4) | lo);
    }

    Some(Operand::Str(result))
}

fn hex_val(b: u8) -> u8 {
    match b {
        b'0'..=b'9' => b - b'0',
        b'a'..=b'f' => b - b'a' + 10,
        b'A'..=b'F' => b - b'A' + 10,
        _ => 0,
    }
}

fn parse_name(data: &[u8], pos: &mut usize) -> Option<Operand> {
    *pos += 1; // skip '/'
    let start = *pos;
    while *pos < data.len() && is_name_char(data[*pos]) {
        *pos += 1;
    }
    Some(Operand::Name(data[start..*pos].to_vec()))
}

fn is_name_char(b: u8) -> bool {
    !matches!(
        b,
        b' ' | b'\t'
            | b'\n'
            | b'\r'
            | b'('
            | b')'
            | b'<'
            | b'>'
            | b'['
            | b']'
            | b'/'
            | b'%'
            | b'\0'
    )
}

fn parse_array(data: &[u8], pos: &mut usize) -> Option<Operand> {
    *pos += 1; // skip '['
    let mut items = Vec::new();
    while *pos < data.len() {
        skip_ws_and_comments(data, pos);
        if *pos >= data.len() || data[*pos] == b']' {
            *pos += 1;
            break;
        }
        if let Some(operand) = parse_operand(data, pos) {
            items.push(operand);
        }
    }
    Some(Operand::Array(items))
}

// ─── Operator execution ───

fn execute_operator(
    op: &[u8],
    operands: &[Operand],
    state: &mut TextState,
    result: &mut ContentResult,
    fonts: &HashMap<String, FontInfo>,
) {
    execute_operator_full(op, operands, state, result, fonts, &HashMap::new(), 0);
}

fn execute_operator_full(
    op: &[u8],
    operands: &[Operand],
    state: &mut TextState,
    result: &mut ContentResult,
    fonts: &HashMap<String, FontInfo>,
    xobjects: &HashMap<String, XObjectData>,
    depth: usize,
) {
    match op {
        // === Text block ===
        b"BT" => {
            state.tm = Matrix::identity();
            state.tlm = Matrix::identity();
        }
        b"ET" => {}

        // === Text positioning ===
        b"Td" => {
            if operands.len() >= 2 {
                let tx = operands[operands.len() - 2].as_f64();
                let ty = operands[operands.len() - 1].as_f64();
                // Actual pen displacement from current position to new position
                let dx = state.tlm.e + tx - state.tm.e;
                let dy = state.tlm.f + ty - state.tm.f;
                maybe_emit_space_for_move(state, result, dx, dy, state.font_size);
                let m = Matrix::new(1.0, 0.0, 0.0, 1.0, tx, ty);
                state.tlm = m.mul(&state.tlm);
                state.tm = state.tlm;
            }
        }
        b"TD" => {
            if operands.len() >= 2 {
                let tx = operands[operands.len() - 2].as_f64();
                let ty = operands[operands.len() - 1].as_f64();
                let dx = state.tlm.e + tx - state.tm.e;
                let dy = state.tlm.f + ty - state.tm.f;
                maybe_emit_space_for_move(state, result, dx, dy, state.font_size);
                state.leading = -ty;
                let m = Matrix::new(1.0, 0.0, 0.0, 1.0, tx, ty);
                state.tlm = m.mul(&state.tlm);
                state.tm = state.tlm;
            }
        }
        b"Tm" => {
            if operands.len() >= 6 {
                let a = operands[operands.len() - 6].as_f64();
                let b_ = operands[operands.len() - 5].as_f64();
                let c = operands[operands.len() - 4].as_f64();
                let d = operands[operands.len() - 3].as_f64();
                let e = operands[operands.len() - 2].as_f64();
                let f = operands[operands.len() - 1].as_f64();
                let new_tm = Matrix::new(a, b_, c, d, e, f);
                let dx = new_tm.e - state.tm.e;
                let dy = new_tm.f - state.tm.f;
                maybe_emit_space_for_move(state, result, dx, dy, state.font_size);
                state.tlm = new_tm;
                state.tm = state.tlm;
            }
        }
        b"T*" => {
            let m = Matrix::new(1.0, 0.0, 0.0, 1.0, 0.0, -state.leading);
            state.tlm = m.mul(&state.tlm);
            state.tm = state.tlm;
        }

        // === Font selection ===
        b"Tf" => {
            if operands.len() >= 2 {
                let name = operands[operands.len() - 2].as_name().to_vec();
                let size = operands[operands.len() - 1].as_f64();
                state.font_name = name;
                state.font_size = size;
            }
        }

        // === Text rendering ===
        b"Tr" => {
            if let Some(op) = operands.last() {
                state.render_mode = op.as_i64() as u8;
            }
        }
        b"Tc" => {
            if let Some(op) = operands.last() {
                state.char_spacing = op.as_f64();
            }
        }
        b"Tw" => {
            if let Some(op) = operands.last() {
                state.word_spacing = op.as_f64();
            }
        }
        b"Tz" => {
            if let Some(op) = operands.last() {
                state.h_scale = op.as_f64() / 100.0;
            }
        }
        b"TL" => {
            if let Some(op) = operands.last() {
                state.leading = op.as_f64();
            }
        }

        // === Text show operators ===
        b"Tj" => {
            if let Some(op) = operands.last() {
                let bytes = op.as_str();
                emit_string(bytes, state, result, fonts);
            }
        }
        b"'" => {
            // T' = T* + Tj
            let m = Matrix::new(1.0, 0.0, 0.0, 1.0, 0.0, -state.leading);
            state.tlm = m.mul(&state.tlm);
            state.tm = state.tlm;
            if let Some(op) = operands.last() {
                let bytes = op.as_str();
                emit_string(bytes, state, result, fonts);
            }
        }
        b"\"" => {
            // T" = set Tc, Tw, T*, Tj
            if operands.len() >= 3 {
                state.char_spacing = operands[operands.len() - 3].as_f64();
                state.word_spacing = operands[operands.len() - 2].as_f64();
                let bytes = operands[operands.len() - 1].as_str();
                let m = Matrix::new(1.0, 0.0, 0.0, 1.0, 0.0, -state.leading);
                state.tlm = m.mul(&state.tlm);
                state.tm = state.tlm;
                emit_string(bytes, state, result, fonts);
            }
        }
        b"TJ" => {
            // Array of strings and kerning values
            if let Some(Operand::Array(arr)) = operands.last() {
                for item in arr {
                    match item {
                        Operand::Str(bytes) => {
                            emit_string(bytes, state, result, fonts);
                        }
                        Operand::Real(_) | Operand::Int(_) => {
                            // Kerning: shift text position
                            let tj = item.as_f64();
                            let shift = -tj * state.font_size * state.h_scale / 1000.0;
                            let m = Matrix::new(1.0, 0.0, 0.0, 1.0, shift, 0.0);
                            state.tm = m.mul(&state.tm);

                            // Emit a synthetic space when the kerning value
                            // is in the word-spacing range (about 0.13–0.6 em
                            // for typical Latin text). Outside this range:
                            //  - Magnitude too small (< 0.13 em): kerning
                            //    within a word, no space.
                            //  - Magnitude too large (> 0.6 em): heading or
                            //    paragraph indent — PyMuPDF does NOT emit a
                            //    space here, e.g. "[(I.)]TJ [-1000(Intro)]TJ"
                            //    stays as "I.Introduction" in pm output.
                            let fs = (state.font_size / 12.0).max(0.5);
                            let min_sep = -150.0 * fs;
                            let max_sep = -700.0 * fs;
                            if tj < min_sep && tj > max_sep && !result.chars.is_empty() {
                                emit_space(state, result);
                            }
                        }
                        _ => {}
                    }
                }
            }
        }

        // === Graphics state ===
        b"q" => state.save_gs(),
        b"Q" => state.restore_gs(),
        b"cm" => {
            if operands.len() >= 6 {
                let a = operands[operands.len() - 6].as_f64();
                let b_ = operands[operands.len() - 5].as_f64();
                let c = operands[operands.len() - 4].as_f64();
                let d = operands[operands.len() - 3].as_f64();
                let e = operands[operands.len() - 2].as_f64();
                let f = operands[operands.len() - 1].as_f64();
                let m = Matrix::new(a, b_, c, d, e, f);
                state.ctm = m.mul(&state.ctm);
            }
        }

        // === Image / Form XObject capture ===
        b"Do" => {
            if let Some(op) = operands.last() {
                let name = String::from_utf8_lossy(op.as_name()).to_string();

                // Check if this is a Form XObject
                if let Some(XObjectData::Form {
                    data,
                    matrix,
                    bbox: _,
                    fonts: form_fonts,
                }) = xobjects.get(&name)
                {
                    if depth < MAX_FORM_DEPTH {
                        // Save graphics state
                        state.save_gs();

                        // Apply Form matrix: concatenate with CTM
                        let form_matrix = Matrix::new(
                            matrix[0], matrix[1], matrix[2], matrix[3], matrix[4], matrix[5],
                        );
                        state.ctm = form_matrix.mul(&state.ctm);

                        // Merge form's own fonts with parent fonts (form fonts take priority)
                        let mut merged_fonts = fonts.clone();
                        for (k, v) in form_fonts {
                            merged_fonts.entry(k.clone()).or_insert_with(|| v.clone());
                        }

                        // Recursively scan the Form's content stream with merged fonts
                        let form_result =
                            scan_content_stream_full(data, &merged_fonts, xobjects, depth + 1);

                        // Merge results (adjust coordinates by current CTM)
                        for mut ch in form_result.chars {
                            let (x, y) = state.ctm.apply(ch.bbox[0], ch.bbox[1]);
                            let (x1, y1) = state.ctm.apply(ch.bbox[2], ch.bbox[3]);
                            ch.bbox = [x, y, x1, y1];
                            result.chars.push(ch);
                        }
                        for mut img in form_result.images {
                            let (x, y) = state.ctm.apply(img.bbox[0], img.bbox[1]);
                            let (x1, y1) = state.ctm.apply(img.bbox[2], img.bbox[3]);
                            img.bbox = [x, y, x1, y1];
                            result.images.push(img);
                        }

                        // Merge diagnostics counters from the recursive scan
                        // so Type3 / undecoded / inline-image counts bubble
                        // up to the caller.
                        result.type3_char_count += form_result.type3_char_count;
                        result.undecoded_byte_count += form_result.undecoded_byte_count;
                        result.inline_image_count += form_result.inline_image_count;

                        // Restore graphics state
                        state.restore_gs();
                    }
                } else {
                    // Image reference
                    let (x0, y0) = state.ctm.apply(0.0, 0.0);
                    let (x1, y1) = state.ctm.apply(1.0, 1.0);
                    result.images.push(ImageRef {
                        name,
                        bbox: [x0.min(x1), y0.min(y1), x0.max(x1), y0.max(y1)],
                        obj_ref: None,
                    });
                }
            }
        }

        // === Color (simplified) ===
        b"rg" => {
            if operands.len() >= 3 {
                let r = (operands[operands.len() - 3].as_f64() * 255.0) as u32;
                let g = (operands[operands.len() - 2].as_f64() * 255.0) as u32;
                let b_ = (operands[operands.len() - 1].as_f64() * 255.0) as u32;
                state.fill_color = (r << 16) | (g << 8) | b_;
            }
        }
        b"g" => {
            if let Some(op) = operands.last() {
                let gray = (op.as_f64() * 255.0) as u32;
                state.fill_color = (gray << 16) | (gray << 8) | gray;
            }
        }
        b"RG" | b"G" | b"CS" | b"cs" | b"SC" | b"sc" | b"SCN" | b"scn" => {
            // Ignore stroke/fill color space changes for now
        }

        // === Ignore all other operators ===
        _ => {}
    }
}

// ─── String emission ───

fn emit_string(
    bytes: &[u8],
    state: &mut TextState,
    result: &mut ContentResult,
    fonts: &HashMap<String, FontInfo>,
) {
    let font_size = state.font_size;
    let h_scale = state.h_scale;
    let font_name = String::from_utf8_lossy(&state.font_name).to_string();

    // Look up font info for proper decoding and width
    let font_info = fonts.get(&font_name);
    let char_width_default = font_size * 0.6 * h_scale;

    // For Type0 (CID) fonts, bytes may be 2-byte codes
    let is_cid = font_info.is_some_and(|f| f.is_type0);
    let mut i = 0;

    while i < bytes.len() {
        let (decoded_chars, code_bytes) = if is_cid && i + 1 < bytes.len() {
            // 2-byte CID code
            let decoded = font_info.map_or_else(
                || vec!['\u{FFFD}'],
                |f| f.decode_chars(&[bytes[i], bytes[i + 1]]),
            );
            (decoded, vec![bytes[i], bytes[i + 1]])
        } else {
            // Single byte
            let decoded = font_info.map_or_else(
                || {
                    if bytes[i] < 128 {
                        vec![bytes[i] as char]
                    } else {
                        vec!['\u{FFFD}']
                    }
                },
                |f| f.decode_chars(&[bytes[i]]),
            );
            (decoded, vec![bytes[i]])
        };

        i += code_bytes.len();

        // Calculate character position. For rotated/sheared text the full
        // TRM = CTM × Tm is non-axis-aligned (b != 0 or c != 0); we compute
        // the device-space bbox via 4-corner transform in that case.
        // Otherwise the cheaper text-space offset path is used (preserves
        // historical behavior and baseline accuracy). The check covers
        // rotation in either Tm (text operator) or CTM (cm operator / Form
        // XObject matrix).
        let trm = state.ctm.mul(&state.tm);
        let is_rotated = trm.b.abs() > 1e-6 || trm.c.abs() > 1e-6;
        let (x, y) = if is_rotated {
            trm.apply(0.0, 0.0)
        } else {
            let (cx, cy) = state.ctm.apply(0.0, 0.0);
            let (tx, ty) = state.tm.apply(0.0, 0.0);
            (cx + tx, cy + ty)
        };

        // Character width from font widths or estimate
        let code_val = if code_bytes.len() == 2 {
            ((code_bytes[0] as u32) << 8) | (code_bytes[1] as u32)
        } else {
            code_bytes[0] as u32
        };
        let char_width = font_info
            .map(|f| {
                // For Type0 (CID) fonts, use CIDFont descendant's /W array
                if let Some(cid_font) = &f.cid_font {
                    cid_font.cid_width(code_val) * font_size / 1000.0 * h_scale
                } else {
                    f.char_width(code_val) * font_size / 1000.0 * h_scale
                }
            })
            .unwrap_or(char_width_default);
        let char_height = font_size;

        // For rotated text in default-width fallback mode (standard 14 fonts
        // without /Widths — common for arXiv-style Times-Roman sidebars),
        // char_width == font_size, which makes a 40-char sidebar span 2× the
        // page height and get dropped by the reading_order_sort margin
        // filter. Substitute a realistic Latin-text average (0.5em) for the
        // advance and per-char unit width so the rotated block stays inside
        // the page rect. This branch is ONLY taken for rotated chars in
        // fallback mode — non-rotated extraction is byte-for-byte unchanged.
        let using_default_width = font_info.is_some_and(|f| f.widths.is_empty())
            && font_info.is_some_and(|f| f.cid_font.is_none());
        let n = decoded_chars.len();
        let (unit_w, advance_w) = if is_rotated && using_default_width {
            let est = font_size * 0.5 * h_scale;
            (est / n.max(1) as f64, est)
        } else {
            let unit_w = char_width / n.max(1) as f64;
            (unit_w, char_width)
        };

        // Diagnostics: count chars emitted under Type 3 fonts and bytes
        // that decoded to U+FFFD. The diagnostics counters live on
        // ContentResult so callers can surface "N items couldn't be
        // faithfully extracted" to the user without re-running the scan.
        let is_type3_font = font_info.is_some_and(|f| f.is_type3);
        if is_type3_font {
            result.type3_char_count += decoded_chars.len();
        }
        result.undecoded_byte_count += decoded_chars.iter().filter(|c| **c == '\u{FFFD}').count();

        // Decode may yield multiple chars (e.g. TeX ligatures: byte 0x0C →
        // "fi"). Push each with proportional width so the rest of the
        // pipeline sees the same number of chars PyMuPDF does.
        for (k, c) in decoded_chars.into_iter().enumerate() {
            let off = unit_w * k as f64;
            let bbox = if is_rotated {
                // Transform all 4 corners of the char cell through TRM and
                // take the AABB. Adobe AFM metrics aren't embedded, so
                // standard-font widths (Times/Helvetica) may be off — we
                // accept that as the cost of recovering rotated text.
                let (ax, ay) = trm.apply(off, 0.0);
                let (bx, by) = trm.apply(off + unit_w, 0.0);
                let (cx2, cy2) = trm.apply(off, char_height);
                let (dx, dy) = trm.apply(off + unit_w, char_height);
                [
                    ax.min(bx).min(cx2).min(dx),
                    ay.min(by).min(cy2).min(dy),
                    ax.max(bx).max(cx2).max(dx),
                    ay.max(by).max(cy2).max(dy),
                ]
            } else {
                [x + off, y, x + off + unit_w, y + char_height]
            };
            result.chars.push(CharInfo {
                c,
                bbox,
                size: font_size,
                rotated: is_rotated,
            });
        }

        // Advance text matrix along the text direction. For rotated text
        // the advance happens along (a,b) in text space; for axis-aligned
        // text the historical pre-multiply is exact.
        let advance = advance_w + state.char_spacing;
        let advance = if code_bytes.len() == 1 && code_bytes[0] == b' ' {
            advance + state.word_spacing
        } else {
            advance
        };
        if is_rotated {
            let mag = (state.tm.a.powi(2) + state.tm.b.powi(2)).sqrt().max(1e-9);
            state.tm.e += advance * state.tm.a / mag;
            state.tm.f += advance * state.tm.b / mag;
        } else {
            let m = Matrix::new(1.0, 0.0, 0.0, 1.0, advance, 0.0);
            state.tm = m.mul(&state.tm);
        }
    }
}

/// Emit a synthetic space character at the current text position.
fn emit_space(state: &TextState, result: &mut ContentResult) {
    let (cx, cy) = state.ctm.apply(0.0, 0.0);
    let (tx, ty) = state.tm.apply(0.0, 0.0);
    let x = cx + tx;
    let y = cy + ty;
    let w = state.font_size * 0.25;
    result.chars.push(CharInfo {
        c: ' ',
        bbox: [x, y, x + w, y + state.font_size],
        size: state.font_size,
        rotated: false,
    });
}

/// Insert a space when a text-positioning operator (Td/TD/Tm) moves the pen
/// horizontally on the same line by a word-space-sized gap.
///
/// PyMuPDF inserts a space at these intra-line pen jumps (common when
/// switching fonts mid-sentence, e.g. an italic "et al." after a regular-font
/// author name). The tight 0.15–1.0em band excludes both tiny pen adjustments
/// (same-word continuation, hyphenation) and large math-formula repositioning.
///
/// `dx`/`dy` are the displacement in text-space units (points). `font_size`
/// is the current font size for em conversion.
fn maybe_emit_space_for_move(
    state: &TextState,
    result: &mut ContentResult,
    dx: f64,
    dy: f64,
    font_size: f64,
) {
    if font_size <= 0.0 {
        return;
    }
    // Must be on the same line (small vertical drift only)
    if dy.abs() > font_size * 0.3 {
        return;
    }
    let dx_em = dx / font_size;
    // Tight band: word-space sized only
    if !(0.15..=1.0).contains(&dx_em) {
        return;
    }
    // Don't emit if the last char is already whitespace or nothing was drawn
    let last = match result.chars.last() {
        Some(c) => c,
        None => return,
    };
    if last.c.is_whitespace() {
        return;
    }
    emit_space(state, result);
}

// ─── Tests ───

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simple_text() {
        let content = b"BT /F1 12 Tf 100 700 Td (Hello) Tj ET";
        let result = scan_content_stream(content);
        assert_eq!(result.chars.len(), 5);
        assert_eq!(result.chars[0].c, 'H');
        assert_eq!(result.chars[4].c, 'o');
    }

    #[test]
    fn test_tj_array() {
        let content = b"BT /F1 12 Tf 100 700 Td [(Hello) -100 (World)] TJ ET";
        let result = scan_content_stream(content);
        assert_eq!(result.chars.len(), 10); // Hello + World
        assert_eq!(result.chars[5].c, 'W');
    }

    #[test]
    fn test_multiple_text_blocks() {
        let content =
            b"BT /F1 12 Tf 100 700 Td (Line1) Tj ET BT /F1 12 Tf 100 680 Td (Line2) Tj ET";
        let result = scan_content_stream(content);
        assert_eq!(result.chars.len(), 10); // Line1 + Line2
    }

    #[test]
    fn test_hex_string() {
        let content = b"BT /F1 12 Tf 100 700 Td <48656C6C6F> Tj ET";
        let result = scan_content_stream(content);
        assert_eq!(result.chars.len(), 5);
        assert_eq!(result.chars[0].c, 'H');
    }

    #[test]
    fn test_image_do() {
        let content = b"q 100 0 0 100 0 0 cm /Im1 Do Q";
        let result = scan_content_stream(content);
        assert_eq!(result.images.len(), 1);
        assert_eq!(result.images[0].name, "Im1");
    }

    #[test]
    fn test_inline_image_basic() {
        // BI ... ID <single ws> <data> EI
        // Image data is 6 bytes of dummy pixel data. The "EI" inside the
        // data should NOT be recognized as a terminator (no surrounding ws).
        let content = b"q 100 0 0 100 0 0 cm BI /W 2 /H 1 /BPC 8 /CS /G ID \xff\xfeEI\x00\x01 EI Q";
        let result = scan_content_stream(content);
        assert_eq!(
            result.inline_image_count, 1,
            "should detect one inline image"
        );
        assert_eq!(result.images.len(), 1);
        assert_eq!(result.images[0].name, "inline");
        // CTM scales the unit square by 100, so bbox should be ~[0,0,100,100]
        let bbox = result.images[0].bbox;
        assert!((bbox[2] - bbox[0] - 100.0).abs() < 1.0, "width: {bbox:?}");
        assert!((bbox[3] - bbox[1] - 100.0).abs() < 1.0, "height: {bbox:?}");
    }

    #[test]
    fn test_inline_image_ei_in_data_with_whitespace() {
        // The "EI" in data IS treated as terminator when surrounded by ws.
        // This is the spec-correct behavior and matches MuPDF.
        let content = b"BI /W 2 /H 1 /BPC 8 /CS /G ID \xff\xfe EI garbage";
        let result = scan_content_stream(content);
        assert_eq!(result.inline_image_count, 1);
    }

    #[test]
    fn test_inline_image_missing_id() {
        // Malformed: no ID operator → no image pushed, no infinite loop.
        let content = b"BI /W 2 /H 1 no_id_here BT /F1 12 Tf (text) Tj ET";
        let result = scan_content_stream(content);
        assert_eq!(result.inline_image_count, 0);
        assert_eq!(result.images.len(), 0);
    }

    #[test]
    fn test_inline_image_missing_ei() {
        // Malformed: no EI terminator → bail without producing image.
        let content = b"BI /W 2 /H 1 /BPC 8 ID \xff\xfe\xff\xfe\xff";
        let result = scan_content_stream(content);
        assert_eq!(result.inline_image_count, 0);
    }

    #[test]
    fn test_multiple_inline_images() {
        let content = b"q 50 0 0 50 0 0 cm BI /W 1 /H 1 /BPC 8 /CS /G ID \xff EI Q q 50 0 0 50 100 0 cm BI /W 1 /H 1 /BPC 8 /CS /G ID \xfe EI Q";
        let result = scan_content_stream(content);
        assert_eq!(result.inline_image_count, 2);
        assert_eq!(result.images.len(), 2);
    }

    #[test]
    fn test_td_positioning() {
        let content = b"BT /F1 12 Tf 100 700 Td (A) Tj 0 -14 Td (B) Tj ET";
        let result = scan_content_stream(content);
        assert_eq!(result.chars.len(), 2);
        // Second char should be lower (smaller y)
        assert!(result.chars[1].bbox[1] < result.chars[0].bbox[1]);
    }

    #[test]
    fn test_tm_matrix() {
        let content = b"BT /F1 12 Tf 1 0 0 1 200 500 Tm (X) Tj ET";
        let result = scan_content_stream(content);
        assert_eq!(result.chars.len(), 1);
        assert!((result.chars[0].bbox[0] - 200.0).abs() < 1.0);
    }

    #[test]
    fn test_kerning_tj() {
        let content = b"BT /F1 12 Tf 100 700 Td [(A) -200 (B)] TJ ET";
        let result = scan_content_stream(content);
        // -200 kerning triggers space insertion (threshold: -150)
        assert_eq!(result.chars.len(), 3); // A, space, B
        assert_eq!(result.chars[0].c, 'A');
        assert_eq!(result.chars[1].c, ' ');
        assert_eq!(result.chars[2].c, 'B');
    }

    #[test]
    fn test_escape_sequences() {
        let content = b"BT /F1 12 Tf 100 700 Td (Hello\\nWorld) Tj ET";
        let result = scan_content_stream(content);
        assert_eq!(result.chars.len(), 11); // Hello\nWorld
    }

    #[test]
    fn test_empty_content() {
        let content = b"";
        let result = scan_content_stream(content);
        assert_eq!(result.chars.len(), 0);
    }
}
