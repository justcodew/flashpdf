use crate::types::{ObjectId, PdfObject};

/// Error type for PDF parsing operations.
#[derive(Debug, Clone)]
pub enum ParseError {
    /// Unexpected end of input
    UnexpectedEof,
    /// Unexpected byte encountered
    UnexpectedByte(u8),
    /// Invalid number format
    InvalidNumber,
    /// Invalid hex string
    InvalidHexString,
    /// Invalid name escape (#XX)
    InvalidNameEscape,
    /// Unbalanced parentheses in string
    UnbalancedString,
    /// Stream missing required /Length
    StreamMissingLength,
    /// Generic message
    Message(&'static str),
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ParseError::UnexpectedEof => write!(f, "unexpected end of input"),
            ParseError::UnexpectedByte(b) => {
                write!(f, "unexpected byte: 0x{:02x} ('{}')", b, *b as char)
            }
            ParseError::InvalidNumber => write!(f, "invalid number format"),
            ParseError::InvalidHexString => write!(f, "invalid hex string"),
            ParseError::InvalidNameEscape => write!(f, "invalid #XX escape in name"),
            ParseError::UnbalancedString => write!(f, "unbalanced parentheses in string"),
            ParseError::StreamMissingLength => write!(f, "stream missing /Length"),
            ParseError::Message(m) => write!(f, "{}", m),
        }
    }
}

impl std::error::Error for ParseError {}

pub type ParseResult<T> = Result<T, ParseError>;

// ─── Byte classification helpers ───

#[inline]
fn is_whitespace(b: u8) -> bool {
    matches!(b, b'\0' | b'\t' | b'\n' | b'\r' | b' ')
}

#[inline]
fn is_delimiter(b: u8) -> bool {
    matches!(
        b,
        b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%'
    )
}

#[inline]
fn is_regular(b: u8) -> bool {
    !is_whitespace(b) && !is_delimiter(b)
}

// ─── Cursor: a thin wrapper over &[u8] with position tracking ───

pub struct Cursor<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    #[inline]
    pub fn remaining(&self) -> &'a [u8] {
        &self.data[self.pos..]
    }

    #[inline]
    pub fn pos(&self) -> usize {
        self.pos
    }

    #[inline]
    pub fn peek(&self) -> Option<u8> {
        self.data.get(self.pos).copied()
    }

    #[inline]
    pub fn advance(&mut self, n: usize) {
        self.pos += n;
    }

    #[inline]
    pub fn consume_byte(&mut self) -> ParseResult<u8> {
        let b = self.peek().ok_or(ParseError::UnexpectedEof)?;
        self.pos += 1;
        Ok(b)
    }

    /// Skip whitespace and % comments.
    pub fn skip_ws(&mut self) {
        while let Some(b) = self.peek() {
            if is_whitespace(b) {
                self.pos += 1;
            } else if b == b'%' {
                // comment: skip to end of line
                self.pos += 1;
                while let Some(b) = self.peek() {
                    self.pos += 1;
                    if b == b'\n' || b == b'\r' {
                        break;
                    }
                }
            } else {
                break;
            }
        }
    }

    /// Check if the next non-ws byte matches. Does NOT consume.
    pub fn peek_after_ws(&mut self) -> Option<u8> {
        let saved = self.pos;
        self.skip_ws();
        let b = self.peek();
        self.pos = saved;
        b
    }

    /// Get a slice of the input from `start` to current position.
    #[inline]
    pub fn slice_from(&self, start: usize) -> &'a [u8] {
        &self.data[start..self.pos]
    }
}

// ─── Top-level parse entry ───

/// Parse a single PDF object from the cursor.
/// After parsing, the cursor is positioned right after the object.
pub fn parse_object<'a>(cur: &mut Cursor<'a>) -> ParseResult<PdfObject<'a>> {
    cur.skip_ws();
    let b = cur.peek().ok_or(ParseError::UnexpectedEof)?;

    match b {
        b'<' => {
            // could be hex string <...> or dict << ... >>
            if cur.remaining().len() >= 2 && cur.remaining()[1] == b'<' {
                parse_dict(cur)
            } else {
                parse_hex_string(cur)
            }
        }
        b'>' => {
            // should not happen at top level; caller handles >>
            Err(ParseError::UnexpectedByte(b'>'))
        }
        b'(' => parse_string(cur),
        b'/' => parse_name(cur),
        b'[' => parse_array(cur),
        b']' => {
            // end of array; caller handles this
            Err(ParseError::UnexpectedByte(b']'))
        }
        b't' => parse_literal(cur, b"true", PdfObject::Bool(true)),
        b'f' => parse_literal(cur, b"false", PdfObject::Bool(false)),
        b'n' => parse_literal(cur, b"null", PdfObject::Null),
        b'0'..=b'9' | b'+' | b'-' | b'.' => parse_number_or_ref(cur),
        _ => Err(ParseError::UnexpectedByte(b)),
    }
}

fn parse_literal<'a>(
    cur: &mut Cursor<'a>,
    expected: &[u8],
    obj: PdfObject<'a>,
) -> ParseResult<PdfObject<'a>> {
    let remaining = cur.remaining();
    if remaining.len() >= expected.len() && &remaining[..expected.len()] == expected {
        // Check that the next char after the literal is a delimiter or whitespace
        let after = remaining.get(expected.len());
        if after.is_none_or(|b| !is_regular(*b)) {
            cur.advance(expected.len());
            return Ok(obj);
        }
    }
    Err(ParseError::UnexpectedByte(remaining[0]))
}

// ─── Number parsing ───

/// Parse a number. If it looks like `N G R`, parse as indirect reference.
fn parse_number_or_ref<'a>(cur: &mut Cursor<'a>) -> ParseResult<PdfObject<'a>> {
    // First, scan ahead to determine if this is a real number (contains '.' or [eE])
    // before hitting whitespace or end-of-tokens.
    let mut has_dot = false;
    let mut has_exp = false;
    let mut scan = cur.pos();
    let data = cur.data;

    // Skip optional sign
    if scan < data.len() && (data[scan] == b'+' || data[scan] == b'-') {
        scan += 1;
    }
    // Skip digits
    while scan < data.len() && data[scan].is_ascii_digit() {
        scan += 1;
    }
    // Check for dot
    if scan < data.len() && data[scan] == b'.' {
        // Make sure next char is digit or non-regular (so it's a real, not a ref separator)
        let next = data.get(scan + 1);
        if next.is_none_or(|c: &u8| !is_regular(*c) || c.is_ascii_digit()) {
            has_dot = true;
            scan += 1;
            while scan < data.len() && data[scan].is_ascii_digit() {
                scan += 1;
            }
        }
    }
    // Check for exponent
    if scan < data.len() && (data[scan] == b'e' || data[scan] == b'E') {
        has_exp = true;
    }

    // If it looks like a real number, parse it as such
    if has_dot || has_exp {
        return parse_real_raw(cur).map(PdfObject::Real);
    }

    // Otherwise, try integer + optional reference pattern
    let first = parse_integer_raw(cur)?;
    let after_first = cur.pos();

    // Skip whitespace and check if this is a reference: N G R
    cur.skip_ws();
    if let Some(b) = cur.peek() {
        if b.is_ascii_digit() || b == b'+' || b == b'-' {
            // Could be gen number
            let second = parse_integer_raw(cur)?;
            cur.skip_ws();
            if let Some(b) = cur.peek() {
                if b == b'R' {
                    // Check that R is followed by delimiter/whitespace/end
                    let after_r = cur.pos() + 1;
                    if after_r >= data.len() || !is_regular(data[after_r]) {
                        cur.advance(1); // consume 'R'
                        return Ok(PdfObject::Ref(ObjectId::new(first as u32, second as u16)));
                    }
                }
            }
            // Not a reference — rewind
            cur.pos = after_first;
        }
    }

    Ok(PdfObject::Integer(first))
}

/// Raw integer parse: optional sign, then digits. Returns i64.
fn parse_integer_raw(cur: &mut Cursor<'_>) -> ParseResult<i64> {
    let mut negative = false;
    let start = cur.pos();

    match cur.peek() {
        Some(b'+') => {
            cur.advance(1);
        }
        Some(b'-') => {
            negative = true;
            cur.advance(1);
        }
        _ => {}
    }

    let digits_start = cur.pos();
    while let Some(b) = cur.peek() {
        if b.is_ascii_digit() {
            cur.advance(1);
        } else {
            break;
        }
    }

    if cur.pos() == digits_start {
        cur.pos = start; // rewind
        return Err(ParseError::InvalidNumber);
    }

    let s = std::str::from_utf8(&cur.data[digits_start..cur.pos()]).unwrap_or("0");
    let n: i64 = s.parse().unwrap_or(0);
    Ok(if negative { -n } else { n })
}

/// Raw real number parse: optional sign, digits, '.', digits.
fn parse_real_raw(cur: &mut Cursor<'_>) -> ParseResult<f64> {
    let start = cur.pos();

    // sign
    if let Some(b'+' | b'-') = cur.peek() {
        cur.advance(1);
    }

    // digits before dot
    while let Some(b) = cur.peek() {
        if b.is_ascii_digit() {
            cur.advance(1);
        } else {
            break;
        }
    }

    // dot
    if cur.peek() == Some(b'.') {
        cur.advance(1);
        // digits after dot
        while let Some(b) = cur.peek() {
            if b.is_ascii_digit() {
                cur.advance(1);
            } else {
                break;
            }
        }
    }

    // exponent (e/E)
    if let Some(b'e' | b'E') = cur.peek() {
        cur.advance(1);
        if let Some(b'+' | b'-') = cur.peek() {
            cur.advance(1);
        }
        while let Some(b) = cur.peek() {
            if b.is_ascii_digit() {
                cur.advance(1);
            } else {
                break;
            }
        }
    }

    let s = std::str::from_utf8(&cur.data[start..cur.pos()]).unwrap_or("0");
    fast_float2::parse(s).map_err(|_| ParseError::InvalidNumber)
}

// ─── String literal (...) ───

fn parse_string<'a>(cur: &mut Cursor<'a>) -> ParseResult<PdfObject<'a>> {
    cur.advance(1); // consume opening '('
    let start = cur.pos();
    let mut depth = 1u32;

    while depth > 0 {
        let b = cur.consume_byte()?;
        match b {
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    // The string content is from start to pos-1 (before the closing ')')
                    return Ok(PdfObject::String(&cur.data[start..cur.pos() - 1]));
                }
            }
            b'\\' => {
                // escape: skip next byte
                cur.consume_byte()?;
            }
            _ => {}
        }
    }

    Err(ParseError::UnbalancedString)
}

// ─── Hex string <...> ───

fn parse_hex_string<'a>(cur: &mut Cursor<'a>) -> ParseResult<PdfObject<'a>> {
    cur.advance(1); // consume '<'
    let start = cur.pos();

    loop {
        let b = cur.peek().ok_or(ParseError::UnexpectedEof)?;
        if b == b'>' {
            cur.advance(1);
            return Ok(PdfObject::HexString(&cur.data[start..cur.pos() - 1]));
        }
        if is_whitespace(b) {
            cur.advance(1);
        } else if b.is_ascii_digit() || (b'a'..=b'f').contains(&b) || (b'A'..=b'F').contains(&b) {
            cur.advance(1);
        } else {
            return Err(ParseError::InvalidHexString);
        }
    }
}

// ─── Name /Name ───

fn parse_name<'a>(cur: &mut Cursor<'a>) -> ParseResult<PdfObject<'a>> {
    cur.advance(1); // consume '/'
    let start = cur.pos();

    // We need to handle #XX escapes. Two passes:
    // 1. Scan to find the end of the name
    // 2. Decode #XX in-place (or just return raw slice if no escapes)

    let mut has_escape = false;
    while let Some(b) = cur.peek() {
        if is_regular(b) {
            if b == b'#' {
                has_escape = true;
            }
            cur.advance(1);
        } else {
            break;
        }
    }

    let raw = &cur.data[start..cur.pos()];

    if !has_escape {
        return Ok(PdfObject::Name(raw));
    }

    // Decode #XX escapes into a temporary buffer
    let mut decoded = Vec::with_capacity(raw.len());
    let mut i = 0;
    while i < raw.len() {
        if raw[i] == b'#' && i + 2 < raw.len() {
            let hi = hex_val(raw[i + 1]).ok_or(ParseError::InvalidNameEscape)?;
            let lo = hex_val(raw[i + 2]).ok_or(ParseError::InvalidNameEscape)?;
            decoded.push((hi << 4) | lo);
            i += 3;
        } else {
            decoded.push(raw[i]);
            i += 1;
        }
    }

    // We leak the decoded bytes to get a &'a [u8] lifetime.
    // This is acceptable because PDF names are small and few.
    let leaked: &'static [u8] = Box::leak(decoded.into_boxed_slice());
    Ok(PdfObject::Name(leaked))
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

// ─── Array [...] ───

fn parse_array<'a>(cur: &mut Cursor<'a>) -> ParseResult<PdfObject<'a>> {
    cur.advance(1); // consume '['
    let mut items = Vec::new();

    loop {
        cur.skip_ws();
        let b = cur.peek().ok_or(ParseError::UnexpectedEof)?;
        if b == b']' {
            cur.advance(1);
            return Ok(PdfObject::Array(items));
        }
        items.push(parse_object(cur)?);
    }
}

// ─── Dictionary << ... >> ───

fn parse_dict<'a>(cur: &mut Cursor<'a>) -> ParseResult<PdfObject<'a>> {
    cur.advance(2); // consume '<<'
    let mut entries: Vec<(&'a [u8], PdfObject<'a>)> = Vec::new();

    loop {
        cur.skip_ws();
        let remaining = cur.remaining();
        if remaining.len() >= 2 && remaining[0] == b'>' && remaining[1] == b'>' {
            cur.advance(2);
            // Check if this dict is followed by "stream" → it's a stream object
            let saved = cur.pos();
            cur.skip_ws();
            if cur.remaining().starts_with(b"stream") {
                // Don't consume stream here; the caller (or a dedicated stream parser) handles it.
                // For now, just rewind and return the dict. Stream parsing is a separate concern.
                cur.pos = saved;
            }
            return Ok(PdfObject::Dict(entries));
        }

        // Key must be a name
        cur.skip_ws();
        let key_byte = cur.peek().ok_or(ParseError::UnexpectedEof)?;
        if key_byte != b'/' {
            return Err(ParseError::UnexpectedByte(key_byte));
        }
        let key_obj = parse_name(cur)?;
        let key = match key_obj {
            PdfObject::Name(k) => k,
            _ => unreachable!(),
        };

        // Value
        cur.skip_ws();
        let value = parse_object(cur)?;

        entries.push((key, value));
    }
}

/// Parse a stream object. The dict must already be parsed.
/// The cursor should be positioned right after the dict's `>>`.
/// This function expects `stream\r?\n` to follow.
pub fn parse_stream<'a>(
    cur: &mut Cursor<'a>,
    dict: Vec<(&'a [u8], PdfObject<'a>)>,
) -> ParseResult<PdfObject<'a>> {
    cur.skip_ws();

    // Expect "stream" keyword
    if !cur.remaining().starts_with(b"stream") {
        return Err(ParseError::Message("expected 'stream' keyword"));
    }
    cur.advance(6);

    // After "stream": either \r\n or \n
    if cur.peek() == Some(b'\r') {
        cur.advance(1);
    }
    if cur.peek() == Some(b'\n') {
        cur.advance(1);
    }

    let data_start = cur.pos();

    // Find "endstream" — look for it after the stream data.
    // We need /Length to know exact size, but as fallback, scan for "endstream".
    let length = find_length_in_dict(&dict);

    if let Some(len) = length {
        let len = len as usize;
        if data_start + len > cur.data.len() {
            return Err(ParseError::UnexpectedEof);
        }
        let data = &cur.data[data_start..data_start + len];
        cur.pos = data_start + len;
        // skip to endstream
        cur.skip_ws();
        if cur.remaining().starts_with(b"endstream") {
            cur.advance(9);
        }
        return Ok(PdfObject::Stream { dict, data });
    }

    // Fallback: scan for "endstream"
    let needle = b"endstream";
    if let Some(offset) = memchr::memmem::find(cur.remaining(), needle) {
        let data = &cur.data[data_start..data_start + offset];
        cur.pos = data_start + offset;
        cur.advance(9); // consume "endstream"
        return Ok(PdfObject::Stream { dict, data });
    }

    Err(ParseError::StreamMissingLength)
}

fn find_length_in_dict(dict: &[(&[u8], PdfObject<'_>)]) -> Option<i64> {
    for (key, val) in dict {
        if *key == b"Length" {
            return val.as_i64();
        }
    }
    None
}

// ─── Convenience: parse a single object from a byte slice ───

/// Parse a single PDF object from the given bytes.
/// Returns the parsed object (any trailing bytes are ignored).
pub fn parse_object_from_bytes<'a>(data: &'a [u8]) -> ParseResult<PdfObject<'a>> {
    let mut cur = Cursor::new(data);
    parse_object(&mut cur)
}

// ─── Re-export cursor for other parser modules ───
pub use self::Cursor as ParserCursor;
