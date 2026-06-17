use crate::parser::object::{parse_object, parse_stream, Cursor, ParseError, ParseResult};
use crate::parser::recovery::recover_xref_by_scan;
use crate::parser::xref::{
    decompress_stream, find_startxref, is_standard_xref, parse_objstm, parse_xref_stream_obj,
    parse_xref_table, XrefEntryType, XrefTable,
};
use crate::types::{ObjectId, PdfObject};
use memmap2::Mmap;
use std::collections::HashMap;
use std::fs::File;
use std::path::Path;
use std::sync::RwLock;

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
}

impl Document {
    /// Open and parse a PDF file.
    pub fn open<P: AsRef<Path>>(path: P) -> ParseResult<Self> {
        let file = File::open(path).map_err(|_| ParseError::Message("cannot open file"))?;
        let mmap =
            unsafe { Mmap::map(&file) }.map_err(|_| ParseError::Message("cannot mmap file"))?;
        Self::from_mmap(mmap)
    }

    /// Parse a PDF from an existing memory-mapped region.
    pub fn from_mmap(mmap: Mmap) -> ParseResult<Self> {
        let data: &[u8] = &mmap;

        // Try standard xref parsing first; fall back to memchr recovery
        let xref = match find_startxref(data) {
            Ok(xref_offset) => {
                if is_standard_xref(data, xref_offset) {
                    parse_xref_table(data, xref_offset)
                } else {
                    // xref stream
                    let (dict, stream_data) = resolve_indirect_object_raw(data, xref_offset)?;
                    parse_xref_stream_obj(&dict, stream_data)
                }
            }
            Err(_) => Err(ParseError::Message("startxref not found")),
        };

        // Fallback: memchr full-file recovery
        let xref = match xref {
            Ok(x) => x,
            Err(_) => recover_xref_by_scan(data)?,
        };

        // Check for encryption
        if xref.encrypt.is_some() {
            return Err(ParseError::Message("encrypted PDFs are not supported"));
        }

        Ok(Self {
            mmap,
            xref,
            object_cache: RwLock::new(HashMap::new()),
            objstm_cache: RwLock::new(HashMap::new()),
        })
    }

    /// Get the document catalog (root) object.
    pub fn root(&self) -> ParseResult<PdfObject<'static>> {
        self.get_object(self.xref.root.num)
    }

    /// Get the /Root object ID.
    pub fn root_id(&self) -> ObjectId {
        self.xref.root
    }

    /// Get the total number of objects (as declared by /Size).
    pub fn size(&self) -> u32 {
        self.xref.size
    }

    /// Get an indirect object by its object number.
    /// Objects are parsed lazily and cached.
    pub fn get_object(&self, obj_num: u32) -> ParseResult<PdfObject<'static>> {
        // Check cache first
        {
            let cache = self.object_cache.read().unwrap();
            if let Some(obj) = cache.get(&obj_num) {
                return Ok(obj.clone());
            }
        }

        let entry = self
            .xref
            .get(obj_num)
            .ok_or(ParseError::Message("object not in xref"))?;

        let obj = match entry.entry_type {
            XrefEntryType::Uncompressed => {
                let offset = entry.field1 as usize;
                let gen = entry.field2;
                let data: &[u8] = &self.mmap;
                parse_object_at(data, offset, gen)?
            }
            XrefEntryType::Compressed => {
                let stream_obj_num = entry.field1;

                // Ensure the object stream is loaded
                {
                    let stm_cache = self.objstm_cache.read().unwrap();
                    if !stm_cache.contains_key(&stream_obj_num) {
                        drop(stm_cache);
                        self.load_objstm(stream_obj_num)?;
                    }
                }

                let stm_cache = self.objstm_cache.read().unwrap();
                stm_cache
                    .get(&stream_obj_num)
                    .and_then(|m| m.get(&obj_num))
                    .cloned()
                    .ok_or(ParseError::Message("object not found in ObjStm"))?
            }
            XrefEntryType::Free => {
                return Err(ParseError::Message("object is free (deleted)"));
            }
        };

        self.object_cache
            .write()
            .unwrap()
            .insert(obj_num, obj.clone());
        Ok(obj)
    }

    fn load_objstm(&self, stream_obj_num: u32) -> ParseResult<()> {
        let entry = self
            .xref
            .get(stream_obj_num)
            .ok_or(ParseError::Message("ObjStm not in xref"))?;

        if entry.entry_type != XrefEntryType::Uncompressed {
            return Err(ParseError::Message("ObjStm must be uncompressed"));
        }

        let offset = entry.field1 as usize;
        let data: &[u8] = &self.mmap;
        let (dict, raw_stream_data) = parse_object_stream_raw(data, offset)?;

        // Decompress based on /Filter
        let filter = dict
            .iter()
            .find(|(k, _)| *k == b"Filter")
            .map(|(_, v)| v.clone());
        let stream_data: Vec<u8> = match filter {
            Some(f) => decompress_stream(raw_stream_data, &f)?,
            None => raw_stream_data.to_vec(),
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
    pub fn page_count(&self) -> ParseResult<u32> {
        let root = self.root()?;
        let pages_ref = root
            .get(b"Pages")
            .ok_or(ParseError::Message("missing /Pages in catalog"))?
            .as_ref()
            .ok_or(ParseError::Message("/Pages must be a reference"))?;

        let pages = self.get_object(pages_ref.num)?;
        pages
            .get(b"Count")
            .and_then(|c| c.as_i64())
            .map(|n| n as u32)
            .ok_or(ParseError::Message("missing /Count in Pages"))
    }

    /// Iterate over page object references.
    pub fn page_refs(&self) -> ParseResult<Vec<ObjectId>> {
        let root = self.root()?;
        let pages_ref = root
            .get(b"Pages")
            .ok_or(ParseError::Message("missing /Pages in catalog"))?
            .as_ref()
            .ok_or(ParseError::Message("/Pages must be a reference"))?;

        let pages = self.get_object(pages_ref.num)?;
        let kids = pages
            .get(b"Kids")
            .ok_or(ParseError::Message("missing /Kids in Pages"))?
            .as_array()
            .ok_or(ParseError::Message("/Kids must be an array"))?;

        let mut page_refs = Vec::new();
        collect_page_refs(self, kids, &mut page_refs)?;
        Ok(page_refs)
    }

    /// Get the xref table (for debugging/inspection).
    pub fn xref(&self) -> &XrefTable {
        &self.xref
    }

    /// Get a reference to the underlying mmap data.
    pub fn mmap_slice(&self) -> &[u8] {
        &self.mmap
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
            .ok_or(ParseError::Message("Kid must be a reference"))?;
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
                .ok_or(ParseError::Message("Pages node missing /Kids"))?
                .as_array()
                .ok_or(ParseError::Message("/Kids must be an array"))?;
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
    let _obj_num = crate::parser::xref::parse_positive_int_from_cursor(&mut cur)? as u32;
    cur.skip_ws();
    let _gen = crate::parser::xref::parse_positive_int_from_cursor(&mut cur)? as u16;
    cur.skip_ws();

    // Expect "obj"
    if !cur.remaining().starts_with(b"obj") {
        return Err(ParseError::Message("expected 'obj' keyword"));
    }
    cur.advance(3);

    // Parse the object value
    let obj = parse_object(&mut cur)?;

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
        return Err(ParseError::Message("expected 'obj' keyword"));
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
                    _ => Err(ParseError::Message("expected stream")),
                }
            } else {
                Err(ParseError::Message("expected stream after dict"))
            }
        }
        _ => Err(ParseError::Message("expected dict for ObjStm")),
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
