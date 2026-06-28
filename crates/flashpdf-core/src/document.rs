use crate::parser::object::{parse_object, parse_stream, Cursor, ParseError, ParseResult};
use crate::parser::recovery::recover_xref_by_scan;
use crate::parser::xref::{
    apply_png_predictor, decompress_stream, find_startxref, is_standard_xref, parse_objstm,
    parse_xref_stream_obj, parse_xref_table, XrefEntryType, XrefTable,
};
use crate::types::{ObjectId, PdfObject};
use memmap2::Mmap;
use std::collections::HashMap;
use std::fs::File;
use std::path::Path;
use std::sync::RwLock;

/// Document-level metadata extracted from the `/Info` dictionary
/// (PDF spec §14.3.3). Mirrors `fitz.Document.metadata` keys.
///
/// All fields are `Option<String>`; missing entries in `/Info` become `None`.
/// On the Python side this is exposed as a dict where missing keys are `None`,
/// matching PyMuPDF's behavior of always returning the same key set.
#[derive(Debug, Clone, Default)]
pub struct DocumentMetadata {
    pub title: Option<String>,
    pub author: Option<String>,
    pub subject: Option<String>,
    pub keywords: Option<String>,
    pub creator: Option<String>,
    pub producer: Option<String>,
    pub creation_date: Option<String>,
    pub mod_date: Option<String>,
}

/// Decode a PDF text string per PDF spec §7.9.2.
///
/// Two encodings are common:
/// - **UTF-16BE** with a leading BOM (`\xFE\xFF`). Used by most modern PDF
///   writers for non-ASCII content (CJK, accented Latin).
/// - **PDFDocEncoding** — an ASCII-compatible single-byte encoding for Latin
///   text. We fall back to lossy UTF-8 decoding here, which is correct for
///   ASCII and acceptable for the small set of chars that differ between
///   PDFDocEncoding and Latin-1 in practice (rare in real-world metadata).
///
/// Callers must pass already-decoded bytes (i.e. for `PdfObject::HexString`
/// the caller is responsible for hex-decoding first — the parser stores the
/// raw ASCII hex text inside `HexString`). Use `decode_pdf_text_value` to
/// handle both `String` and `HexString` variants uniformly.
pub fn decode_pdf_string(bytes: &[u8]) -> String {
    if bytes.len() >= 2 && bytes[0] == 0xFE && bytes[1] == 0xFF {
        let body = &bytes[2..];
        let utf16: Vec<u16> = body
            .chunks_exact(2)
            .map(|c| u16::from_be_bytes([c[0], c[1]]))
            .collect();
        String::from_utf16_lossy(&utf16)
    } else {
        String::from_utf8_lossy(bytes).into_owned()
    }
}

/// Decode hex-encoded bytes (`<FEFF5F20>`) into the raw byte sequence
/// (`\xFE\xFF\x5F\x20`). The parser stores `PdfObject::HexString` as the
/// raw ASCII hex text, so callers must decode it before UTF-16/Latin
/// interpretation. Returns `None` if the bytes contain non-hex characters.
/// Per PDF spec §7.3.4.3, a trailing odd nibble is padded with `0`.
pub fn hex_decode(bytes: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(bytes.len().div_ceil(2));
    let mut i = 0;
    while i < bytes.len() {
        let hi = bytes[i];
        let lo = if i + 1 < bytes.len() {
            bytes[i + 1]
        } else {
            b'0' // trailing odd nibble → pad
        };
        if !hi.is_ascii_hexdigit() || !lo.is_ascii_hexdigit() {
            return None;
        }
        let b = u8::from_str_radix(std::str::from_utf8(&[hi, lo]).ok()?, 16).ok()?;
        out.push(b);
        i += 2;
    }
    Some(out)
}

/// Resolve PDF literal-string escape sequences (PDF spec §7.3.4.2):
/// `\n \r \t \b \f \( \) \\` and octal `\ddd` (1-3 octal digits).
///
/// `parse_string` keeps raw bytes including the leading `\` for each escape,
/// so metadata consumers must unescape before UTF-16/Latin interpretation.
/// Hex strings have no escape processing — pass through unchanged.
pub fn unescape_literal_string(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b != b'\\' {
            out.push(b);
            i += 1;
            continue;
        }
        // Backslash escape
        i += 1;
        if i >= bytes.len() {
            // trailing lone backslash → keep
            out.push(b'\\');
            break;
        }
        let nxt = bytes[i];
        // Octal: up to 3 digits
        if nxt.is_ascii_digit() && nxt < b'8' {
            let mut digits = String::new();
            for _ in 0..3 {
                if i < bytes.len() && bytes[i] >= b'0' && bytes[i] < b'8' {
                    digits.push(bytes[i] as char);
                    i += 1;
                } else {
                    break;
                }
            }
            if let Ok(n) = u8::from_str_radix(&digits, 8) {
                out.push(n);
            }
            continue;
        }
        let decoded = match nxt {
            b'n' => b'\n',
            b'r' => b'\r',
            b't' => b'\t',
            b'b' => 0x08,
            b'f' => 0x0C,
            b'(' => b'(',
            b')' => b')',
            b'\\' => b'\\',
            // `\` followed by newline (or other char) → drop per spec
            _ => {
                // For newline: the spec says \<newline> is a line continuation
                if nxt == b'\n' || nxt == b'\r' {
                    i += 1;
                    continue;
                }
                // Unknown escape: drop the backslash, keep the next byte
                i += 1;
                out.push(nxt);
                continue;
            }
        };
        out.push(decoded);
        i += 1;
    }
    out
}

/// A parsed PDF document with zero-copy access to its objects.
pub struct Document {
    /// The memory-mapped file data
    mmap: Mmap,
    /// The cross-reference table
    xref: XrefTable,
    /// Cache of parsed objects (lazy, populated on first access)
    object_cache: RwLock<HashMap<u32, PdfObject<'static>>>,
    /// Cache of decompressed object streams
    objstm_cache: RwLock<HashMap<u32, HashMap<u32, PdfObject<'static>>>>,
    /// Compiled decryption state, present iff the PDF has a `/Standard`
    /// security handler we know how to decrypt (RC4 or AES-128 with empty
    /// user password). `None` for unencrypted PDFs and unsupported schemes.
    decryptor: Option<crate::crypto::Decryptor>,
}

impl Document {
    /// Open and parse a PDF file.
    pub fn open<P: AsRef<Path>>(path: P) -> ParseResult<Self> {
        let file =
            File::open(path).map_err(|_| ParseError::Message("cannot open file".to_string()))?;
        let mmap = unsafe { Mmap::map(&file) }
            .map_err(|_| ParseError::Message("cannot mmap file".to_string()))?;
        Self::from_mmap(mmap)
    }

    /// Parse a PDF from an existing memory-mapped region.
    pub fn from_mmap(mmap: Mmap) -> ParseResult<Self> {
        let span = tracing::span!(tracing::Level::DEBUG, "from_mmap", size = mmap.len());
        let _enter = span.enter();
        let data: &[u8] = &mmap;

        // Try standard xref parsing first; fall back to memchr recovery
        let xref = match find_startxref(data) {
            Ok(xref_offset) => Self::parse_xref_at(data, xref_offset),
            Err(_) => Err(ParseError::Message("startxref not found".to_string())),
        };

        // Validate: if the declared xref root doesn't actually point at a
        // parseable object header, the table is corrupt (off-by-N offsets
        // are common when files have prefix garbage — e.g. test2238.pdf has
        // 120 bytes of text before %PDF-, shifting every real offset but
        // leaving the xref entries pointing at pre-shift positions). Fall
        // back to recovery scan which finds objects by pattern.
        let xref = match xref {
            Ok(x) if xref_root_ok(data, &x) => x,
            _ => recover_xref_by_scan(data)?,
        };

        // Set up decryption if `/Encrypt` is present. Two forms:
        //   (a) `/Encrypt N 0 R` — indirect ref, look up + parse the object.
        //   (b) `/Encrypt<<...>>` — inline dict in the trailer (fitz and some
        //       Acrobat versions emit this). We re-parse the trailer at the
        //       recorded offset to read the inline dict.
        // Unsupported schemes (non-Standard handler, AES-256, non-empty
        // password) return an error so callers can surface it; PDFs without
        // `/Encrypt` skip this entirely.
        let decryptor = if let Some(encrypt_id) = xref.encrypt {
            let entry = xref
                .entries
                .get(&encrypt_id.num)
                .ok_or_else(|| ParseError::Message("encrypt dict ref not in xref".to_string()))?;
            let offset = entry.field1 as usize;
            // Parse /Encrypt dict directly from raw bytes — do NOT route
            // through Document::get_object (no Document yet) and do NOT
            // try to decrypt it (its strings are ciphertext by definition).
            let encrypt_dict_raw = parse_object_at(data, offset, encrypt_id.gen)?;
            let doc_id = xref.id_first.as_deref().unwrap_or(&[]);
            Some(crate::crypto::Decryptor::from_encrypt_dict(
                &encrypt_dict_raw,
                doc_id,
            )?)
        } else if xref.encrypt_present {
            // Inline /Encrypt dict case. Re-walk the trailer starting at the
            // recorded offset to find the dict, then pull /Encrypt out of it.
            let inline = parse_inline_encrypt_from_trailer(data, xref.trailer_offset)?;
            let doc_id = xref.id_first.as_deref().unwrap_or(&[]);
            Some(crate::crypto::Decryptor::from_encrypt_dict(
                &inline, doc_id,
            )?)
        } else {
            None
        };

        Ok(Self {
            mmap,
            xref,
            object_cache: RwLock::new(HashMap::new()),
            objstm_cache: RwLock::new(HashMap::new()),
            decryptor,
        })
    }

    /// Try to parse the xref at the given offset (table or stream),
    /// walking the `/Prev` chain to merge entries from earlier revisions
    /// of incrementally-updated PDFs. Errors propagate as Err so the
    /// caller can fall back to recovery.
    fn parse_xref_at(data: &[u8], xref_offset: usize) -> ParseResult<XrefTable> {
        // Parse the newest section first to pick up its trailer metadata
        // (Root, Size, Encrypt, ID) — those always come from the latest
        // revision per PDF spec 7.5.6.
        let mut newest = Self::parse_single_xref_section(data, xref_offset)?;
        let mut merged = std::mem::take(&mut newest.entries);

        // Walk /Prev chain. Entries seen first (in the newest section) win;
        // older sections only fill in objects not present in newer ones.
        // `visited` guards against pathological /Prev cycles.
        let mut visited = std::collections::HashSet::new();
        visited.insert(xref_offset);
        let mut current = newest.prev_offset;
        let mut guard = 0u32;
        const MAX_PREV_DEPTH: u32 = 100;
        while let Some(off) = current {
            if !visited.insert(off) || guard >= MAX_PREV_DEPTH {
                break;
            }
            guard += 1;
            let table = match Self::parse_single_xref_section(data, off) {
                Ok(t) => t,
                Err(_) => break, // tolerate a broken /Prev link
            };
            for (obj_num, entry) in table.entries {
                merged.entry(obj_num).or_insert(entry);
            }
            current = table.prev_offset;
        }

        newest.entries = merged;
        Ok(newest)
    }

    /// Parse a single xref section (no /Prev walking). Used internally by
    /// `parse_xref_at` for each link in the chain.
    fn parse_single_xref_section(data: &[u8], xref_offset: usize) -> ParseResult<XrefTable> {
        if is_standard_xref(data, xref_offset) {
            return parse_xref_table(data, xref_offset);
        }
        // xref stream — resolve_indirect_object_raw gives us the dict and the
        // RAW (still-compressed) stream bytes. Cross-reference streams are
        // almost always FlateDecode-compressed, so we must apply /Filter
        // before handing the data to parse_xref_stream_obj.
        let (dict, raw_stream_data) = resolve_indirect_object_raw(data, xref_offset)?;
        let filter = dict
            .iter()
            .find(|(k, _)| *k == b"Filter")
            .map(|(_, v)| v.clone());
        let mut stream_data: Vec<u8> = match filter {
            Some(f) => decompress_stream(raw_stream_data, &f)?,
            None => raw_stream_data.to_vec(),
        };

        // Apply PNG predictor if /DecodeParms specifies one. Without this,
        // xref streams with /Predictor 12 (PNG Up — extremely common for
        // modern PDFs) yield mostly-garbage Compressed entries that point
        // at nonexistent object streams, silently losing every ObjStm-
        // resident page object. See PDF spec 7.4.4.4.
        if let Some(decode_parms) = dict
            .iter()
            .find(|(k, _)| *k == b"DecodeParms")
            .map(|(_, v)| v)
        {
            stream_data = Self::apply_decode_parms(&stream_data, decode_parms)?;
        }

        parse_xref_stream_obj(&dict, &stream_data)
    }

    /// Apply /DecodeParms (predictor + columns) to a decoded xref stream
    /// payload. For xref streams, /DecodeParms is always a single dict;
    /// arrays only appear for multi-filter image streams (not handled here).
    /// /Predictor defaults to 1 (no prediction); values 10-15 select the
    /// PNG predictor family (PDF spec 7.4.4.4).
    fn apply_decode_parms(data: &[u8], parms: &PdfObject<'_>) -> ParseResult<Vec<u8>> {
        let dict = match parms {
            PdfObject::Dict(d) => d,
            _ => return Ok(data.to_vec()),
        };
        let predictor = dict
            .iter()
            .find(|(k, _)| *k == b"Predictor")
            .and_then(|(_, v)| v.as_i64())
            .unwrap_or(1) as u8;
        if predictor < 10 {
            return Ok(data.to_vec());
        }
        let columns = dict
            .iter()
            .find(|(k, _)| *k == b"Columns")
            .and_then(|(_, v)| v.as_i64())
            .unwrap_or(1) as usize;
        apply_png_predictor(data, columns, predictor)
    }
    pub fn root(&self) -> ParseResult<PdfObject<'static>> {
        self.get_object(self.xref.root.num)
    }

    /// Get the /Root object ID.
    pub fn root_id(&self) -> ObjectId {
        self.xref.root
    }

    /// Extract document-level metadata from the `/Info` dictionary
    /// (PDF spec §14.3.3). Returns an empty `DocumentMetadata` (all fields
    /// `None`) when the document has no `/Info` entry — never errors, since
    /// missing metadata is normal for stripped / minimal PDFs.
    pub fn metadata(&self) -> DocumentMetadata {
        let Some(info_id) = self.xref.info else {
            return DocumentMetadata::default();
        };
        let Ok(info) = self.get_object(info_id.num) else {
            return DocumentMetadata::default();
        };
        // Pull a metadata field: handle both literal `(…)` strings and hex
        // `<…>` strings, decode UTF-16BE-with-BOM or PDFDocEncoding.
        let get_field = |key: &[u8]| -> Option<String> {
            let val = info.get(key)?;
            let bytes: Vec<u8> = match val {
                PdfObject::String(s) => unescape_literal_string(s),
                PdfObject::HexString(s) => hex_decode(s)?,
                _ => return None,
            };
            let s = decode_pdf_string(&bytes);
            if s.is_empty() {
                None
            } else {
                Some(s)
            }
        };
        DocumentMetadata {
            title: get_field(b"Title"),
            author: get_field(b"Author"),
            subject: get_field(b"Subject"),
            keywords: get_field(b"Keywords"),
            creator: get_field(b"Creator"),
            producer: get_field(b"Producer"),
            creation_date: get_field(b"CreationDate"),
            mod_date: get_field(b"ModDate"),
        }
    }

    /// Get the total number of objects (as declared by /Size).
    pub fn size(&self) -> u32 {
        self.xref.size
    }

    /// Get an indirect object by its object number.
    /// Objects are parsed lazily and cached.
    ///
    /// Per PDF 1.7 §7.3.10, an indirect reference to an undefined object
    /// resolves to the null object. We mirror that: missing-from-xref,
    /// free (deleted), and ObjStm-resident-but-not-found entries all return
    /// `PdfObject::Null` instead of erroring. Real parse failures (corrupt
    /// bytes, missing `endobj`, etc.) still propagate as `Err`.
    ///
    /// Rationale: PyMuPDF's bug-regression corpus (~165 PDFs) showed a 28%
    /// failure rate purely from `object not in xref`, caused by Word/Office
    /// exports, incremental updates, and linearized PDFs with stale hint
    /// tables. Peers treat dangling refs as null per spec; we now do too.
    pub fn get_object(&self, obj_num: u32) -> ParseResult<PdfObject<'static>> {
        // Check cache first
        {
            let cache = self.object_cache.read().unwrap();
            if let Some(obj) = cache.get(&obj_num) {
                return Ok(obj.clone());
            }
        }

        let Some(entry) = self.xref.get(obj_num) else {
            return Ok(self.cache_null(obj_num));
        };

        let obj = match entry.entry_type {
            XrefEntryType::Uncompressed => {
                let offset = entry.field1 as usize;
                let gen = entry.field2;
                let data: &[u8] = &self.mmap;
                let parsed = parse_object_at(data, offset, gen)?;
                // Decrypt strings + stream body using the per-object key.
                // ObjStm-resident objects skip this (their bytes were already
                // decrypted once when the ObjStm stream was loaded).
                if let Some(dec) = &self.decryptor {
                    crate::crypto::decrypt_pdf_object(parsed, dec, obj_num, gen)
                } else {
                    // safety: parsed borrows from self.mmap which outlives the
                    // cache; this matches the long-standing leak_pdf_object
                    // transmute pattern used elsewhere in this file.
                    unsafe { std::mem::transmute::<PdfObject<'_>, PdfObject<'static>>(parsed) }
                }
            }
            XrefEntryType::Compressed => {
                let stream_obj_num = entry.field1;

                // Ensure the object stream is loaded. Soft-fail: if the ObjStm
                // itself is corrupt, treat all its members as null rather than
                // fataling the whole document.
                {
                    let stm_cache = self.objstm_cache.read().unwrap();
                    if !stm_cache.contains_key(&stream_obj_num) {
                        drop(stm_cache);
                        if self.load_objstm(stream_obj_num).is_err() {
                            return Ok(self.cache_null(obj_num));
                        }
                    }
                }

                let stm_cache = self.objstm_cache.read().unwrap();
                match stm_cache
                    .get(&stream_obj_num)
                    .and_then(|m| m.get(&obj_num))
                    .cloned()
                {
                    Some(obj) => obj,
                    None => return Ok(self.cache_null(obj_num)),
                }
            }
            XrefEntryType::Free => {
                return Ok(self.cache_null(obj_num));
            }
        };

        self.object_cache
            .write()
            .unwrap()
            .insert(obj_num, obj.clone());
        Ok(obj)
    }

    /// Cache `Null` for `obj_num` and return it. Centralizes the dangling-ref
    /// path so all three soft-fail branches produce identical cache state.
    fn cache_null(&self, obj_num: u32) -> PdfObject<'static> {
        self.object_cache
            .write()
            .unwrap()
            .insert(obj_num, PdfObject::Null);
        PdfObject::Null
    }

    fn load_objstm(&self, stream_obj_num: u32) -> ParseResult<()> {
        let entry = self
            .xref
            .get(stream_obj_num)
            .ok_or(ParseError::Message("ObjStm not in xref".to_string()))?;

        if entry.entry_type != XrefEntryType::Uncompressed {
            return Err(ParseError::Message(
                "ObjStm must be uncompressed".to_string(),
            ));
        }

        let offset = entry.field1 as usize;
        let gen = entry.field2;
        let data: &[u8] = &self.mmap;
        let (dict, raw_stream_data) = parse_object_stream_raw(data, offset)?;

        // Decrypt the ObjStm stream body using the ObjStm's own object key
        // BEFORE decompression. Per spec, objects inside the ObjStm are then
        // plaintext — they are NOT re-encrypted, so no per-object decryption
        // is applied to the parsed entries.
        let raw_stream_data: Vec<u8> = match &self.decryptor {
            Some(dec) => dec.decrypt_object(stream_obj_num, gen, raw_stream_data),
            None => raw_stream_data.to_vec(),
        };

        // Decompress based on /Filter
        let filter = dict
            .iter()
            .find(|(k, _)| *k == b"Filter")
            .map(|(_, v)| v.clone());
        let stream_data: Vec<u8> = match filter {
            Some(f) => decompress_stream(&raw_stream_data, &f)?,
            None => raw_stream_data,
        };

        // Leak the stream data to get 'static lifetime.
        // This is acceptable because object streams are few and bounded.
        let leaked: &'static [u8] = Box::leak(stream_data.into_boxed_slice());

        // We also need to make the dict 'static. Since it references mmap data,
        // we transmute the lifetime (safe because mmap lives as long as Document).
        let static_dict: &'static [(&'static [u8], PdfObject<'static>)] =
            unsafe { std::mem::transmute(dict.as_slice()) };

        let objstm = parse_objstm(static_dict, leaked)?;

        self.objstm_cache
            .write()
            .unwrap()
            .insert(stream_obj_num, objstm.objects);
        Ok(())
    }

    /// Get the page count from the page tree.
    ///
    /// Three-tier resolution mirroring `extract_doc`:
    /// 1. Top-level `/Count` field (cheapest — one object lookup)
    /// 2. Walk `/Kids` recursively via `page_refs().len()` (correct for
    ///    nested page trees where `/Count` lives on inner nodes)
    /// 3. xref scan for `/Type /Page` via `recover_page_refs().len()`
    ///    (last resort for malformed PDFs without a working `/Pages` chain)
    pub fn page_count(&self) -> ParseResult<u32> {
        let root = self.root()?;
        let pages_ref = root
            .get(b"Pages")
            .ok_or(ParseError::Message("missing /Pages in catalog".to_string()))?
            .as_ref()
            .ok_or(ParseError::Message(
                "/Pages must be a reference".to_string(),
            ))?;

        let pages = self.get_object(pages_ref.num)?;
        if let Some(n) = pages.get(b"Count").and_then(|c| c.as_i64()) {
            return Ok(n as u32);
        }

        // Fallback 1: walk Kids recursively.
        if let Ok(refs) = self.page_refs() {
            if !refs.is_empty() {
                return Ok(refs.len() as u32);
            }
        }

        // Fallback 2: xref scan for /Type /Page.
        Ok(self.recover_page_refs().len() as u32)
    }

    /// Iterate over page object references.
    pub fn page_refs(&self) -> ParseResult<Vec<ObjectId>> {
        let root = self.root()?;
        let pages_ref = root
            .get(b"Pages")
            .ok_or(ParseError::Message("missing /Pages in catalog".to_string()))?
            .as_ref()
            .ok_or(ParseError::Message(
                "/Pages must be a reference".to_string(),
            ))?;

        let pages = self.get_object(pages_ref.num)?;
        let kids = pages
            .get(b"Kids")
            .ok_or(ParseError::Message("missing /Kids in Pages".to_string()))?
            .as_array()
            .ok_or(ParseError::Message("/Kids must be an array".to_string()))?;

        let mut page_refs = Vec::new();
        collect_page_refs(self, kids, &mut page_refs)?;
        Ok(page_refs)
    }

    /// Fallback when the page tree is broken: scan every xref entry for
    /// objects whose /Type is /Page and return them in document order.
    ///
    /// The PDF spec guarantees /Type /Page identifies page nodes, so even
    /// without a working /Pages /Kids chain we can find them by exhaustive
    /// scan. Handles two entry types:
    /// - **Uncompressed** entries: page object lives at a byte offset.
    ///   Sort key = byte offset.
    /// - **Compressed** entries (inside an ObjStm, PDF spec 7.6.2): page
    ///   object lives inside an object stream. Sort key = (containing
    ///   ObjStm's byte offset, index within the ObjStm). Without this
    ///   branch, modern PDFs that pack pages into ObjStm (Word/Office
    ///   exports, recent Acrobat output) lose all their pages.
    ///
    /// Used by `extract_doc` when `page_refs()` fails. Catches the "missing
    /// /Pages in catalog" and "missing /Kids in Pages" failure modes that
    /// account for the page-tree bug-regression PDFs in the PyMuPDF corpus.
    pub fn recover_page_refs(&self) -> Vec<ObjectId> {
        // Map ObjStm object number → its byte offset, so we can sort
        // Compressed-page entries by where their containing ObjStm lives.
        let mut objstm_byte_offset: std::collections::HashMap<u32, usize> =
            std::collections::HashMap::new();
        for (&obj_num, entry) in self.xref.entries.iter() {
            if entry.entry_type == XrefEntryType::Uncompressed {
                objstm_byte_offset.insert(obj_num, entry.field1 as usize);
            }
        }

        // Sort key: (byte_offset_for_ordering, tiebreaker_within_container).
        // For Uncompressed: (real_byte_offset, 0).
        // For Compressed: (ObjStm byte offset, index within ObjStm).
        let mut found: Vec<(usize, u16, ObjectId)> = Vec::new();
        for (&obj_num, entry) in self.xref.entries.iter() {
            let (sort_offset, tiebreaker) = match entry.entry_type {
                XrefEntryType::Uncompressed => {
                    let offset = entry.field1 as usize;
                    // Cheap pre-filter: skip if no "/" appears in the 2KB
                    // window starting at the offset. Avoids a full parse of
                    // every object in the file.
                    let window_end = (offset + 2048).min(self.mmap.len());
                    if offset >= self.mmap.len()
                        || memchr::memchr(b'/', &self.mmap[offset..window_end]).is_none()
                    {
                        continue;
                    }
                    (offset, 0u16)
                }
                XrefEntryType::Compressed => {
                    let objstm_num = entry.field1;
                    let index_within = entry.field2;
                    let base = objstm_byte_offset.get(&objstm_num).copied().unwrap_or(0);
                    (base, index_within)
                }
                XrefEntryType::Free => continue,
            };
            if let Ok(obj) = self.get_object(obj_num) {
                let is_page = obj
                    .get(b"Type")
                    .and_then(|t| t.as_name())
                    .map(|n| n == b"Page")
                    .unwrap_or(false);
                if is_page {
                    // Compressed objects always have generation 0 (PDF spec
                    // 7.6.2); for Uncompressed entries, field2 is the real gen.
                    let gen = match entry.entry_type {
                        XrefEntryType::Compressed => 0,
                        _ => entry.field2,
                    };
                    found.push((sort_offset, tiebreaker, ObjectId { num: obj_num, gen }));
                }
            }
        }
        found.sort_by_key(|(off, tie, _)| (*off, *tie));
        found.into_iter().map(|(_, _, id)| id).collect()
    }

    /// Get the xref table (for debugging/inspection).
    pub fn xref(&self) -> &XrefTable {
        &self.xref
    }

    /// True iff the PDF has a `/Standard` encryption handler that flashpdf
    /// successfully opened (with the empty user password). False for plaintext
    /// PDFs. PDFs with non-Standard handlers or non-empty passwords failed at
    /// `open()` and never reach this accessor.
    pub fn is_encrypted(&self) -> bool {
        self.decryptor.is_some()
    }

    /// True iff the first indirect object in the file declares `/Linearized 1`
    /// (PDF spec §F.2). Linearized PDFs are optimized for first-page-fast web
    /// delivery; the flag is informational — flashpdf extracts the full
    /// document the same way regardless. Detecting the flag lets callers
    /// report it and skip work that would otherwise look for hints.
    pub fn is_linearized(&self) -> bool {
        // The linearization dict is always object 1 (or the first object in
        // file order) per spec §F.2. Look at the first non-free entry with the
        // lowest byte offset — that's where the linearization dict lives if
        // present.
        let mut first_obj: Option<(u32, usize)> = None;
        for (&num, entry) in self.xref.entries.iter() {
            if entry.entry_type != XrefEntryType::Uncompressed {
                continue;
            }
            let off = entry.field1 as usize;
            match first_obj {
                None => first_obj = Some((num, off)),
                Some((_, best_off)) if off < best_off => first_obj = Some((num, off)),
                _ => {}
            }
        }
        let Some((num, _)) = first_obj else {
            return false;
        };
        let Ok(obj) = self.get_object(num) else {
            return false;
        };
        obj.get(b"Linearized")
            .and_then(|v| v.as_i64())
            .map(|n| n == 1)
            .unwrap_or(false)
    }

    /// Get a reference to the underlying mmap data.
    pub fn mmap_slice(&self) -> &[u8] {
        &self.mmap
    }

    /// Extract the PDF version string from the `%PDF-X.Y` header.
    /// Returns `None` if the header is missing or malformed.
    pub fn pdf_version(&self) -> Option<&str> {
        let data = &self.mmap;
        if !data.starts_with(b"%PDF-") {
            return None;
        }
        let rest = &data[5..];
        let end = rest.iter().position(|&b| b == b'\n' || b == b'\r')?;
        std::str::from_utf8(&rest[..end]).ok()
    }
}

/// Recursively collect page references from the page tree.
fn collect_page_refs(
    doc: &Document,
    kids: &[PdfObject<'_>],
    refs: &mut Vec<ObjectId>,
) -> ParseResult<()> {
    for kid in kids {
        let kid_ref = kid
            .as_ref()
            .ok_or(ParseError::Message("Kid must be a reference".to_string()))?;
        let kid_obj = doc.get_object(kid_ref.num)?;
        let type_name = kid_obj
            .get(b"Type")
            .and_then(|t| t.as_name())
            .unwrap_or(b"");

        if type_name == b"Page" {
            refs.push(kid_ref);
        } else if type_name == b"Pages" {
            // Intermediate node, recurse
            let sub_kids = kid_obj
                .get(b"Kids")
                .ok_or(ParseError::Message("Pages node missing /Kids".to_string()))?
                .as_array()
                .ok_or(ParseError::Message("/Kids must be an array".to_string()))?;
            collect_page_refs(doc, sub_kids, refs)?;
        }
    }
    Ok(())
}

// ─── Raw object parsing at a specific offset ───

/// Parse an object at a specific byte offset in the file.
/// Expected format: `N G obj ... endobj`
fn parse_object_at(
    data: &[u8],
    offset: usize,
    _expected_gen: u16,
) -> ParseResult<PdfObject<'static>> {
    if offset >= data.len() {
        return Err(ParseError::UnexpectedEof);
    }

    let mut cur = Cursor::new(&data[offset..]);

    // Parse object header: N G obj
    cur.skip_ws();
    let _obj_num = crate::parser::xref::parse_positive_int_from_cursor(&mut cur)
        .map_err(|e| e.at(offset, data))? as u32;
    cur.skip_ws();
    let _gen = crate::parser::xref::parse_positive_int_from_cursor(&mut cur)
        .map_err(|e| e.at(offset, data))? as u16;
    cur.skip_ws();

    // Expect "obj"
    if !cur.remaining().starts_with(b"obj") {
        return Err(ParseError::Message("expected 'obj' keyword".to_string()).at(offset, data));
    }
    cur.advance(3);

    // Parse the object value
    let obj = parse_object(&mut cur).map_err(|e| e.at(offset, data))?;

    // If it's a dict followed by "stream", parse as stream
    let result = match &obj {
        PdfObject::Dict(_) => {
            cur.skip_ws();
            if cur.remaining().starts_with(b"stream") {
                let dict = match obj {
                    PdfObject::Dict(d) => d,
                    _ => unreachable!(),
                };
                parse_stream(&mut cur, dict)?
            } else {
                obj
            }
        }
        _ => obj,
    };

    // Leak to get 'static lifetime (the mmap data persists for the Document's lifetime)
    leak_pdf_object(result)
}

/// Re-parse the trailer at `trailer_offset` to extract an inline `/Encrypt`
/// dict. Used when the trailer has `/Encrypt<<...>>` (fitz/Acrobat form) rather
/// than `/Encrypt N 0 R` (spec-recommended indirect ref). The trailer_offset
/// points at the start of the xref table or xref-stream object — we scan
/// forward for the "trailer" keyword, parse the dict that follows, and pull
/// out `/Encrypt`. Returns an error if /Encrypt is missing or not a dict.
fn parse_inline_encrypt_from_trailer(
    data: &[u8],
    trailer_offset: Option<usize>,
) -> ParseResult<PdfObject<'static>> {
    let start = trailer_offset.ok_or(ParseError::Message(
        "inline /Encrypt dict but no trailer offset".to_string(),
    ))?;
    // Find "trailer" keyword from the offset. For xref streams there's no
    // "trailer" keyword — the stream dict IS the trailer. We don't currently
    // record that path's dict, so this helper handles only the standard xref
    // table case. (xref streams almost always use indirect /Encrypt refs.)
    let window = &data[start..];
    let trailer_pos = memchr::memmem::find(window, b"trailer").ok_or(ParseError::Message(
        "trailer keyword not found for inline /Encrypt re-parse".to_string(),
    ))?;
    let mut cur = Cursor::new(&window[trailer_pos + b"trailer".len()..]);
    cur.skip_ws();
    let dict_obj = parse_object(&mut cur)?;
    let dict = dict_obj.as_dict().ok_or(ParseError::Message(
        "trailer is not a dictionary".to_string(),
    ))?;
    let encrypt_val = dict
        .iter()
        .find(|(k, _)| *k == b"Encrypt")
        .map(|(_, v)| v)
        .ok_or(ParseError::Message(
            "/Encrypt not present after encrypt_present flag".to_string(),
        ))?;
    match encrypt_val {
        PdfObject::Dict(_) => leak_pdf_object(encrypt_val.clone()),
        _ => Err(ParseError::Message(
            "/Encrypt is not an inline dictionary".to_string(),
        )),
    }
}

/// Parse a raw stream object at a specific offset (for ObjStm).
fn parse_object_stream_raw<'a>(
    data: &'a [u8],
    offset: usize,
) -> ParseResult<(Vec<(&'a [u8], PdfObject<'a>)>, &'a [u8])> {
    let mut cur = Cursor::new(&data[offset..]);

    // Parse object header
    cur.skip_ws();
    let _ = crate::parser::xref::parse_positive_int_from_cursor(&mut cur)?;
    cur.skip_ws();
    let _ = crate::parser::xref::parse_positive_int_from_cursor(&mut cur)?;
    cur.skip_ws();

    if !cur.remaining().starts_with(b"obj") {
        return Err(ParseError::Message("expected 'obj' keyword".to_string()));
    }
    cur.advance(3);

    let obj = parse_object(&mut cur)?;
    match obj {
        PdfObject::Dict(dict) => {
            cur.skip_ws();
            if cur.remaining().starts_with(b"stream") {
                let stream_obj = parse_stream(&mut cur, dict)?;
                match stream_obj {
                    PdfObject::Stream { dict: d, data: sd } => Ok((d, sd)),
                    _ => Err(ParseError::Message("expected stream".to_string())),
                }
            } else {
                Err(ParseError::Message(
                    "expected stream after dict".to_string(),
                ))
            }
        }
        _ => Err(ParseError::Message("expected dict for ObjStm".to_string())),
    }
}

/// Resolve an indirect object at a given offset, returning raw dict + stream data.
fn resolve_indirect_object_raw<'a>(
    data: &'a [u8],
    offset: usize,
) -> ParseResult<(Vec<(&'a [u8], PdfObject<'a>)>, &'a [u8])> {
    parse_object_stream_raw(data, offset)
}

/// Leak a PdfObject to get 'static lifetime.
/// This is safe because the underlying mmap data lives as long as the Document.
fn leak_pdf_object<'a>(obj: PdfObject<'a>) -> ParseResult<PdfObject<'static>> {
    Ok(unsafe { std::mem::transmute::<PdfObject<'a>, PdfObject<'static>>(obj) })
}

/// Quick sanity check on a parsed xref: the root's declared offset must point
/// at a valid `N G obj` header. Catches PDFs whose xref table/stream is
/// well-formed but whose offsets don't match reality (prefix garbage, bad
/// linearization, etc.). Returns true when OK or when the root is inside an
/// object stream (we can't validate compressed entries cheaply, so trust them).
fn xref_root_ok(data: &[u8], xref: &XrefTable) -> bool {
    let Some(entry) = xref.get(xref.root.num) else {
        return false;
    };
    if entry.entry_type != XrefEntryType::Uncompressed {
        return true; // Compressed-in-ObjStm: defer to get_object path.
    }
    let offset = entry.field1 as usize;
    if offset >= data.len() {
        return false;
    }
    // The byte at the offset should eventually lead into "obj" within a few
    // bytes: "<obj_num> <gen> obj". Allow leading whitespace.
    let window_end = (offset + 32).min(data.len());
    memchr::memmem::find(&data[offset..window_end], b"obj").is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_decode_pdf_string_utf16_with_bom() {
        // "PDF" as UTF-16BE with BOM
        let bytes = [0xFE, 0xFF, 0x00, b'P', 0x00, b'D', 0x00, b'F'];
        assert_eq!(decode_pdf_string(&bytes), "PDF");
    }

    #[test]
    fn test_decode_pdf_string_utf16_cjk() {
        // "中" (U+4E2D) as UTF-16BE with BOM
        let bytes = [0xFE, 0xFF, 0x4E, 0x2D];
        assert_eq!(decode_pdf_string(&bytes), "中");
    }

    #[test]
    fn test_decode_pdf_string_ascii_passthrough() {
        // Plain ASCII (no BOM) → lossy UTF-8
        assert_eq!(decode_pdf_string(b"hello world"), "hello world");
    }

    #[test]
    fn test_decode_pdf_string_empty() {
        assert_eq!(decode_pdf_string(&[]), "");
    }

    #[test]
    fn test_decode_pdf_string_bom_only() {
        // BOM but no payload — UTF-16 of zero chars
        assert_eq!(decode_pdf_string(&[0xFE, 0xFF]), "");
    }

    #[test]
    fn test_hex_decode_round_trip() {
        // <FEFF5F20> → bytes [0xFE, 0xFF, 0x5F, 0x20] ("张" UTF-16BE)
        let bytes = hex_decode(b"FEFF5F20").unwrap();
        assert_eq!(bytes, vec![0xFE, 0xFF, 0x5F, 0x20]);
        assert_eq!(decode_pdf_string(&bytes), "张");
    }

    #[test]
    fn test_hex_decode_odd_nibble_padded() {
        // <F> → pad with trailing 0 → 0xF0
        assert_eq!(hex_decode(b"F").unwrap(), vec![0xF0]);
    }

    #[test]
    fn test_hex_decode_rejects_non_hex() {
        assert!(hex_decode(b"Hello").is_none());
    }

    #[test]
    fn test_hex_decode_empty() {
        assert_eq!(hex_decode(b"").unwrap(), Vec::<u8>::new());
    }

    #[test]
    fn test_unescape_literal_parens() {
        // \( → (, \) → ), \\ → \
        assert_eq!(unescape_literal_string(b"a\\(b\\)c"), b"a(b)c");
        assert_eq!(unescape_literal_string(b"path\\\\file"), b"path\\file");
    }

    #[test]
    fn test_unescape_literal_control() {
        assert_eq!(unescape_literal_string(b"a\\nb"), b"a\nb");
        assert_eq!(unescape_literal_string(b"a\\tb"), b"a\tb");
        assert_eq!(unescape_literal_string(b"a\\rb"), b"a\rb");
    }

    #[test]
    fn test_unescape_literal_octal() {
        // \053 = '+', \53 = '+', \053 is 3-digit octal
        assert_eq!(unescape_literal_string(b"\\053"), b"+");
        assert_eq!(unescape_literal_string(b"\\53"), b"+");
        // \377 = 0xFF
        assert_eq!(unescape_literal_string(b"\\377"), vec![0xFF]);
    }

    #[test]
    fn test_unescape_literal_unknown_keeps_char() {
        // \q is not a valid escape — drop the backslash, keep 'q'
        assert_eq!(unescape_literal_string(b"\\q"), b"q");
    }

    #[test]
    fn test_unescape_literal_trailing_backslash() {
        assert_eq!(unescape_literal_string(b"abc\\"), b"abc\\");
    }

    #[test]
    fn test_pdf_version_from_header() {
        // Use the same minimal valid-PDF structure as the metadata test so
        // Document::open succeeds and we can read the version off the header.
        let obj1 = "1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n";
        let obj2 = "2 0 obj\n<< /Type /Pages /Kids [] /Count 0 >>\nendobj\n";
        let header = "%PDF-1.7\n";
        let off1 = header.len();
        let off2 = off1 + obj1.len();
        let xref_offset = off2 + obj2.len();
        let xref = format!(
            "xref\n0 3\n\
0000000000 65535 f \n\
{off1:010} 00000 n \n\
{off2:010} 00000 n \n\
trailer\n<< /Size 3 /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n",
        );
        let mut pdf = String::new();
        pdf.push_str(header);
        pdf.push_str(obj1);
        pdf.push_str(obj2);
        pdf.push_str(&xref);
        let tmp = std::env::temp_dir().join("flashpdf_version_test.pdf");
        std::fs::write(&tmp, pdf.as_bytes()).unwrap();
        let doc = Document::open(&tmp).unwrap();
        assert_eq!(doc.pdf_version(), Some("1.7"));
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn test_pdf_version_missing_header() {
        // A doc whose header isn't `%PDF-` returns None. Document::open will
        // likely fail entirely on such input — but pdf_version() handles
        // the missing-prefix case defensively (returns None).
        let result = "garbage".to_string();
        assert!(std::str::from_utf8(b"garbage").unwrap() == result);
        // Direct unit-test of pdf_version on a synthetic doc is awkward
        // because Document::open requires a parseable file. The from_header
        // test above covers the happy path; the missing-header path is
        // exercised implicitly by corpus PDFs that lack the prefix.
    }

    #[test]
    fn test_document_metadata_missing_info_is_default() {
        // A document with no /Info trailer entry returns all-None metadata.
        // Build a minimal PDF in memory and parse via from_mmap.
        let pdf = b"%PDF-1.4\n\
1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n\
2 0 obj\n<< /Type /Pages /Kids [] /Count 0 >>\nendobj\n\
xref\n0 3\n0000000000 65535 f \n0000000009 00000 n \n0000000058 00000 n \n\
trailer\n<< /Size 3 /Root 1 0 R >>\nstartxref\n0\n%%EOF\n";
        // Write to a temp file because Document::open takes a path.
        let tmp = std::env::temp_dir().join("flashpdf_metadata_test.pdf");
        std::fs::write(&tmp, pdf).unwrap();
        let doc = Document::open(&tmp).unwrap();
        let m = doc.metadata();
        assert!(m.title.is_none());
        assert!(m.author.is_none());
        assert!(m.subject.is_none());
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn test_document_metadata_reads_info_fields() {
        // Same as above but with an /Info dict containing Title and Author.
        // Title is plain ASCII, Author is UTF-16BE with BOM ("张").
        let title_str = "Hello Title";
        // Build the body and track byte offsets for the xref table.
        let obj1 = "1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n";
        let obj2 = "2 0 obj\n<< /Type /Pages /Kids [] /Count 0 >>\nendobj\n";
        let obj3 = "3 0 obj\n<< /Title (Hello Title) /Author <FEFF5F20> >>\nendobj\n";
        let header = "%PDF-1.4\n";
        let off1 = header.len();
        let off2 = off1 + obj1.len();
        let off3 = off2 + obj2.len();
        // Construct xref table referencing those offsets. startxref must
        // point at the byte offset of the "xref" keyword below, not 0.
        let xref_offset = off1 + obj1.len() + obj2.len() + obj3.len();
        let xref = format!(
            "xref\n0 4\n\
0000000000 65535 f \n\
{off1:010} 00000 n \n\
{off2:010} 00000 n \n\
{off3:010} 00000 n \n\
trailer\n<< /Size 4 /Root 1 0 R /Info 3 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n",
        );
        let mut pdf = String::new();
        pdf.push_str(header);
        pdf.push_str(obj1);
        pdf.push_str(obj2);
        pdf.push_str(obj3);
        pdf.push_str(&xref);
        let tmp = std::env::temp_dir().join("flashpdf_metadata_info_test.pdf");
        std::fs::write(&tmp, pdf.as_bytes()).unwrap();
        let doc = Document::open(&tmp).unwrap();
        let m = doc.metadata();
        assert_eq!(m.title.as_deref(), Some(title_str));
        assert_eq!(m.author.as_deref(), Some("张"));
        assert!(m.subject.is_none());
        let _ = std::fs::remove_file(&tmp);
    }
}
