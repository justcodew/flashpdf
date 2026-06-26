/// Font handling: CMap parsing, width lookup, CID-to-Unicode mapping.
///
/// Implements the full fallback chain:
/// ToUnicode CMap → Encoding differences → raw byte mapping → U+FFFD
use crate::parser::ParseResult;
use crate::types::PdfObject;
use std::collections::HashMap;

/// Parsed font information from a font dictionary.
#[derive(Debug, Clone)]
pub struct FontInfo {
    pub base_font: String,
    pub encoding: Option<String>,
    pub is_type0: bool,
    /// True if /Subtype is /Type3. Type 3 fonts define each glyph as a
    /// small content stream rather than an outline; flashpdf treats them
    /// as ordinary fonts (uses /Widths + ToUnicode if present) but flags
    /// them so the diagnostics layer can report glyphs that may have
    /// been mis-decoded or skipped.
    pub is_type3: bool,
    pub widths: Vec<f64>,
    /// Char code of widths[0] (from /FirstChar). Widths array is indexed
    /// relative to this. PDF spec: /Widths[i] is the width of char code
    /// /FirstChar + i, in thousandths of a unit.
    pub first_char: u32,
    pub default_width: f64,
    pub cmap: Option<CMap>,
    pub differences: Option<HashMap<u8, Vec<u8>>>,
    /// CIDFont descendant (for Type0 composite fonts)
    pub cid_font: Option<CIDFontInfo>,
    /// fitz-compatible span flags bitmask (PDF spec §7.9.2 + name heuristics).
    /// Bits (fitz numbering): italic=`2`, serif=`4`, monospaced=`8`, bold=`16`.
    /// `0` when no signal is available. Populated by `build_font_map` after
    /// resolving `/FontDescriptor /Flags`.
    pub flags: u32,
}

/// A parsed ToUnicode CMap.
#[derive(Debug, Clone)]
pub struct CMap {
    /// Single character mappings: char code → Unicode string
    pub bfchar: HashMap<Vec<u8>, Vec<u8>>,
    /// Range mappings: (start, end) → Unicode base
    pub bfrange: Vec<(Vec<u8>, Vec<u8>, Vec<u8>)>,
}

/// A contiguous range of CID widths: [c_first, c_first+len) with individual widths.
#[derive(Debug, Clone)]
pub struct CIDWidthRange {
    pub c_first: u32,
    pub widths: Vec<f64>,
}

/// CIDFont descendant info for Type0 composite fonts.
#[derive(Debug, Clone)]
pub struct CIDFontInfo {
    pub base_font: String,
    pub subtype: String, // CIDFontType0 or CIDFontType2
    /// CID width ranges (sorted by c_first for binary search)
    pub width_ranges: Vec<CIDWidthRange>,
    pub default_width: f64,
    pub dw: f64,
    /// CID to GID mapping (for CIDFontType2 TrueType-based)
    pub cid_to_gid: Vec<u8>,
    /// /DW2: default vertical displacement [vy, w1y]
    pub dw2: Option<[f64; 2]>,
    /// /W2: vertical widths
    pub w2: Vec<f64>,
}

impl Default for CIDFontInfo {
    fn default() -> Self {
        Self {
            base_font: String::new(),
            subtype: String::new(),
            width_ranges: Vec::new(),
            default_width: 1000.0,
            dw: 1000.0,
            cid_to_gid: Vec::new(),
            dw2: None,
            w2: Vec::new(),
        }
    }
}

impl CIDFontInfo {
    /// Get CID width from parsed /W ranges using binary search.
    pub fn cid_width(&self, cid: u32) -> f64 {
        // Binary search for the range containing cid
        match self.width_ranges.binary_search_by(|r| {
            if cid < r.c_first {
                std::cmp::Ordering::Greater
            } else if cid >= r.c_first + r.widths.len() as u32 {
                std::cmp::Ordering::Less
            } else {
                std::cmp::Ordering::Equal
            }
        }) {
            Ok(idx) => {
                let offset = (cid - self.width_ranges[idx].c_first) as usize;
                self.width_ranges[idx].widths[offset]
            }
            Err(_) => self.dw,
        }
    }
}

impl FontInfo {
    /// Decode a character code to Unicode using the fallback chain.
    pub fn decode_char(&self, code: &[u8]) -> char {
        // 1. Try ToUnicode CMap
        if let Some(cmap) = &self.cmap {
            if let Some(unicode) = cmap.lookup(code) {
                if let Some(c) = bytes_to_char(&unicode) {
                    return c;
                }
            }
        }

        // 2. Try Encoding differences
        if let Some(diffs) = &self.differences {
            if code.len() == 1 {
                if let Some(name) = diffs.get(&code[0]) {
                    if let Some(c) = adobe_glyph_to_char(name) {
                        return c;
                    }
                }
            }
        }

        // 3. Try built-in standard font encodings (Symbol, ZapfDingbats)
        if code.len() == 1 {
            if let Some(c) = builtin_font_decode(&self.base_font, code[0]) {
                return c;
            }
        }

        // 4. Try raw byte (for Latin-1 / ASCII)
        if code.len() == 1 {
            let b = code[0];
            if (0x20..0x7F).contains(&b) {
                return b as char;
            }
            // Latin-1 supplement
            if b >= 0x80 {
                return char::from(b);
            }
            // Control chars (< 0x20): PyMuPDF outputs the raw byte as a char
            // rather than U+FFFD when no mapping exists. Common for fonts like
            // CMEX10 that have unmapped glyph slots (e.g. byte 0x0C in a
            // "no glyph" position becomes '\x0c' in pm output).
            if b < 0x20 {
                return b as char;
            }
        }

        // 5. Fallback
        '\u{FFFD}'
    }

    /// Decode a character code to one or more Unicode chars.
    ///
    /// ToUnicode CMaps can map a single byte to multiple Unicode code points
    /// (e.g. TeX's CMR10 maps byte 0x0C to <00660069> = "fi"). This method
    /// returns all of them so the caller can emit each char with proportional
    /// width, matching PyMuPDF.
    pub fn decode_chars(&self, code: &[u8]) -> Vec<char> {
        // 1. Try ToUnicode CMap — may produce multiple UTF-16BE code units
        if let Some(cmap) = &self.cmap {
            if let Some(unicode) = cmap.lookup(code) {
                let chars = unicode_bytes_to_chars(&unicode);
                if !chars.is_empty() {
                    return chars;
                }
            }
        }
        // All other fallback paths produce a single char
        vec![self.decode_char_skip_cmap(code)]
    }

    /// Same as decode_char but skips the ToUnicode step (used by decode_chars
    /// after the ToUnicode path already failed).
    fn decode_char_skip_cmap(&self, code: &[u8]) -> char {
        // 2. Try Encoding differences
        if let Some(diffs) = &self.differences {
            if code.len() == 1 {
                if let Some(name) = diffs.get(&code[0]) {
                    if let Some(c) = adobe_glyph_to_char(name) {
                        return c;
                    }
                }
            }
        }
        // 3. Try built-in standard font encodings
        if code.len() == 1 {
            if let Some(c) = builtin_font_decode(&self.base_font, code[0]) {
                return c;
            }
        }
        // 4. Raw byte
        if code.len() == 1 {
            let b = code[0];
            if (0x20..0x7F).contains(&b) {
                return b as char;
            }
            if b >= 0x80 {
                return char::from(b);
            }
            if b < 0x20 {
                return b as char;
            }
        }
        '\u{FFFD}'
    }

    /// Get the width of a character code in thousandths of a unit.
    pub fn char_width(&self, code: u32) -> f64 {
        if code >= self.first_char {
            let idx = (code - self.first_char) as usize;
            if idx < self.widths.len() {
                return self.widths[idx];
            }
        }
        self.default_width
    }
}

impl CMap {
    pub fn lookup(&self, code: &[u8]) -> Option<Vec<u8>> {
        // Direct lookup
        if let Some(unicode) = self.bfchar.get(code) {
            return Some(unicode.clone());
        }

        // Range lookup
        for (start, end, base) in &self.bfrange {
            if code.len() == start.len()
                && code.len() == end.len()
                && code >= start.as_slice()
                && code <= end.as_slice()
            {
                let offset = code_offset(code, start);
                return Some(add_offset(base, offset));
            }
        }

        None
    }
}

fn code_offset(code: &[u8], base: &[u8]) -> u32 {
    let mut offset = 0u32;
    for i in 0..code.len() {
        offset = offset.wrapping_shl(8) | ((code[i] as u32).wrapping_sub(base[i] as u32));
    }
    offset
}

fn add_offset(base: &[u8], offset: u32) -> Vec<u8> {
    let mut result = base.to_vec();
    let mut carry = offset as i64;
    for i in (0..result.len()).rev() {
        let val = result[i] as i64 + carry;
        result[i] = (val & 0xFF) as u8;
        carry = val >> 8;
    }
    result
}

fn bytes_to_char(bytes: &[u8]) -> Option<char> {
    if bytes.len() == 1 {
        return Some(bytes[0] as char);
    }
    if bytes.len() == 2 {
        let code = ((bytes[0] as u32) << 8) | (bytes[1] as u32);
        return char::from_u32(code);
    }
    if bytes.len() == 3 {
        let code = ((bytes[0] as u32) << 16) | ((bytes[1] as u32) << 8) | (bytes[2] as u32);
        return char::from_u32(code);
    }
    // 4 bytes: could be UTF-16BE surrogate pair or direct 4-byte encoding
    if bytes.len() == 4 {
        let code = ((bytes[0] as u32) << 24)
            | ((bytes[1] as u32) << 16)
            | ((bytes[2] as u32) << 8)
            | (bytes[3] as u32);
        // Check if it's a UTF-16BE surrogate pair (0xD800..0xDBFF followed by 0xDC00..0xDFFF)
        if (code >> 16) >= 0xD800
            && (code >> 16) <= 0xDBFF
            && (code & 0xFFFF) >= 0xDC00
            && (code & 0xFFFF) <= 0xDFFF
        {
            let high = (code >> 16) - 0xD800;
            let low = (code & 0xFFFF) - 0xDC00;
            let unicode = 0x10000 + (high << 10) + low;
            return char::from_u32(unicode);
        }
        // Direct 4-byte encoding
        return char::from_u32(code);
    }
    None
}

/// Interpret ToUnicode output as a sequence of UTF-16BE code units, returning
/// one char per scalar value (surrogate pairs decoded to a single char).
///
/// ToUnicode mappings can produce multiple code points for a single byte
/// (e.g. <00660069> = "fi"). Each pair of bytes is one UTF-16BE unit; an
/// odd-length byte sequence falls back to single-byte chars so we never
/// silently drop data.
fn unicode_bytes_to_chars(bytes: &[u8]) -> Vec<char> {
    if bytes.is_empty() {
        return Vec::new();
    }
    // Single byte: direct char (Latin-1-ish, used by some CMaps)
    if bytes.len() == 1 {
        return vec![bytes[0] as char];
    }
    // Even-length ≥ 2: treat as UTF-16BE code unit sequence
    let mut chars = Vec::new();
    let mut i = 0;
    while i + 1 < bytes.len() {
        let unit = ((bytes[i] as u32) << 8) | (bytes[i + 1] as u32);
        // High surrogate → try to consume the next unit as low surrogate
        if (0xD800..0xDBFF).contains(&unit) {
            if i + 3 < bytes.len() {
                let low = ((bytes[i + 2] as u32) << 8) | (bytes[i + 3] as u32);
                if (0xDC00..0xDFFF).contains(&low) {
                    let scalar = 0x10000 + ((unit - 0xD800) << 10) + (low - 0xDC00);
                    if let Some(c) = char::from_u32(scalar) {
                        chars.push(c);
                        i += 4;
                        continue;
                    }
                }
            }
            // Malformed surrogate — emit replacement
            chars.push('\u{FFFD}');
            i += 2;
            continue;
        }
        if let Some(c) = char::from_u32(unit) {
            chars.push(c);
        } else {
            chars.push('\u{FFFD}');
        }
        i += 2;
    }
    // Trailing odd byte
    if i < bytes.len() {
        chars.push(bytes[i] as char);
    }
    chars
}

// ─── CMap parsing ───

/// Parse a ToUnicode CMap from stream data.
pub fn parse_cmap(data: &[u8]) -> ParseResult<CMap> {
    let mut bfchar = HashMap::new();
    let mut bfrange = Vec::new();
    let mut pos = 0;
    let s = std::str::from_utf8(data).unwrap_or("");

    // Find beginbfchar ... endbfchar blocks
    while let Some(start) = find_token(s, pos, "beginbfchar") {
        let after = start + "beginbfchar".len();
        let end = find_token(s, after, "endbfchar").unwrap_or(s.len());
        let block = &s[after..end];
        parse_bfchar_block(block, &mut bfchar);
        pos = end + "endbfchar".len();
    }

    // Find beginbfrange ... endbfrange blocks
    pos = 0;
    while let Some(start) = find_token(s, pos, "beginbfrange") {
        let after = start + "beginbfrange".len();
        let end = find_token(s, after, "endbfrange").unwrap_or(s.len());
        let block = &s[after..end];
        parse_bfrange_block(block, &mut bfrange);
        pos = end + "endbfrange".len();
    }

    Ok(CMap { bfchar, bfrange })
}

fn find_token(s: &str, from: usize, token: &str) -> Option<usize> {
    s[from..].find(token).map(|i| i + from)
}

fn parse_bfchar_block(block: &str, map: &mut HashMap<Vec<u8>, Vec<u8>>) {
    let tokens = extract_hex_tokens(block);
    let mut i = 0;
    while i + 1 < tokens.len() {
        let src = &tokens[i];
        let dst = &tokens[i + 1];
        if !src.is_empty() && !dst.is_empty() {
            map.insert(src.clone(), dst.clone());
        }
        i += 2;
    }
}

fn parse_bfrange_block(block: &str, ranges: &mut Vec<(Vec<u8>, Vec<u8>, Vec<u8>)>) {
    let tokens = extract_hex_tokens(block);
    let mut i = 0;
    while i + 2 < tokens.len() {
        let start = &tokens[i];
        let end = &tokens[i + 1];
        let dst = &tokens[i + 2];
        if !start.is_empty() && !end.is_empty() && !dst.is_empty() {
            ranges.push((start.clone(), end.clone(), dst.clone()));
        }
        i += 3;
    }
}

/// Extract all `<hex>` tokens from a CMap block, returning their byte values.
///
/// Handles both space-separated (`<21> <21> <0041>`) and concatenated
/// (`<21><21><0041>`) forms. Also strips bfrange array syntax like
/// `[<0041><0042>]` by flattening brackets.
fn extract_hex_tokens(s: &str) -> Vec<Vec<u8>> {
    let mut tokens = Vec::new();
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'<' {
            let start = i + 1;
            let mut j = start;
            while j < bytes.len() && bytes[j] != b'>' {
                j += 1;
            }
            if j < bytes.len() {
                // Parse hex between < and >
                let hex = &s[start..j];
                let parsed = parse_hex_bytes(hex);
                if !parsed.is_empty() {
                    tokens.push(parsed);
                }
                i = j + 1;
            } else {
                break;
            }
        } else {
            i += 1;
        }
    }
    tokens
}

fn parse_hex_bytes(hex: &str) -> Vec<u8> {
    let mut result = Vec::new();
    let chars: Vec<char> = hex.chars().filter(|c| !c.is_whitespace()).collect();
    let mut i = 0;
    while i + 1 < chars.len() {
        let hi = hex_char_val(chars[i]);
        let lo = hex_char_val(chars[i + 1]);
        result.push((hi << 4) | lo);
        i += 2;
    }
    // Odd trailing nibble
    if i < chars.len() {
        let hi = hex_char_val(chars[i]);
        result.push(hi << 4);
    }
    result
}

fn hex_char_val(c: char) -> u8 {
    match c {
        '0'..='9' => (c as u8) - b'0',
        'a'..='f' => (c as u8) - b'a' + 10,
        'A'..='F' => (c as u8) - b'A' + 10,
        _ => 0,
    }
}

// ─── Font info extraction ───

/// Extract FontInfo from a font dictionary object.
pub fn extract_font_info(font_obj: &PdfObject<'_>) -> FontInfo {
    let base_font = font_obj
        .get(b"BaseFont")
        .and_then(|v| v.as_name())
        .map(|n| String::from_utf8_lossy(n).to_string())
        .unwrap_or_else(|| "Unknown".to_string());

    let encoding = font_obj
        .get(b"Encoding")
        .and_then(|v| v.as_name())
        .map(|n| String::from_utf8_lossy(n).to_string());

    let subtype = font_obj
        .get(b"Subtype")
        .and_then(|v| v.as_name())
        .unwrap_or(b"");

    let is_type0 = subtype == b"Type0";

    // Extract /Widths array. NOTE: /Widths may be an indirect reference
    // (common in real-world PDFs). Resolution is the caller's responsibility
    // (build_font_map). Here we handle only the inline-array case.
    let widths = font_obj
        .get(b"Widths")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|item| item.as_f64()).collect())
        .unwrap_or_default();

    // /FirstChar: char code of widths[0]. Default 0 keeps historical behavior
    // for fonts that emit a full 0..256 widths array inline.
    let first_char = font_obj
        .get(b"FirstChar")
        .and_then(|v| v.as_f64())
        .map(|v| v as u32)
        .unwrap_or(0);

    let default_width = font_obj
        .get(b"DW")
        .and_then(|v| v.as_f64())
        .unwrap_or(1000.0);

    // Extract /Differences from Encoding dict
    let differences = extract_differences(font_obj);

    // Initial flags from base_font name heuristics; /FontDescriptor /Flags
    // bits are OR'd in by build_font_map after resolving the indirect ref.
    let flags = compute_font_flags(&base_font, None);

    FontInfo {
        base_font,
        encoding,
        is_type0,
        is_type3: subtype == b"Type3",
        widths,
        first_char,
        default_width,
        cmap: None, // Populated later from ToUnicode stream
        differences,
        cid_font: None, // Populated for Type0 fonts
        flags,
    }
}

/// Compute fitz-compatible span flags from a font's /BaseFont name and
/// (optionally) /FontDescriptor /Flags. The bitmask uses fitz numbering
/// so the result can flow directly to TextSpan.flags:
///
/// * `2^1 (2)` italic — PDF /Flags Italic (bit 7 = 64) or Script (bit 4 = 8), or name matches `*Italic*` / `*Oblique*`
/// * `2^2 (4)` serif — PDF /Flags Serif (bit 2 = 2), or name matches `*Times*` / `*Serif*` / `*Garamond*` / `*Palatino*`
/// * `2^3 (8)` monospaced — PDF /Flags FixedPitch (bit 1 = 1), or name matches `*Courier*` / `*Mono*` / `*Consolas*` / `*Menlo*`
/// * `2^4 (16)` bold — name matches `*Bold*` / `*Black*` / `*Heavy*` / `*Demi*` / `*Semibold*`
///
/// PDF /Flags has no bold bit — bold is name-only.
pub fn compute_font_flags(base_font: &str, descriptor_flags: Option<i64>) -> u32 {
    let mut flags: u32 = 0;
    let name_lower = base_font.to_ascii_lowercase();

    // /FontDescriptor /Flags bits (PDF spec §7.9.2 Table 21)
    if let Some(df) = descriptor_flags {
        // bit 1 (value 1) = FixedPitch
        if df & 0x01 != 0 {
            flags |= 8; // mono
        }
        // bit 2 (value 2) = Serif
        if df & 0x02 != 0 {
            flags |= 4; // serif
        }
        // bit 4 (value 8) = Script
        if df & 0x08 != 0 {
            flags |= 2; // italic
        }
        // bit 7 (value 64) = Italic
        if df & 0x40 != 0 {
            flags |= 2; // italic
        }
    }

    // Name heuristics — useful when /FontDescriptor is missing (subset fonts
    // like "ABCDEF+TimesNewRoman-Bold" still carry the style suffix).
    if name_lower.contains("bold")
        || name_lower.contains("black")
        || name_lower.contains("heavy")
        || name_lower.contains("demi")
        || name_lower.contains("semibold")
    {
        flags |= 16;
    }
    if name_lower.contains("italic") || name_lower.contains("oblique") {
        flags |= 2;
    }
    if name_lower.contains("courier")
        || name_lower.contains("mono")
        || name_lower.contains("consolas")
        || name_lower.contains("menlo")
        || name_lower.contains("consol")
        || name_lower.contains("nimbusmono")
        // TeX Computer Modern Typewriter (monospace).
        || name_lower.contains("cmtt")
    {
        flags |= 8;
    }
    if name_lower.contains("times")
        || name_lower.contains("serif")
        || name_lower.contains("garamond")
        || name_lower.contains("palatino")
        || name_lower.contains("georgia")
        || name_lower.contains("songti")
        || name_lower.contains("simsun")
        || name_lower.contains("mincho")
        // Ghostscript's Nimbus family ships as open-source clones of the
        // standard 14: NimbusRom = Times-equivalent (serif), NimbusMono =
        // Courier-equivalent (mono). Common in arxiv / academic PDFs.
        || name_lower.contains("nimbusrom")
        || name_lower.contains("nimbusromno")
        // TeX Computer Modern family (CMR/CMB/CMMI/CMSY/CMEX) is serif by
        // fitz convention. Math variants (CMMI/CMSY/CMMIB) are also italic.
        || name_lower.starts_with("cmr")
        || name_lower.starts_with("cmb")
        || name_lower.starts_with("cmmi")
        || name_lower.starts_with("cmsy")
        || name_lower.starts_with("cmex")
        || name_lower.contains("cambria")
    {
        flags |= 4;
    }

    // TeX Computer Modern Math / Symbol variants are italic by convention.
    if name_lower.starts_with("cmmi")
        || name_lower.starts_with("cmsy")
        || name_lower.starts_with("cmmib")
    {
        flags |= 2;
    }

    flags
}

fn extract_differences(font_obj: &PdfObject<'_>) -> Option<HashMap<u8, Vec<u8>>> {
    let encoding = font_obj.get(b"Encoding")?;
    let dict = match encoding {
        PdfObject::Dict(d) => d,
        PdfObject::Ref(_) => return None, // Would need to resolve via get_object
        _ => return None,
    };

    let diff_array = dict
        .iter()
        .find(|(k, _)| *k == b"Differences")?
        .1
        .as_array()?;

    let mut result = HashMap::new();
    let mut current_code: u8 = 0;

    for item in diff_array {
        match item {
            PdfObject::Integer(n) => {
                current_code = *n as u8;
            }
            PdfObject::Name(name) => {
                result.insert(current_code, name.to_vec());
                current_code = current_code.wrapping_add(1);
            }
            _ => {}
        }
    }

    if result.is_empty() {
        None
    } else {
        Some(result)
    }
}

/// Extract differences from encoding dict, resolving references.
fn extract_differences_with_resolve<'a>(
    font_obj: &PdfObject<'a>,
    get_object: &impl Fn(u32) -> ParseResult<PdfObject<'a>>,
) -> Option<HashMap<u8, Vec<u8>>> {
    let encoding = font_obj.get(b"Encoding")?;

    // Resolve reference if needed
    let resolved;
    let dict = match encoding {
        PdfObject::Dict(d) => d,
        PdfObject::Ref(r) => {
            resolved = get_object(r.num).ok()?;
            match &resolved {
                PdfObject::Dict(d) => d,
                _ => return None,
            }
        }
        _ => return None,
    };

    let diff_array = dict
        .iter()
        .find(|(k, _)| *k == b"Differences")?
        .1
        .as_array()?;

    let mut result = HashMap::new();
    let mut current_code: u8 = 0;

    for item in diff_array {
        match item {
            PdfObject::Integer(n) => {
                current_code = *n as u8;
            }
            PdfObject::Name(name) => {
                result.insert(current_code, name.to_vec());
                current_code = current_code.wrapping_add(1);
            }
            _ => {}
        }
    }

    if result.is_empty() {
        None
    } else {
        Some(result)
    }
}

/// Parse the /Encoding vector from an embedded Type1 font program (PFA/PFB).
///
/// Returns a byte → glyph-name map. Used for TeX CM fonts that have no PDF
/// /Encoding and no /ToUnicode, leaving the glyph→Unicode mapping recoverable
/// only via the font program itself.
fn extract_encoding_from_font_program<'a>(
    font_obj: &PdfObject<'a>,
    get_object: &impl Fn(u32) -> ParseResult<PdfObject<'a>>,
) -> Option<HashMap<u8, Vec<u8>>> {
    // Resolve FontDescriptor → FontFile / FontFile2 / FontFile3
    let fd_ref = match font_obj.get(b"FontDescriptor")? {
        PdfObject::Ref(r) => r,
        _ => return None,
    };
    let fd = get_object(fd_ref.num).ok()?;
    let ff_ref = fd
        .get(b"FontFile")
        .or_else(|| fd.get(b"FontFile2"))
        .or_else(|| fd.get(b"FontFile3"))?;
    let ff_ref = match ff_ref {
        PdfObject::Ref(r) => r,
        _ => return None,
    };
    let ff_obj = get_object(ff_ref.num).ok()?;
    let (data, dict) = match &ff_obj {
        PdfObject::Stream { data, dict } => (data, dict),
        _ => return None,
    };
    let filter = dict.iter().find(|(k, _)| *k == b"Filter").map(|(_, v)| v);
    let raw = match filter {
        Some(f) => {
            crate::parser::xref::decompress_stream(data, f).unwrap_or_else(|_| data.to_vec())
        }
        None => data.to_vec(),
    };

    // PFB has a binary header (0x80 <type> <len-lo> <len-hi> per section).
    // PFA is plain ASCII. Detect PFB and strip headers to get ASCII text.
    let text = if raw.starts_with(&[0x80]) {
        strip_pfb_header(&raw)
    } else {
        String::from_utf8_lossy(&raw).to_string()
    };

    parse_type1_encoding(&text)
}

/// Strip PFB binary section headers, concatenating the ASCII payload.
fn strip_pfb_header(raw: &[u8]) -> String {
    let mut out = Vec::new();
    let mut i = 0;
    while i + 3 < raw.len() {
        if raw[i] != 0x80 {
            break;
        }
        let _seg_type = raw[i + 1];
        let len = (raw[i + 2] as usize) | ((raw[i + 3] as usize) << 8);
        i += 4;
        let end = (i + len).min(raw.len());
        out.extend_from_slice(&raw[i..end]);
        i = end;
    }
    String::from_utf8_lossy(&out).to_string()
}

/// Extract `dup <pos> /<glyphname> put` entries from a Type1 font program.
fn parse_type1_encoding(text: &str) -> Option<HashMap<u8, Vec<u8>>> {
    let bytes = text.as_bytes();
    let mut result = HashMap::new();
    let needle = b"dup ";
    let mut i = 0;
    while let Some(rel) = memchr::memmem::find(&bytes[i..], needle) {
        i += rel + needle.len();
        // Skip whitespace
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        // Parse decimal number
        let start = i;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
        }
        if i == start {
            continue;
        }
        let pos_str = &text[start..i];
        let Ok(pos) = pos_str.parse::<u8>() else {
            continue;
        };
        // Skip whitespace
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        // Expect '/'
        if i >= bytes.len() || bytes[i] != b'/' {
            continue;
        }
        i += 1;
        let name_start = i;
        while i < bytes.len() && !bytes[i].is_ascii_whitespace() && bytes[i] != b'/' {
            i += 1;
        }
        let name = &text[name_start..i];
        if name == ".notdef" {
            continue;
        }
        result.insert(pos, name.as_bytes().to_vec());
    }
    if result.is_empty() {
        None
    } else {
        Some(result)
    }
}

/// Build a complete font map from a page's /Resources /Font dictionary.
pub fn build_font_map<'a>(
    resources: &PdfObject<'a>,
    get_object: impl Fn(u32) -> ParseResult<PdfObject<'a>>,
) -> HashMap<String, FontInfo> {
    let mut result = HashMap::new();

    let fonts_dict = match resources.get(b"Font") {
        Some(PdfObject::Dict(d)) => d,
        _ => return result,
    };

    for (name, font_ref) in fonts_dict {
        let font_name = String::from_utf8_lossy(name).to_string();

        let font_obj = match font_ref {
            PdfObject::Ref(r) => match get_object(r.num) {
                Ok(obj) => obj,
                _ => continue,
            },
            PdfObject::Dict(_) => font_ref.clone(),
            _ => continue,
        };

        let mut info = extract_font_info(&font_obj);

        // Resolve /FontDescriptor /Flags (PDF spec §7.9.2 Table 21) and OR
        // the converted bits into info.flags. extract_font_info already
        // populated name-heuristic bits, so we only add /Flags-derived bits.
        if let Some(PdfObject::Ref(r)) = font_obj.get(b"FontDescriptor") {
            if let Ok(fd) = get_object(r.num) {
                if let Some(df) = fd.get(b"Flags").and_then(|v| v.as_i64()) {
                    info.flags |= compute_font_flags(&info.base_font, Some(df));
                }
            }
        }

        // /Widths is commonly an indirect reference (e.g. "443 0 R") which
        // extract_font_info cannot resolve on its own. If widths is empty
        // but /Widths exists as a Ref, resolve it here so char_width returns
        // real values instead of /DW (default 1000 — far too wide).
        if info.widths.is_empty() {
            if let Some(PdfObject::Ref(r)) = font_obj.get(b"Widths") {
                if let Ok(w_obj) = get_object(r.num) {
                    if let Some(arr) = w_obj.as_array() {
                        info.widths = arr.iter().filter_map(|item| item.as_f64()).collect();
                    }
                }
            }
        }

        // Try to resolve encoding differences if not already extracted
        if info.differences.is_none() {
            info.differences = extract_differences_with_resolve(&font_obj, &get_object);
        }

        // Last resort: parse the embedded Type1 font program's /Encoding
        // vector. TeX CM fonts (CMSY/CMMI/CMEX/CMR) ship without a PDF-level
        // /Encoding and without /ToUnicode, so the only source of the byte →
        // glyph-name mapping is the PFA/PFB stream in /FontFile.
        if info.differences.is_none() && info.cmap.is_none() {
            if let Some(diffs) = extract_encoding_from_font_program(&font_obj, &get_object) {
                info.differences = Some(diffs);
            }
        }

        // Try to get ToUnicode CMap
        if let Some(PdfObject::Ref(r)) = font_obj.get(b"ToUnicode") {
            if let Ok(tounicode_obj) = get_object(r.num) {
                if let PdfObject::Stream { data, dict } = &tounicode_obj {
                    // Decompress if needed
                    let filter = dict.iter().find(|(k, _)| *k == b"Filter").map(|(_, v)| v);
                    let cmap_data = match filter {
                        Some(f) => crate::parser::xref::decompress_stream(data, f)
                            .unwrap_or_else(|_| data.to_vec()),
                        None => data.to_vec(),
                    };

                    if let Ok(cmap) = parse_cmap(&cmap_data) {
                        info.cmap = Some(cmap);
                    }
                }
            }
        }

        // For Type0 fonts, resolve CIDFont descendant
        if info.is_type0 {
            if let Some(PdfObject::Array(descendants)) = font_obj.get(b"DescendantFonts") {
                if let Some(PdfObject::Ref(r)) = descendants.first() {
                    if let Ok(cid_font_obj) = get_object(r.num) {
                        info.cid_font = Some(extract_cid_font_info(&cid_font_obj));
                    }
                }
            }
        }

        result.insert(font_name, info);
    }

    result
}

/// Extract CIDFont info from a CIDFont dictionary.
fn extract_cid_font_info(cid_obj: &PdfObject<'_>) -> CIDFontInfo {
    let base_font = cid_obj
        .get(b"BaseFont")
        .and_then(|v| v.as_name())
        .map(|n| String::from_utf8_lossy(n).to_string())
        .unwrap_or_default();

    let subtype = cid_obj
        .get(b"Subtype")
        .and_then(|v| v.as_name())
        .map(|n| String::from_utf8_lossy(n).to_string())
        .unwrap_or_default();

    let dw = cid_obj
        .get(b"DW")
        .and_then(|v| v.as_f64())
        .unwrap_or(1000.0);

    // Parse /W array into ranges: format is c_first c_last w1 w2 ... wn OR c [w1 w2 ... wn]
    let width_ranges = parse_cid_widths(cid_obj);

    // Parse /CIDToGIDMap (for CIDFontType2)
    let cid_to_gid = match cid_obj.get(b"CIDToGIDMap") {
        Some(PdfObject::Stream { data, .. }) => data.to_vec(),
        _ => Vec::new(),
    };

    let dw2 = cid_obj.get(b"DW2").and_then(|v| {
        v.as_array().and_then(|arr| {
            if arr.len() >= 2 {
                Some([
                    arr[0].as_f64().unwrap_or(0.0),
                    arr[1].as_f64().unwrap_or(0.0),
                ])
            } else {
                None
            }
        })
    });

    let w2 = cid_obj
        .get(b"W2")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|item| item.as_f64()).collect())
        .unwrap_or_default();

    CIDFontInfo {
        base_font,
        subtype,
        width_ranges,
        default_width: dw,
        dw,
        cid_to_gid,
        dw2,
        w2,
    }
}

/// Parse CIDFont /W array into width ranges.
/// Format: c_first c_last w1 w2 ... wn (range) OR c [w1 w2 ... wn] (array)
fn parse_cid_widths(cid_obj: &PdfObject<'_>) -> Vec<CIDWidthRange> {
    let w_array = match cid_obj.get(b"W") {
        Some(PdfObject::Array(a)) => a,
        _ => return Vec::new(),
    };

    let mut ranges = Vec::new();
    let mut i = 0;
    while i < w_array.len() {
        let c_first = match &w_array[i] {
            PdfObject::Integer(n) => *n as u32,
            _ => {
                i += 1;
                continue;
            }
        };

        if i + 1 >= w_array.len() {
            break;
        }

        // Check if next is an array (c [w1 w2 ... wn]) or integer (c_first c_last w...)
        match &w_array[i + 1] {
            PdfObject::Array(arr) => {
                // Individual widths: c [w1 w2 ... wn]
                let widths: Vec<f64> = arr.iter().map(|w| w.as_f64().unwrap_or(0.0)).collect();
                if !widths.is_empty() {
                    ranges.push(CIDWidthRange { c_first, widths });
                }
                i += 2;
            }
            PdfObject::Integer(c_last) => {
                // Range: c_first c_last w1 w2 ... wn
                let c_last = *c_last as u32;
                let count = (c_last - c_first + 1) as usize;
                let mut widths = Vec::with_capacity(count);
                for j in 0..count {
                    if i + 2 + j < w_array.len() {
                        widths.push(w_array[i + 2 + j].as_f64().unwrap_or(0.0));
                    } else {
                        widths.push(0.0);
                    }
                }
                ranges.push(CIDWidthRange { c_first, widths });
                i += 2 + count;
            }
            _ => {
                i += 1;
            }
        }
    }

    // Sort by c_first for binary search
    ranges.sort_by_key(|r| r.c_first);
    ranges
}

// ─── Adobe Glyph List (subset) ───

fn adobe_glyph_to_char(name: &[u8]) -> Option<char> {
    let s = std::str::from_utf8(name).ok()?;
    match s {
        "space" => Some(' '),
        "exclam" => Some('!'),
        "quotedbl" => Some('"'),
        "numbersign" => Some('#'),
        "dollar" => Some('$'),
        "percent" => Some('%'),
        "ampersand" => Some('&'),
        "quotesingle" => Some('\''),
        "parenleft" => Some('('),
        "parenright" => Some(')'),
        "asterisk" => Some('*'),
        "plus" => Some('+'),
        "comma" => Some(','),
        "hyphen" => Some('-'),
        "period" => Some('.'),
        "slash" => Some('/'),
        "zero" => Some('0'),
        "one" => Some('1'),
        "two" => Some('2'),
        "three" => Some('3'),
        "four" => Some('4'),
        "five" => Some('5'),
        "six" => Some('6'),
        "seven" => Some('7'),
        "eight" => Some('8'),
        "nine" => Some('9'),
        "colon" => Some(':'),
        "semicolon" => Some(';'),
        "less" => Some('<'),
        "equal" => Some('='),
        "greater" => Some('>'),
        "question" => Some('?'),
        "at" => Some('@'),
        "bracketleft" => Some('['),
        "backslash" => Some('\\'),
        "bracketright" => Some(']'),
        "asciicircum" => Some('^'),
        "underscore" => Some('_'),
        "grave" => Some('`'),
        "braceleft" => Some('{'),
        "bar" => Some('|'),
        "braceright" => Some('}'),
        "asciitilde" => Some('~'),
        "bullet" => Some('•'),
        "dagger" => Some('†'),
        "daggerdbl" => Some('‡'),
        "ellipsis" => Some('…'),
        "emdash" => Some('—'),
        "endash" => Some('–'),
        "fi" => Some('ﬁ'),
        "fl" => Some('ﬂ'),
        "fraction" => Some('⁄'),
        "guilsinglleft" => Some('‹'),
        "guilsinglright" => Some('›'),
        "minus" => Some('−'),
        "mu" => Some('μ'),
        "multiply" => Some('×'),
        "onehalf" => Some('½'),
        "onequarter" => Some('¼'),
        "onesuperior" => Some('¹'),
        "plusminus" => Some('±'),
        // Math glyphs commonly recovered from CM font programs
        "asteriskmath" => Some('∗'),
        "circlemultiply" => Some('⊗'),
        "circleplus" => Some('⊕'),
        "circumflex" => Some('ˆ'),
        "equivalence" => Some('≡'),
        "existential" => Some('∃'),
        "openbullet" => Some('◦'),
        "prime" => Some('′'),
        "propersubset" => Some('⊂'),
        "propersuperset" => Some('⊃'),
        "reflectequiv" => Some('≅'),
        "similar" => Some('∼'),
        "universal" => Some('∀'),
        "quotedblbase" => Some('„'),
        "quotedblleft" => Some('\u{201C}'),
        "quotedblright" => Some('\u{201D}'),
        "quoteleft" => Some('\u{2018}'),
        "quoteright" => Some('\u{2019}'),
        "quotesinglbase" => Some('‚'),
        "registered" => Some('®'),
        "threequarters" => Some('¾'),
        "threesuperior" => Some('³'),
        "trademark" => Some('™'),
        "twosuperior" => Some('²'),
        "Euro" => Some('€'),
        "Lslash" => Some('Ł'),
        "OE" => Some('Œ'),
        "Scaron" => Some('Š'),
        "Zcaron" => Some('Ž'),
        "lslash" => Some('ł'),
        "oe" => Some('œ'),
        "scaron" => Some('š'),
        "zcaron" => Some('ž'),
        // Greek letters (common in mathematics)
        "Alpha" => Some('Α'),
        "Beta" => Some('Β'),
        "Gamma" => Some('Γ'),
        "Delta" => Some('Δ'),
        "Epsilon" => Some('Ε'),
        "Zeta" => Some('Ζ'),
        "Eta" => Some('Η'),
        "Theta" => Some('Θ'),
        "Iota" => Some('Ι'),
        "Kappa" => Some('Κ'),
        "Lambda" => Some('Λ'),
        "Mu" => Some('Μ'),
        "Nu" => Some('Ν'),
        "Xi" => Some('Ξ'),
        "Omicron" => Some('Ο'),
        "Pi" => Some('Π'),
        "Rho" => Some('Ρ'),
        "Sigma" => Some('Σ'),
        "Tau" => Some('Τ'),
        "Upsilon" => Some('Υ'),
        "Phi" => Some('Φ'),
        "Chi" => Some('Χ'),
        "Psi" => Some('Ψ'),
        "Omega" => Some('Ω'),
        "alpha" => Some('α'),
        "beta" => Some('β'),
        "gamma" => Some('γ'),
        "delta" => Some('δ'),
        "epsilon" => Some('ε'),
        "zeta" => Some('ζ'),
        "eta" => Some('η'),
        "theta" => Some('θ'),
        "iota" => Some('ι'),
        "kappa" => Some('κ'),
        "lambda" => Some('λ'),
        "nu" => Some('ν'),
        "xi" => Some('ξ'),
        "omicron" => Some('ο'),
        "pi" => Some('π'),
        "rho" => Some('ρ'),
        "sigma" => Some('σ'),
        "tau" => Some('τ'),
        "upsilon" => Some('υ'),
        "phi" => Some('φ'),
        "chi" => Some('χ'),
        "psi" => Some('ψ'),
        "omega" => Some('ω'),
        "phi1" => Some('ϕ'),
        "epsilon1" => Some('ϵ'),
        // Mathematical symbols
        "infinity" => Some('∞'),
        "partialdiff" => Some('∂'),
        "nabla" => Some('∇'),
        "integral" => Some('∫'),
        "product" => Some('∏'),
        "summation" => Some('∑'),
        "radical" => Some('√'),
        "sim" => Some('∼'),
        "congruent" => Some('≅'),
        "approxequal" => Some('≈'),
        "notequal" => Some('≠'),
        "lessequal" => Some('≤'),
        "greaterequal" => Some('≥'),
        "logicalnot" => Some('¬'),
        "logicaland" => Some('∧'),
        "logicalor" => Some('∨'),
        "element" => Some('∈'),
        "notelement" => Some('∉'),
        "subset" => Some('⊂'),
        "superset" => Some('⊃'),
        "subseteq" => Some('⊆'),
        "supersetequal" => Some('⊇'),
        "union" => Some('∪'),
        "intersection" => Some('∩'),
        "emptyset" => Some('∅'),
        "forall" => Some('∀'),
        "exist" => Some('∃'),
        "angle" => Some('∠'),
        "perpendicular" => Some('⊥'),
        "parallel" => Some('∥'),
        "arrowleft" => Some('←'),
        "arrowright" => Some('→'),
        "arrowup" => Some('↑'),
        "arrowdown" => Some('↓'),
        "arrowboth" => Some('↔'),
        "arrowdblleft" => Some('⇐'),
        "arrowdblright" => Some('⇒'),
        "arrowdblup" => Some('⇑'),
        "arrowdbldown" => Some('⇓'),
        // Common diacritical characters
        "Aacute" => Some('Á'),
        "Agrave" => Some('À'),
        "Acircumflex" => Some('Â'),
        "Atilde" => Some('Ã'),
        "Adieresis" => Some('Ä'),
        "Aring" => Some('Å'),
        "Ccedilla" => Some('Ç'),
        "Eacute" => Some('É'),
        "Egrave" => Some('È'),
        "Ecircumflex" => Some('Ê'),
        "Edieresis" => Some('Ë'),
        "Iacute" => Some('Í'),
        "Igrave" => Some('Ì'),
        "Icircumflex" => Some('Î'),
        "Idieresis" => Some('Ï'),
        "Ntilde" => Some('Ñ'),
        "Oacute" => Some('Ó'),
        "Ograve" => Some('Ò'),
        "Ocircumflex" => Some('Ô'),
        "Otilde" => Some('Õ'),
        "Odieresis" => Some('Ö'),
        "Uacute" => Some('Ú'),
        "Ugrave" => Some('Ù'),
        "Ucircumflex" => Some('Û'),
        "Udieresis" => Some('Ü'),
        "Yacute" => Some('Ý'),
        "aacute" => Some('á'),
        "agrave" => Some('à'),
        "acircumflex" => Some('â'),
        "atilde" => Some('ã'),
        "adieresis" => Some('ä'),
        "aring" => Some('å'),
        "ccedilla" => Some('ç'),
        "eacute" => Some('é'),
        "egrave" => Some('è'),
        "ecircumflex" => Some('ê'),
        "edieresis" => Some('ë'),
        "iacute" => Some('í'),
        "igrave" => Some('ì'),
        "icircumflex" => Some('î'),
        "idieresis" => Some('ï'),
        "ntilde" => Some('ñ'),
        "oacute" => Some('ó'),
        "ograve" => Some('ò'),
        "ocircumflex" => Some('ô'),
        "otilde" => Some('õ'),
        "odieresis" => Some('ö'),
        "uacute" => Some('ú'),
        "ugrave" => Some('ù'),
        "ucircumflex" => Some('û'),
        "udieresis" => Some('ü'),
        "yacute" => Some('ý'),
        "ydieresis" => Some('ÿ'),
        // Default: try to parse as uniXXXX
        _ => {
            if s.starts_with("uni") && s.len() >= 7 {
                let hex = &s[3..];
                if let Ok(code) = u32::from_str_radix(hex, 16) {
                    return char::from_u32(code);
                }
            }
            None
        }
    }
}

// ─── Built-in standard font encodings (Symbol, ZapfDingbats) ───

/// Strip a subset prefix like `RPUFKZ+Dingbats` → `Dingbats`.
fn strip_subset_prefix(name: &str) -> &str {
    match name.find('+') {
        Some(idx) => &name[idx + 1..],
        None => name,
    }
}

/// Look up a byte in the built-in encoding for a standard PDF font
/// (Symbol or ZapfDingbats). Returns None for non-standard fonts or
/// unmapped codes.
fn builtin_font_decode(font_name: &str, byte: u8) -> Option<char> {
    let stripped = strip_subset_prefix(font_name);
    // PDF standard fonts (exact match)
    match stripped {
        "Symbol" => return symbol_decode(byte),
        "ZapfDingbats" | "Dingbats" => return zapfdingbats_decode(byte),
        _ => {}
    }
    // TeX Computer Modern Symbol font (matched by prefix, since subsets
    // like `NIWCFP+CMSY8` retain the family name after the subset tag).
    // LaTeX-generated PDFs typically omit the ToUnicode CMap for cmsy
    // fonts, leaving glyphs like `bullet` (byte 15) and `multiply`
    // (byte 2) undecodable without this table.
    if stripped.starts_with("CMSY") {
        return cmsy_decode(byte);
    }
    None
}

/// Adobe Symbol font encoding (PDF 1.7 Appendix D.5).
fn symbol_decode(byte: u8) -> Option<char> {
    Some(match byte {
        b' ' => ' ',
        b'!' => '!',
        0x22 => '∀',
        b'#' => '#',
        b'$' => '∃',
        b'%' => '%',
        b'&' => '&',
        b'\'' => '∋',
        b'(' => '(',
        b')' => ')',
        b'*' => '∗',
        b'+' => '+',
        b',' => ',',
        b'-' => '−', // U+2212 MINUS SIGN
        b'.' => '.',
        b'/' => '/',
        b'0'..=b'9' => byte as char,
        b':' => ':',
        b';' => ';',
        b'<' => '<',
        b'=' => '=',
        b'>' => '>',
        b'?' => '?',
        b'@' => '≅',
        b'A' => 'Α',
        b'B' => 'Β',
        b'C' => 'Χ',
        b'D' => 'Δ',
        b'E' => 'Ε',
        b'F' => 'Φ',
        b'G' => 'Γ',
        b'H' => 'Η',
        b'I' => 'Ι',
        b'J' => 'ϑ',
        b'K' => 'Κ',
        b'L' => 'Λ',
        b'M' => 'Μ',
        b'N' => 'Ν',
        b'O' => 'Ο',
        b'P' => 'Π',
        b'Q' => 'Θ',
        b'R' => 'Ρ',
        b'S' => 'Σ',
        b'T' => 'Τ',
        b'U' => 'Υ',
        b'V' => 'ς',
        b'W' => 'Ω',
        b'X' => 'Ξ',
        b'Y' => 'Ψ',
        b'Z' => 'Ζ',
        b'[' => '[',
        b'\\' => '∴',
        b']' => ']',
        b'^' => '⊥',
        b'_' => '_',
        b'`' => '─', // radicalex — U+FFE3? use U+2500 box drawings
        b'a' => 'α',
        b'b' => 'β',
        b'c' => 'χ',
        b'd' => 'δ',
        b'e' => 'ε',
        b'f' => 'φ',
        b'g' => 'γ',
        b'h' => 'η',
        b'i' => 'ι',
        b'j' => 'ϕ',
        b'k' => 'κ',
        b'l' => 'λ',
        b'm' => 'μ',
        b'n' => 'ν',
        b'o' => 'ο',
        b'p' => 'π',
        b'q' => 'θ',
        b'r' => 'ρ',
        b's' => 'σ',
        b't' => 'τ',
        b'u' => 'υ',
        b'v' => 'ϖ',
        b'w' => 'ω',
        b'x' => 'ξ',
        b'y' => 'ψ',
        b'z' => 'ζ',
        b'{' => '{',
        b'|' => '|',
        b'}' => '}',
        b'~' => '∼',
        0xA1 => 'ℵ', // aleph
        0xA2 => 'ℜ', // real
        0xA3 => 'ℑ', // imaginary
        0xA4 => 'ℓ', // ell
        0xA5 => '℘', // weierstrass p
        0xA6 => '⊕',
        0xA7 => '⊗',
        0xA8 => '∅',
        0xA9 => '∩',
        0xAA => '∪',
        0xAB => '⊃',
        0xAC => '⊇',
        0xAD => '⊄',
        0xAE => '⊆',
        0xAF => '∈',
        0xB0 => '∠',
        0xB1 => '∇',
        0xB2 => '∏', // product (Pi)
        0xB3 => '√',
        0xB4 => '⋅',
        0xB5 => '¬',
        0xB6 => '∧',
        0xB7 => '∨',
        0xB8 => '⇔',
        0xB9 => '⇐',
        0xBA => '⇒',
        0xBB => '↔',
        0xBC => '↕',
        0xBD => '←',
        0xBE => '↑',
        0xBF => '→',
        0xC0 => '↓',
        0xC1 => '↖',
        0xC2 => '↗',
        0xC3 => '↘',
        0xC4 => '↙',
        0xC5 => '∂',
        0xC6 => '■',
        0xC7 => '┐',
        0xC8 => '└',
        0xC9 => '┘',
        0xCA => '┌',
        0xCB => '┼',
        0xCC => '⎯', // horiz scan
        0xCD => '─',
        0xCE => '█',
        0xD0..=0xD6 => char::from_u32(0x391 + (byte - 0xD0) as u32)?, // Α-Ζ (with gaps, may be None)
        0xD7 => '×',
        0xD8 => 'Ø', // is this right? probably skip
        0xE0..=0xE9 => char::from_u32(0x3B1 + (byte - 0xE0) as u32)?,
        0xEA => '∈',
        0xEB => '∪',
        0xEC => '∝',
        0xED => '∼',
        0xEE => '≍',
        0xEF => '≈',
        0xF0 => '≡',
        0xF1 => '≠',
        0xF2 => '≥',
        0xF3 => '≤',
        0xF4 => '>',
        0xF5 => '∋',
        0xF6 => '∀',
        0xF7 => '∂',
        0xF8 => '∫',
        0xF9 => '÷',
        0xFA => '√',
        0xFB => '∇',
        0xFC => '⌋',
        0xFD => '⌈',
        0xFE => '∩',
        _ => return None,
    })
}

/// Adobe ZapfDingbats font encoding (PDF 1.7 Appendix D.6).
/// Codes 0x21-0xFE map to glyphs in the Unicode Dingbats block (U+2700-27BF)
/// and a few others.
fn zapfdingbats_decode(byte: u8) -> Option<char> {
    Some(match byte {
        0x20 => ' ',
        0x21 => '✁',
        0x22 => '✂',
        0x23 => '✃',
        0x24 => '✄',
        0x25 => '☎',
        0x26 => '✆',
        0x27 => '✇',
        0x28 => '✈',
        0x29 => '✉',
        0x2A => '✊',
        0x2B => '✋',
        0x2C => '✌',
        0x2D => '✍',
        0x2E => '✎',
        0x2F => '✏',
        0x30 => '✐',
        0x31 => '✑',
        0x32 => '✒',
        0x33 => '✓',
        0x34 => '✔',
        0x35 => '✕',
        0x36 => '✖',
        0x37 => '✗',
        0x38 => '✘',
        0x39 => '✙',
        0x3A => '✚',
        0x3B => '✛',
        0x3C => '✜',
        0x3D => '✝',
        0x3E => '✞',
        0x3F => '✟',
        0x40 => '✠',
        0x41 => '✡',
        0x42 => '✢',
        0x43 => '✣',
        0x44 => '✤',
        0x45 => '✥',
        0x46 => '✦',
        0x47 => '✧',
        0x48 => '★',
        0x49 => '✩',
        0x4A => '✪',
        0x4B => '✫',
        0x4C => '✬',
        0x4D => '✭',
        0x4E => '✮',
        0x4F => '✯',
        0x50 => '✰',
        0x51 => '✱',
        0x52 => '✲',
        0x53 => '✳',
        0x54 => '✴',
        0x55 => '✵',
        0x56 => '✶',
        0x57 => '✷',
        0x58 => '✸',
        0x59 => '✹',
        0x5A => '✺',
        0x5B => '✻',
        0x5C => '✼',
        0x5D => '✽',
        0x5E => '✾',
        0x5F => '✿',
        0x60 => '❀',
        0x61 => '❁',
        0x62 => '❂',
        0x63 => '❃',
        0x64 => '❄',
        0x65 => '❅',
        0x66 => '❆',
        0x67 => '❇',
        0x68 => '❈',
        0x69 => '❉',
        0x6A => '❊',
        0x6B => '❋',
        0x6C => '●',
        0x6D => '❍',
        0x6E => '■',
        0x6F => '❏',
        0x70 => '❐',
        0x71 => '❑',
        0x72 => '❒',
        0x73 => '▲',
        0x74 => '▼',
        0x75 => '◆',
        0x76 => '❖',
        0x77 => '◄',
        0x78 => '►',
        0x79 => '❘',
        0x7A => '❙',
        0x7B => '❚',
        0x7C => '❛',
        0x7D => '❜',
        0x7E => '❝',
        0xA1 => '❞',
        0xA2 => '❡',
        0xA3 => '❢',
        0xA4 => '❣',
        0xA5 => '❤',
        0xA6 => '❥',
        0xA7 => '✐',
        0xA8 => '❧',
        0xA9 => '❨',
        0xAA => '❩',
        0xAB => '❪',
        0xAC => '❫',
        0xAD => '❬',
        0xAE => '❭',
        0xAF => '❮',
        0xB0 => '❯',
        0xB1 => '❰',
        0xB2 => '❱',
        0xB3 => '❲',
        0xB4 => '❳',
        0xB5 => '❴',
        0xB6 => '❵',
        0xB7 => '❶',
        0xB8 => '❷',
        0xB9 => '❸',
        0xBA => '❹',
        0xBB => '❺',
        0xBC => '❻',
        0xBD => '❼',
        0xBE => '❽',
        0xBF => '❾',
        0xC0 => '❿',
        0xC1 => '➀',
        0xC2 => '➁',
        0xC3 => '➂',
        0xC4 => '➃',
        0xC5 => '➄',
        0xC6 => '➅',
        0xC7 => '➆',
        0xC8 => '➇',
        0xC9 => '➈',
        0xCA => '➉',
        0xCB => '➊',
        0xCC => '➋',
        0xCD => '➌',
        0xCE => '➍',
        0xCF => '➎',
        0xD0 => '➏',
        0xD1 => '➐',
        0xD2 => '➑',
        0xD3 => '➒',
        0xD4 => '➓',
        0xD5 => '┄',
        0xD6 => '┅',
        0xD7 => '┆',
        0xD8 => '┇',
        0xD9 => '┈',
        0xDA => '┉',
        0xDB => '┊',
        0xDC => '┋',
        0xDD => '╌',
        0xDE => '╍',
        0xDF => '═',
        0xE0 => '│',
        0xE1 => '║',
        0xE2 => '░',
        0xE3 => '▒',
        0xE4 => '▓',
        0xE5 => '█',
        0xE6 => '▌',
        0xE7 => '▐',
        0xE8 => '▀',
        0xE9 => '▄',
        0xEA => '◆',
        0xEB => '◇',
        0xEC => '○',
        0xED => '●',
        0xEE => '◐',
        0xEF => '◑',
        0xF0 => '◒',
        0xF1 => '◓',
        0xF2 => '◔',
        0xF3 => '◕',
        0xF4 => '◖',
        0xF5 => '◗',
        0xF6 => '◘',
        0xF7 => '◙',
        0xF8 => '◢',
        _ => return None,
    })
}

/// TeX Computer Modern Symbol (cmsy) encoding, used by LaTeX for math
/// symbols. Subsetted fonts like `NIWCFP+CMSY8` keep the family prefix
/// and apply the standard OMS encoding. Mappings below are the most
/// commonly encountered glyphs in academic papers (confirmed against
/// `FontDescriptor.CharSet` of typical cmsy8 subsets).
fn cmsy_decode(byte: u8) -> Option<char> {
    Some(match byte {
        0x01 => '′', // prime (U+2032)
        0x02 => '×', // multiply (U+00D7)
        0x0F => '•', // bullet (U+2022)
        _ => return None,
    })
}

// ─── Tests ───

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compute_font_flags_name_bold() {
        // Name-only "Bold" → bold bit (16)
        let f = compute_font_flags("Arial-BoldMT", None);
        assert!(f & 16 != 0, "expected bold bit set, got {f:#b}");
        // Not italic, not serif, not mono
        assert_eq!(f & 2, 0);
        assert_eq!(f & 4, 0);
        assert_eq!(f & 8, 0);
    }

    #[test]
    fn test_compute_font_flags_name_bold_italic() {
        // Bold + Italic in name
        let f = compute_font_flags("Helvetica-BoldOblique", None);
        assert!(f & 16 != 0, "bold");
        assert!(f & 2 != 0, "italic");
    }

    #[test]
    fn test_compute_font_flags_courier_mono() {
        // Courier → mono (8)
        let f = compute_font_flags("Courier-New", None);
        assert!(f & 8 != 0, "mono");
    }

    #[test]
    fn test_compute_font_flags_times_serif() {
        // Times → serif (4)
        let f = compute_font_flags("Times-Roman", None);
        assert!(f & 4 != 0, "serif");
    }

    #[test]
    fn test_compute_font_flags_descriptor_fixed_pitch() {
        // /Flags bit 1 (FixedPitch = 1) → fitz mono (8)
        let f = compute_font_flags("SomeFont", Some(0x01));
        assert!(f & 8 != 0, "FixedPitch should map to mono");
    }

    #[test]
    fn test_compute_font_flags_descriptor_italic() {
        // /Flags bit 7 (Italic = 64) → fitz italic (2)
        let f = compute_font_flags("SomeFont", Some(0x40));
        assert!(f & 2 != 0, "Italic bit should map to fitz italic");
    }

    #[test]
    fn test_compute_font_flags_descriptor_serif() {
        // /Flags bit 2 (Serif = 2) → fitz serif (4)
        let f = compute_font_flags("SomeFont", Some(0x02));
        assert!(f & 4 != 0, "Serif bit should map to fitz serif");
    }

    #[test]
    fn test_compute_font_flags_combined_or() {
        // /Flags combines with name heuristics
        let f = compute_font_flags("ABCDEF+TimesNewRoman-Bold", Some(0x02));
        assert!(f & 16 != 0, "bold from name");
        assert!(f & 4 != 0, "serif from descriptor + name");
    }

    #[test]
    fn test_compute_font_flags_no_signal_zero() {
        assert_eq!(compute_font_flags("Helvetica", None), 0);
    }

    #[test]
    fn test_cmap_parse_simple() {
        let cmap_data = b"1 beginbfchar\n<41> <42>\nendbfchar\n";
        let cmap = parse_cmap(cmap_data).unwrap();
        assert_eq!(cmap.bfchar.len(), 1);
        assert_eq!(cmap.lookup(&[0x41]), Some(vec![0x42]));
    }

    #[test]
    fn test_cmap_range() {
        let cmap_data = b"1 beginbfrange\n<41> <43> <61>\nendbfrange\n";
        let cmap = parse_cmap(cmap_data).unwrap();
        assert_eq!(cmap.lookup(&[0x41]), Some(vec![0x61])); // A → a
        assert_eq!(cmap.lookup(&[0x42]), Some(vec![0x62])); // B → b
        assert_eq!(cmap.lookup(&[0x43]), Some(vec![0x63])); // C → c
        assert_eq!(cmap.lookup(&[0x44]), None);
    }

    #[test]
    fn test_adobe_glyph_lookup() {
        assert_eq!(adobe_glyph_to_char(b"space"), Some(' '));
        assert_eq!(adobe_glyph_to_char(b"A"), None);
        assert_eq!(adobe_glyph_to_char(b"Euro"), Some('€'));
        assert_eq!(adobe_glyph_to_char(b"uni0041"), Some('A'));
    }

    #[test]
    fn test_font_decode_latin() {
        let info = FontInfo {
            base_font: "Helvetica".into(),
            encoding: None,
            is_type0: false,
            is_type3: false,
            widths: vec![],
            first_char: 0,
            default_width: 600.0,
            cmap: None,
            differences: None,
            cid_font: None,
            flags: 0,
        };
        assert_eq!(info.decode_char(&[0x41]), 'A');
        assert_eq!(info.decode_char(&[0x20]), ' ');
    }

    #[test]
    fn test_builtin_symbol_decode() {
        // Greek letters in Symbol font
        let info = FontInfo {
            base_font: "Symbol".into(),
            encoding: None,
            is_type0: false,
            is_type3: false,
            widths: vec![],
            first_char: 0,
            default_width: 600.0,
            cmap: None,
            differences: None,
            cid_font: None,
            flags: 0,
        };
        assert_eq!(info.decode_char(&[0x41]), 'Α'); // Alpha
        assert_eq!(info.decode_char(&[0x61]), 'α'); // alpha
        assert_eq!(info.decode_char(&[0x44]), 'Δ'); // Delta
        assert_eq!(info.decode_char(&[0x50]), 'Π'); // Pi
                                                    // Subset prefix should also work
        let info_subset = FontInfo {
            base_font: "ABCDEF+Symbol".into(),
            ..info
        };
        assert_eq!(info_subset.decode_char(&[0x61]), 'α');
    }

    #[test]
    fn test_builtin_zapfdingbats_decode() {
        // Test the actual case from dbnet_plus.pdf: code 0x46 in Dingbats
        let info = FontInfo {
            base_font: "RPUFKZ+Dingbats".into(),
            encoding: None,
            is_type0: false,
            is_type3: false,
            widths: vec![],
            first_char: 0,
            default_width: 600.0,
            cmap: None,
            differences: None,
            cid_font: None,
            flags: 0,
        };
        assert_eq!(info.decode_char(&[0x46]), '✦'); // BLACK FOUR POINTED STAR
        assert_eq!(info.decode_char(&[0x6C]), '●'); // large black circle
                                                    // Standard name without subset prefix
        let info_plain = FontInfo {
            base_font: "ZapfDingbats".into(),
            ..info
        };
        assert_eq!(info_plain.decode_char(&[0x46]), '✦');
    }

    #[test]
    fn test_builtin_cmsy_decode() {
        // LaTeX Computer Modern Symbol font — the actual case from
        // dbnet_plus.pdf: bullets between author affiliations.
        let info = FontInfo {
            base_font: "NIWCFP+CMSY8".into(),
            encoding: None,
            is_type0: false,
            is_type3: false,
            widths: vec![],
            first_char: 0,
            default_width: 600.0,
            cmap: None,
            differences: None,
            cid_font: None,
            flags: 0,
        };
        assert_eq!(info.decode_char(&[0x0F]), '•'); // bullet
        assert_eq!(info.decode_char(&[0x02]), '×'); // multiply
        assert_eq!(info.decode_char(&[0x01]), '′'); // prime
    }

    #[test]
    fn test_font_decode_with_cmap() {
        let mut bfchar = HashMap::new();
        bfchar.insert(vec![0x00, 0x41], vec![0x00, 0x61]); // A → a

        let info = FontInfo {
            base_font: "TestFont".into(),
            encoding: None,
            is_type0: false,
            is_type3: false,
            widths: vec![],
            first_char: 0,
            default_width: 1000.0,
            cmap: Some(CMap {
                bfchar,
                bfrange: vec![],
            }),
            differences: None,
            cid_font: None,
            flags: 0,
        };
        assert_eq!(info.decode_char(&[0x00, 0x41]), 'a');
    }
}
