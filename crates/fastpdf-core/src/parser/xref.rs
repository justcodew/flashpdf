use crate::parser::object::{parse_object, Cursor, ParseError, ParseResult};
use crate::types::{ObjectId, PdfObject};
use std::collections::HashMap;

/// Type of xref entry
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum XrefEntryType {
    /// Free object (deleted)
    Free,
    /// Normal uncompressed object
    Uncompressed,
    /// Compressed inside an object stream
    Compressed,
}

/// A single xref table entry
#[derive(Debug, Clone, Copy)]
pub struct XrefEntry {
    pub entry_type: XrefEntryType,
    /// For Uncompressed: byte offset in file
    /// For Compressed: object stream number
    pub field1: u32,
    /// For Uncompressed: generation number
    /// For Compressed: index within the object stream
    pub field2: u16,
}

impl XrefEntry {
    pub fn free(gen: u16, next_free: u32) -> Self {
        Self {
            entry_type: XrefEntryType::Free,
            field1: next_free,
            field2: gen,
        }
    }

    pub fn uncompressed(offset: u32, gen: u16) -> Self {
        Self {
            entry_type: XrefEntryType::Uncompressed,
            field1: offset,
            field2: gen,
        }
    }

    pub fn compressed(stream_obj_num: u32, index: u16) -> Self {
        Self {
            entry_type: XrefEntryType::Compressed,
            field1: stream_obj_num,
            field2: index,
        }
    }
}

/// The parsed xref + trailer information from a PDF file.
#[derive(Debug, Clone)]
pub struct XrefTable {
    /// Map from object number to entry
    pub entries: HashMap<u32, XrefEntry>,
    /// /Root reference (the document catalog)
    pub root: ObjectId,
    /// /Size: total number of entries (highest object number + 1)
    pub size: u32,
    /// /Info reference (optional)
    pub info: Option<ObjectId>,
    /// /Encrypt reference (optional — we don't support encryption, but record it)
    pub encrypt: Option<ObjectId>,
}

impl XrefTable {
    /// Look up an object's xref entry.
    pub fn get(&self, obj_num: u32) -> Option<&XrefEntry> {
        self.entries.get(&obj_num)
    }
}

// ─── Standard xref table parsing ───

/// Parse a standard (text-format) xref table.
/// `data` is the entire file. `startxref_offset` is the byte offset of the "xref" keyword.
pub fn parse_xref_table(data: &[u8], startxref_offset: usize) -> ParseResult<XrefTable> {
    let mut cur = Cursor::new(&data[startxref_offset..]);

    // Expect "xref"
    if !cur.remaining().starts_with(b"xref") {
        return Err(ParseError::Message("expected 'xref' keyword"));
    }
    cur.advance(4);
    cur.skip_ws();

    let mut all_entries: HashMap<u32, XrefEntry> = HashMap::new();

    // Parse subsections: "start_obj_num count" followed by count entries
    loop {
        cur.skip_ws();
        let remaining = cur.remaining();

        // Check if we've hit "trailer"
        if remaining.starts_with(b"trailer") {
            break;
        }

        // Parse start object number and count
        let start_obj = parse_positive_int(&mut cur)?;
        cur.skip_ws();
        let count = parse_positive_int(&mut cur)?;
        cur.skip_ws();

        for i in 0..count {
            let obj_num = (start_obj + i) as u32;
            // Each entry: "offset gen n/f \r?\n" (exactly 20 bytes per spec, but we parse flexibly)
            let entry_offset = parse_positive_int(&mut cur)? as u32;
            cur.skip_ws();
            let gen = parse_positive_int(&mut cur)? as u16;
            cur.skip_ws();

            let in_use = match cur.peek() {
                Some(b'n') => true,
                Some(b'f') => false,
                _ => return Err(ParseError::Message("expected 'n' or 'f' in xref entry")),
            };
            cur.advance(1);

            // Skip to next line (handle \r\n, \n, \r)
            cur.skip_ws();

            if in_use {
                all_entries.insert(obj_num, XrefEntry::uncompressed(entry_offset, gen));
            } else {
                all_entries.insert(obj_num, XrefEntry::free(gen, entry_offset));
            }
        }
    }

    // Parse trailer dictionary
    cur.skip_ws();
    if !cur.remaining().starts_with(b"trailer") {
        return Err(ParseError::Message("expected 'trailer' keyword"));
    }
    cur.advance(7);

    let trailer_obj = parse_object(&mut cur)?;
    let trailer_dict = match &trailer_obj {
        PdfObject::Dict(d) => d,
        _ => return Err(ParseError::Message("trailer must be a dictionary")),
    };

    let (root, size, info, encrypt) = extract_trailer_fields(trailer_dict)?;

    // Merge entries (later xref sections override earlier ones via /Prev chain)
    Ok(XrefTable {
        entries: all_entries,
        root,
        size,
        info,
        encrypt,
    })
}

// ─── Xref stream parsing (PDF 1.5+) ───

/// Parse a cross-reference stream.
/// The stream object must already be parsed (dict + data).
pub fn parse_xref_stream_obj(
    stream_dict: &[(&[u8], PdfObject<'_>)],
    stream_data: &[u8],
) -> ParseResult<XrefTable> {
    // Extract /W array: [W1 W2 W3]
    let w = extract_w_array(stream_dict)?;
    // Extract /Size
    let size = extract_field_u32(stream_dict, b"Size")?;
    // Extract /Index array (optional, defaults to [0 Size])
    let index = extract_index_array(stream_dict).unwrap_or_else(|| vec![(0u32, size)]);
    // Extract /Root
    let root = extract_ref_field(stream_dict, b"Root")?;
    // Extract /Info (optional)
    let info = extract_ref_field_opt(stream_dict, b"Info");
    // Extract /Encrypt (optional)
    let encrypt = extract_ref_field_opt(stream_dict, b"Encrypt");

    // Compute total entry count from /Index
    let total_count: u32 = index.iter().map(|(_, count)| count).sum();

    // Each entry is W1+W2+W3 bytes
    let entry_size = (w[0] + w[1] + w[2]) as usize;
    if stream_data.len() < total_count as usize * entry_size {
        return Err(ParseError::Message("xref stream data too short"));
    }

    let mut entries: HashMap<u32, XrefEntry> = HashMap::new();
    let mut pos = 0usize;
    let mut obj_num: u32;

    for (start, count) in &index {
        obj_num = *start;
        for _ in 0..*count {
            let entry = parse_xref_stream_entry(&stream_data[pos..], w)?;
            entries.insert(obj_num, entry);
            pos += entry_size;
            obj_num += 1;
        }
    }

    Ok(XrefTable {
        entries,
        root,
        size,
        info,
        encrypt,
    })
}

/// Parse a single entry from xref stream data.
/// w = [W1, W2, W3] field widths in bytes.
fn parse_xref_stream_entry(data: &[u8], w: [u16; 3]) -> ParseResult<XrefEntry> {
    let mut pos = 0;

    // Field 1: type (default 1 if W1=0)
    let f1 = if w[0] == 0 {
        1u32
    } else {
        read_uint_field(&data[pos..pos + w[0] as usize])?
    };
    pos += w[0] as usize;

    // Field 2
    let f2 = read_uint_field(&data[pos..pos + w[1] as usize])? as u32;
    pos += w[1] as usize;

    // Field 3
    let f3 = read_uint_field(&data[pos..pos + w[2] as usize])? as u16;

    match f1 {
        0 => Ok(XrefEntry::free(f3, f2)),
        1 => Ok(XrefEntry::uncompressed(f2, f3)),
        2 => Ok(XrefEntry::compressed(f2, f3)),
        _ => Err(ParseError::Message("unknown xref stream entry type")),
    }
}

/// Read an unsigned integer from big-endian bytes.
fn read_uint_field(data: &[u8]) -> ParseResult<u32> {
    let mut val: u32 = 0;
    for &b in data {
        val = val.wrapping_shl(8) | (b as u32);
    }
    Ok(val)
}

// ─── Trailer parsing & chain walking ───

/// Parse trailer fields from a dictionary.
fn extract_trailer_fields(
    dict: &[(&[u8], PdfObject<'_>)],
) -> ParseResult<(ObjectId, u32, Option<ObjectId>, Option<ObjectId>)> {
    let root = extract_ref_field(dict, b"Root")?;
    let size = extract_field_u32(dict, b"Size")?;
    let info = extract_ref_field_opt(dict, b"Info");
    let encrypt = extract_ref_field_opt(dict, b"Encrypt");
    Ok((root, size, info, encrypt))
}

fn extract_ref_field(dict: &[(&[u8], PdfObject<'_>)], key: &[u8]) -> ParseResult<ObjectId> {
    for (k, v) in dict {
        if *k == key {
            if let PdfObject::Ref(id) = v {
                return Ok(*id);
            }
            return Err(ParseError::Message("expected reference for trailer field"));
        }
    }
    Err(ParseError::Message("missing required trailer field"))
}

fn extract_ref_field_opt(dict: &[(&[u8], PdfObject<'_>)], key: &[u8]) -> Option<ObjectId> {
    for (k, v) in dict {
        if *k == key {
            if let PdfObject::Ref(id) = v {
                return Some(*id);
            }
        }
    }
    None
}

fn extract_field_u32(dict: &[(&[u8], PdfObject<'_>)], key: &[u8]) -> ParseResult<u32> {
    for (k, v) in dict {
        if *k == key {
            return v
                .as_i64()
                .map(|n| n as u32)
                .ok_or(ParseError::Message("expected integer for trailer field"));
        }
    }
    Err(ParseError::Message("missing required trailer field"))
}

fn extract_w_array(dict: &[(&[u8], PdfObject<'_>)]) -> ParseResult<[u16; 3]> {
    for (k, v) in dict {
        if *k == b"W" {
            if let PdfObject::Array(arr) = v {
                if arr.len() >= 3 {
                    return Ok([
                        arr[0].as_i64().unwrap_or(0) as u16,
                        arr[1].as_i64().unwrap_or(0) as u16,
                        arr[2].as_i64().unwrap_or(0) as u16,
                    ]);
                }
            }
            return Err(ParseError::Message("/W must be an array of 3 integers"));
        }
    }
    Err(ParseError::Message("missing /W in xref stream"))
}

fn extract_index_array(dict: &[(&[u8], PdfObject<'_>)]) -> Option<Vec<(u32, u32)>> {
    for (k, v) in dict {
        if *k == b"Index" {
            if let PdfObject::Array(arr) = v {
                let mut result = Vec::new();
                let mut i = 0;
                while i + 1 < arr.len() {
                    let start = arr[i].as_i64().unwrap_or(0) as u32;
                    let count = arr[i + 1].as_i64().unwrap_or(0) as u32;
                    result.push((start, count));
                    i += 2;
                }
                return Some(result);
            }
        }
    }
    None
}

// ─── startxref location ───

/// Find the `startxref` value at the end of the PDF file.
/// Returns the byte offset that startxref points to.
pub fn find_startxref(data: &[u8]) -> ParseResult<usize> {
    // Search backwards from end of file for "startxref"
    let needle = b"startxref";
    let search_area = &data[data.len().saturating_sub(1024)..];

    let mut found = None;
    let mut offset = 0;
    while offset <= search_area.len().saturating_sub(needle.len()) {
        if let Some(pos) = memchr::memmem::find(&search_area[offset..], needle) {
            found = Some(offset + pos);
            offset += pos + needle.len();
        } else {
            break;
        }
    }

    let found = found.ok_or(ParseError::Message("startxref not found"))?;
    let after_keyword = &search_area[found + needle.len()..];

    // Parse the integer after startxref
    let mut cur = Cursor::new(after_keyword);
    cur.skip_ws();
    let xref_offset = parse_positive_int(&mut cur)? as usize;

    Ok(xref_offset)
}

/// Determine if the xref at the given offset is a standard table or a stream object.
/// Returns `true` if it starts with "xref", `false` if it's an xref stream (object).
pub fn is_standard_xref(data: &[u8], offset: usize) -> bool {
    let remaining = &data[offset..];
    remaining.starts_with(b"xref")
}

// ─── Object stream (ObjStm) parsing ───

/// Parsed object stream: contains N embedded objects.
pub struct ObjStm<'a> {
    /// The embedded objects, keyed by their object number
    pub objects: HashMap<u32, PdfObject<'a>>,
}

/// Parse an object stream (Type /ObjStm).
/// `stream_dict` is the stream's dictionary, `stream_data` is the decompressed data.
pub fn parse_objstm<'a>(
    stream_dict: &[(&[u8], PdfObject<'a>)],
    stream_data: &'a [u8],
) -> ParseResult<ObjStm<'a>> {
    let n = extract_field_u32(stream_dict, b"N")? as usize;
    let first = extract_field_u32(stream_dict, b"First")? as usize;

    // Parse the N pairs of (obj_num, offset) at the beginning
    let mut cur = Cursor::new(&stream_data[..first]);
    let mut obj_offsets: Vec<(u32, usize)> = Vec::with_capacity(n);

    for _ in 0..n {
        cur.skip_ws();
        let obj_num = parse_positive_int(&mut cur)? as u32;
        cur.skip_ws();
        let offset = parse_positive_int(&mut cur)? as usize;
        obj_offsets.push((obj_num, first + offset));
    }

    // Parse each embedded object
    let mut objects = HashMap::new();
    let data_start = &stream_data[first..];

    for (i, (obj_num, _abs_offset)) in obj_offsets.iter().enumerate() {
        // Calculate the byte range for this object
        let start = if i == 0 {
            0
        } else {
            obj_offsets[i - 1].1 - first
        };
        let end = if i + 1 < obj_offsets.len() {
            obj_offsets[i + 1].1 - first
        } else {
            data_start.len()
        };

        if start > data_start.len() || end > data_start.len() || start >= end {
            continue;
        }

        let obj_data = &data_start[start..end];
        match parse_object_from_slice(obj_data) {
            Ok(obj) => {
                objects.insert(*obj_num, obj);
            }
            Err(_) => {
                // Skip unparseable objects (tolerant)
            }
        }
    }

    Ok(ObjStm { objects })
}

/// Helper: parse a positive integer from a cursor.
pub fn parse_positive_int_from_cursor(cur: &mut Cursor<'_>) -> ParseResult<i64> {
    parse_positive_int(cur)
}

/// Internal helper: parse a positive integer from a cursor.
fn parse_positive_int(cur: &mut Cursor<'_>) -> ParseResult<i64> {
    let start = cur.pos();
    while let Some(b) = cur.peek() {
        if b.is_ascii_digit() {
            cur.advance(1);
        } else {
            break;
        }
    }
    if cur.pos() == start {
        return Err(ParseError::InvalidNumber);
    }
    let slice = cur.slice_from(start);
    let s = std::str::from_utf8(slice).unwrap_or("0");
    s.parse().map_err(|_| ParseError::InvalidNumber)
}

/// Parse an object from a raw byte slice (for ObjStm embedded objects).
fn parse_object_from_slice(data: &[u8]) -> ParseResult<PdfObject<'_>> {
    let mut cur = Cursor::new(data);
    parse_object(&mut cur)
}

// ─── Decompression helper ───

/// Decompress FlateDecode stream data.
pub fn decompress_flate(data: &[u8]) -> ParseResult<Vec<u8>> {
    use flate2::read::ZlibDecoder;
    use std::io::Read;

    let mut decoder = ZlibDecoder::new(data);
    let mut output = Vec::new();
    decoder
        .read_to_end(&mut output)
        .map_err(|_| ParseError::Message("flate decompression failed"))?;
    Ok(output)
}

/// Decompress LZWDecode stream data.
/// Implements the PDF LZW variant (early code change, MSB bit packing).
pub fn decompress_lzw(data: &[u8]) -> ParseResult<Vec<u8>> {
    if data.is_empty() {
        return Ok(Vec::new());
    }

    let mut output = Vec::new();
    let mut table: Vec<Vec<u8>> = Vec::new();

    // Initialize table with single-byte entries 0-255
    for i in 0u16..256 {
        table.push(vec![i as u8]);
    }
    // 256 = clear table, 257 = EOD
    table.push(Vec::new()); // 256 - clear
    table.push(Vec::new()); // 257 - EOD

    let mut bit_pos = 0usize;
    let mut code_size = 9u32;
    let mut prev_code: Option<u16> = None;

    loop {
        if bit_pos + code_size as usize > data.len() * 8 {
            break;
        }

        // Read code (MSB first)
        let code = read_lzw_code(data, bit_pos, code_size as usize) as u16;
        bit_pos += code_size as usize;

        if code == 257 {
            // End of data
            break;
        }

        if code == 256 {
            // Clear table
            table.truncate(258);
            code_size = 9;
            prev_code = None;
            continue;
        }

        if let Some(prev) = prev_code {
            let mut entry = table[prev as usize].clone();

            if (code as usize) < table.len() {
                // Code exists in table
                let current = table[code as usize].clone();
                output.extend_from_slice(&current);
                entry.push(current[0]);
            } else {
                // Code not in table: prev + first byte of prev
                let first_byte = entry[0];
                entry.push(first_byte);
                output.extend_from_slice(&entry);
            }

            // Add to table
            table.push(entry);

            // Check if we need to increase code size
            if table.len() >= (1 << code_size) && code_size < 12 {
                code_size += 1;
            }
        } else {
            // First code
            if (code as usize) < table.len() {
                output.extend_from_slice(&table[code as usize]);
            }
        }

        prev_code = Some(code);
    }

    Ok(output)
}

/// Read a code of `bits` length from `data` starting at `bit_pos` (MSB first).
fn read_lzw_code(data: &[u8], bit_pos: usize, bits: usize) -> u32 {
    let byte_pos = bit_pos / 8;
    let bit_offset = bit_pos % 8;

    // Read up to 4 bytes to cover the code
    let mut val: u32 = 0;
    for i in 0..4 {
        if byte_pos + i < data.len() {
            val = (val << 8) | (data[byte_pos + i] as u32);
        } else {
            val <<= 8;
        }
    }

    // Shift to get the code
    let shift = 32 - bit_offset - bits;
    (val >> shift) & ((1u32 << bits) - 1)
}

/// Decode ASCII85Decode stream data.
pub fn decode_ascii85(data: &[u8]) -> ParseResult<Vec<u8>> {
    let mut output = Vec::new();
    let mut group = [0u8; 5];
    let mut group_len = 0;

    let mut i = 0;
    while i < data.len() {
        let b = data[i];
        i += 1;

        match b {
            b'~' => {
                // Check for end-of-data marker "~>"
                if i < data.len() && data[i] == b'>' {
                    break;
                }
                return Err(ParseError::Message("invalid ASCII85: lone ~"));
            }
            b'z' => {
                // Special case: 'z' represents 4 zero bytes
                if group_len > 0 {
                    return Err(ParseError::Message("invalid ASCII85: z mid-group"));
                }
                output.extend_from_slice(&[0, 0, 0, 0]);
            }
            0x21..=0x75 => {
                // Valid ASCII85 digit (33-117 maps to 0-84)
                if group_len < 5 {
                    group[group_len] = b - 0x21;
                    group_len += 1;
                }
                if group_len == 5 {
                    let val = group[0] as u64 * 85u64.pow(4)
                        + group[1] as u64 * 85u64.pow(3)
                        + group[2] as u64 * 85u64.pow(2)
                        + group[3] as u64 * 85
                        + group[4] as u64;
                    output.push((val >> 24) as u8);
                    output.push((val >> 16) as u8);
                    output.push((val >> 8) as u8);
                    output.push(val as u8);
                    group_len = 0;
                }
            }
            // Whitespace is ignored
            b' ' | b'\t' | b'\n' | b'\r' => {}
            _ => {
                return Err(ParseError::Message("invalid ASCII85 character"));
            }
        }
    }

    // Handle partial group (less than 5 chars means fewer output bytes)
    if group_len > 0 {
        // Pad with 'u' (84) for missing chars
        for j in group_len..5 {
            group[j] = 84;
        }
        let val = group[0] as u64 * 85u64.pow(4)
            + group[1] as u64 * 85u64.pow(3)
            + group[2] as u64 * 85u64.pow(2)
            + group[3] as u64 * 85
            + group[4] as u64;
        // Output only group_len - 1 bytes for partial group
        let bytes_to_output = group_len - 1;
        for j in 0..bytes_to_output {
            output.push((val >> (24 - j * 8)) as u8);
        }
    }

    Ok(output)
}

/// Decode RunLengthDecode stream data.
pub fn decode_run_length(data: &[u8]) -> ParseResult<Vec<u8>> {
    let mut output = Vec::new();
    let mut i = 0;

    while i < data.len() {
        let length = data[i] as i8;
        i += 1;

        if length == -128i8 {
            // EOD marker
            break;
        } else if length >= 0 {
            // Copy next (length + 1) bytes literally
            let count = (length as usize) + 1;
            if i + count > data.len() {
                return Err(ParseError::Message("RunLength: unexpected end of data"));
            }
            output.extend_from_slice(&data[i..i + count]);
            i += count;
        } else {
            // Repeat next byte (1 - length) times
            if i >= data.len() {
                return Err(ParseError::Message("RunLength: unexpected end of data"));
            }
            let byte = data[i];
            i += 1;
            let count = (1 - length as i32) as usize;
            output.extend(std::iter::repeat_n(byte, count));
        }
    }

    Ok(output)
}

/// Decompress a stream using its /Filter specification.
/// Supports single filter or array of filters (applied in order).
pub fn decompress_stream(data: &[u8], filter: &PdfObject<'_>) -> ParseResult<Vec<u8>> {
    match filter {
        PdfObject::Name(name) => apply_single_filter(data, name),
        PdfObject::Array(filters) => {
            // Apply filters in order
            let mut current = data.to_vec();
            for f in filters {
                if let Some(name) = f.as_name() {
                    current = apply_single_filter(&current, name)?;
                }
            }
            Ok(current)
        }
        _ => Ok(data.to_vec()),
    }
}

fn apply_single_filter(data: &[u8], name: &[u8]) -> ParseResult<Vec<u8>> {
    match name {
        b"FlateDecode" => decompress_flate(data),
        b"LZWDecode" => decompress_lzw(data),
        b"ASCII85Decode" => decode_ascii85(data),
        b"RunLengthDecode" => decode_run_length(data),
        b"ASCIIHexDecode" => decode_ascii_hex(data),
        _ => Ok(data.to_vec()), // Unknown filter: return raw data
    }
}

/// Decode ASCIIHexDecode stream data.
fn decode_ascii_hex(data: &[u8]) -> ParseResult<Vec<u8>> {
    let mut output = Vec::new();
    let mut nibbles = Vec::new();

    for &b in data {
        match b {
            b'0'..=b'9' => nibbles.push(b - b'0'),
            b'a'..=b'f' => nibbles.push(b - b'a' + 10),
            b'A'..=b'F' => nibbles.push(b - b'A' + 10),
            b'>' => break, // End of hex string
            _ => {}        // Ignore whitespace and other chars
        }
    }

    // Pad odd-length with trailing 0
    if nibbles.len() % 2 != 0 {
        nibbles.push(0);
    }

    for chunk in nibbles.chunks(2) {
        output.push((chunk[0] << 4) | chunk[1]);
    }

    Ok(output)
}
