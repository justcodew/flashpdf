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
                let m = Matrix::new(1.0, 0.0, 0.0, 1.0, tx, ty);
                state.tlm = m.mul(&state.tlm);
                state.tm = state.tlm;
            }
        }
        b"TD" => {
            if operands.len() >= 2 {
                let tx = operands[operands.len() - 2].as_f64();
                let ty = operands[operands.len() - 1].as_f64();
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
                state.tlm = Matrix::new(a, b_, c, d, e, f);
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

                            // Large kerning values indicate word boundaries.
                            // Threshold adapts to font size: smaller fonts need smaller gaps.
                            let threshold = -150.0 * (state.font_size / 12.0).max(0.5);
                            if tj < threshold && !result.chars.is_empty() {
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
        let (c, code_bytes) = if is_cid && i + 1 < bytes.len() {
            // 2-byte CID code
            let _code = ((bytes[i] as u32) << 8) | (bytes[i + 1] as u32);
            let decoded =
                font_info.map_or('\u{FFFD}', |f| f.decode_char(&[bytes[i], bytes[i + 1]]));
            (decoded, vec![bytes[i], bytes[i + 1]])
        } else {
            // Single byte
            let decoded = font_info.map_or_else(
                || {
                    if bytes[i] < 128 {
                        bytes[i] as char
                    } else {
                        '\u{FFFD}'
                    }
                },
                |f| f.decode_char(&[bytes[i]]),
            );
            (decoded, vec![bytes[i]])
        };

        i += code_bytes.len();

        // Calculate character position
        let (cx, cy) = state.ctm.apply(0.0, 0.0);
        let (tx, ty) = state.tm.apply(0.0, 0.0);
        let x = cx + tx;
        let y = cy + ty;

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

        // Expand ligatures: ﬁ → fi, ﬂ → fl, ﬃ → ffi, ﬄ → ffl
        match c {
            '\u{FB01}' => {
                // fi ligature
                let half = char_width * 0.5;
                result.chars.push(CharInfo {
                    c: 'f',
                    bbox: [x, y, x + half, y + char_height],
                });
                result.chars.push(CharInfo {
                    c: 'i',
                    bbox: [x + half, y, x + char_width, y + char_height],
                });
            }
            '\u{FB02}' => {
                // fl ligature
                let half = char_width * 0.5;
                result.chars.push(CharInfo {
                    c: 'f',
                    bbox: [x, y, x + half, y + char_height],
                });
                result.chars.push(CharInfo {
                    c: 'l',
                    bbox: [x + half, y, x + char_width, y + char_height],
                });
            }
            '\u{FB03}' => {
                // ffi ligature
                let third = char_width / 3.0;
                result.chars.push(CharInfo {
                    c: 'f',
                    bbox: [x, y, x + third, y + char_height],
                });
                result.chars.push(CharInfo {
                    c: 'f',
                    bbox: [x + third, y, x + third * 2.0, y + char_height],
                });
                result.chars.push(CharInfo {
                    c: 'i',
                    bbox: [x + third * 2.0, y, x + char_width, y + char_height],
                });
            }
            '\u{FB04}' => {
                // ffl ligature
                let third = char_width / 3.0;
                result.chars.push(CharInfo {
                    c: 'f',
                    bbox: [x, y, x + third, y + char_height],
                });
                result.chars.push(CharInfo {
                    c: 'f',
                    bbox: [x + third, y, x + third * 2.0, y + char_height],
                });
                result.chars.push(CharInfo {
                    c: 'l',
                    bbox: [x + third * 2.0, y, x + char_width, y + char_height],
                });
            }
            '\u{FB05}' | '\u{FB06}' => {
                // st ligature
                let half = char_width * 0.5;
                result.chars.push(CharInfo {
                    c: 's',
                    bbox: [x, y, x + half, y + char_height],
                });
                result.chars.push(CharInfo {
                    c: 't',
                    bbox: [x + half, y, x + char_width, y + char_height],
                });
            }
            _ => {
                result.chars.push(CharInfo {
                    c,
                    bbox: [x, y, x + char_width, y + char_height],
                });
            }
        }

        // Advance text matrix
        let advance = char_width + state.char_spacing;
        if code_bytes.len() == 1 && code_bytes[0] == b' ' {
            let total_advance = advance + state.word_spacing;
            let m = Matrix::new(1.0, 0.0, 0.0, 1.0, total_advance, 0.0);
            state.tm = m.mul(&state.tm);
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
    });
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
