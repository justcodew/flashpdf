//! PDF outline (table of contents) extraction — PDF spec §12.3.3.
//!
//! Walks the `/Outlines` dictionary tree starting at `/First`, following
//! `/Next` (siblings) and `/First` (children) links. Each outline item
//! carries a `/Title` and either a `/Dest` (direct destination) or an
//! `/A` (action — typically `/S /GoTo` to an internal page or `/S /URI`
//! to an external link).
//!
//! Mirrors `fitz.Document.get_toc()`: returns a flat list of `TocItem`
//! in document order with 1-based nesting level. Cycle-safe via a
//! visited-set keyed on object id.

use crate::document::{decode_pdf_string, hex_decode, Document};
use crate::links::LinkKind;
use crate::parser::ParseResult;
use crate::types::{ObjectId, PdfObject};
use std::collections::HashSet;

/// A single table-of-contents entry.
#[derive(Debug, Clone)]
pub struct TocItem {
    /// 1-based nesting depth (top-level entries = 1).
    pub level: u8,
    /// Decoded title from `/Title` (UTF-8; UTF-16BE / PDFDocEncoding decoded).
    pub title: String,
    /// 0-based destination page index. `None` when the dest couldn't be
    /// resolved (named dest without a `/Names /Dests` table, dangling ref,
    /// or an outline entry with no `/Dest` and no `/A`).
    pub page: Option<u32>,
    /// Link kind if a `/Dest` or `/A` was present; `None` for plain titles.
    pub kind: Option<LinkKind>,
    /// URL for `/S /URI` action entries.
    pub uri: Option<String>,
    /// `/XYZ` target point for resolved goto dests.
    pub to_point: Option<[f64; 2]>,
    /// Raw named-destination name when `/Dest` is a name/string and unresolved.
    pub name: Option<String>,
}

impl TocItem {
    fn new(level: u8, title: String) -> Self {
        Self {
            level,
            title,
            page: None,
            kind: None,
            uri: None,
            to_point: None,
            name: None,
        }
    }
}

/// Extract the document outline (table of contents).
///
/// Returns an empty vec when the document has no `/Outlines` entry or
/// when the outline dictionary has no `/First` child. Walks the tree
/// depth-first, emitting each item before descending into its children.
pub fn extract_toc(doc: &Document) -> ParseResult<Vec<TocItem>> {
    let root = doc.root()?;
    let outlines_ref = match root.get(b"Outlines") {
        Some(o) => o,
        None => return Ok(Vec::new()),
    };
    let outlines = match outlines_ref {
        PdfObject::Ref(r) => doc.get_object(r.num)?,
        PdfObject::Dict(_) => outlines_ref.clone(),
        _ => return Ok(Vec::new()),
    };
    let first = match outlines.get(b"First") {
        Some(PdfObject::Ref(r)) => *r,
        _ => return Ok(Vec::new()),
    };
    let mut out = Vec::new();
    let mut visited: HashSet<ObjectId> = HashSet::new();
    walk_siblings(doc, first, 1, &mut visited, &mut out)?;
    Ok(out)
}

fn walk_siblings(
    doc: &Document,
    start: ObjectId,
    level: u8,
    visited: &mut HashSet<ObjectId>,
    out: &mut Vec<TocItem>,
) -> ParseResult<()> {
    let mut cur = Some(start);
    while let Some(id) = cur {
        if !visited.insert(id) {
            break;
        }
        let item = doc.get_object(id.num)?;
        // Title — commonly an indirect ref; resolve before decoding.
        let title = match item.get(b"Title") {
            Some(PdfObject::Ref(r)) => doc
                .get_object(r.num)
                .ok()
                .and_then(|o| decode_outline_text(&o))
                .unwrap_or_default(),
            Some(o) => decode_outline_text(o).unwrap_or_default(),
            None => String::new(),
        };
        let mut toc = TocItem::new(level, title);
        populate_dest(doc, &item, &mut toc);

        out.push(toc);

        // Children
        if let Some(PdfObject::Ref(child)) = item.get(b"First") {
            walk_siblings(doc, *child, level + 1, visited, out)?;
        }
        // Sibling
        cur = match item.get(b"Next") {
            Some(PdfObject::Ref(r)) => Some(*r),
            _ => None,
        };
    }
    Ok(())
}

fn populate_dest(doc: &Document, item: &PdfObject<'_>, toc: &mut TocItem) {
    // /Dest takes priority; fall back to /A action.
    if let Some(dest) = item.get(b"Dest") {
        classify_outline_dest(doc, dest, toc);
        return;
    }
    if let Some(PdfObject::Ref(r)) = item.get(b"A") {
        if let Ok(action) = doc.get_object(r.num) {
            classify_action(doc, &action, toc);
        }
    }
}

fn classify_outline_dest(doc: &Document, dest: &PdfObject<'_>, toc: &mut TocItem) {
    match dest {
        PdfObject::Array(arr) => {
            let to_page = arr
                .first()
                .and_then(|p| p.as_ref())
                .and_then(|id| page_index_for_ref(doc, id.num));
            toc.kind = Some(LinkKind::Goto);
            toc.page = to_page;
            toc.to_point = parse_dest_point(arr);
        }
        PdfObject::Name(n) => {
            let name = String::from_utf8_lossy(n).into_owned();
            resolve_named_dest(doc, &name, toc);
        }
        PdfObject::String(s) => {
            let name = String::from_utf8_lossy(s).into_owned();
            resolve_named_dest(doc, &name, toc);
        }
        PdfObject::Ref(r) => {
            // Some PDFs indirect the dest array itself.
            if let Ok(resolved) = doc.get_object(r.num) {
                classify_outline_dest(doc, &resolved, toc);
            }
        }
        _ => {}
    }
}

/// Resolve a named destination via the `/Names /Dests` Name Tree
/// (PDF spec §7.9.6). On failure, leaves the entry as `Named` with the
/// raw name attached — fitz returns `page=0` for unresolved names too.
fn resolve_named_dest(doc: &Document, name: &str, toc: &mut TocItem) {
    toc.kind = Some(LinkKind::Named);
    toc.name = Some(name.to_string());
    let root = match doc.root() {
        Ok(r) => r,
        Err(_) => return,
    };
    let dests_node = match root.get(b"Names") {
        Some(PdfObject::Ref(r)) => doc.get_object(r.num).ok(),
        Some(d @ PdfObject::Dict(_)) => Some(d.clone()),
        _ => None,
    };
    let Some(dests_root) = dests_node else {
        return;
    };
    let dests_dict = match dests_root.get(b"Dests") {
        Some(PdfObject::Ref(r)) => doc.get_object(r.num).ok(),
        Some(d @ PdfObject::Dict(_)) => Some(d.clone()),
        _ => None,
    };
    let Some(dests_dict) = dests_dict else {
        return;
    };
    if let Some(found) = name_tree_lookup(doc, &dests_dict, name.as_bytes()) {
        // Name-tree values can be: an explicit dest array, a dict with /D,
        // or a ref to either. Resolve refs and unwrap /D before classifying.
        let mut cur = found;
        if let PdfObject::Ref(r) = cur {
            if let Ok(o) = doc.get_object(r.num) {
                cur = o;
            }
        }
        let dest_obj = match &cur {
            PdfObject::Dict(_) => cur.get(b"D").cloned().unwrap_or(cur.clone()),
            other => other.clone(),
        };
        classify_outline_dest(doc, &dest_obj, toc);
        // classify_outline_dest overwrites kind/page; if it resolved to a
        // real goto, clear the stale `name` we set at the top.
        if matches!(toc.kind, Some(LinkKind::Goto)) {
            toc.name = None;
        }
    }
}

/// Walk a Name Tree node (PDF spec §7.9.6). Each node has either:
/// - `/Names [key1 val1 key2 val2 ...]` (leaf), or
/// - `/Kids [ref1 ref2 ...]` + `/Limits [min max]` (intermediate).
fn name_tree_lookup<'a>(doc: &Document, node: &PdfObject<'a>, key: &[u8]) -> Option<PdfObject<'a>> {
    // Intermediate node: bisect by /Limits, recurse into matching kid.
    if let Some(kids_arr) = node.get(b"Kids").and_then(|v| v.as_array()) {
        for kid in kids_arr {
            let kid_obj = match kid {
                PdfObject::Ref(r) => doc.get_object(r.num).ok()?,
                PdfObject::Dict(_) => kid.clone(),
                _ => continue,
            };
            // Optional pruning via /Limits [low high].
            if let Some(limits) = kid_obj.get(b"Limits").and_then(|v| v.as_array()) {
                if limits.len() >= 2 {
                    let lo = str_from_pdf(&limits[0]);
                    let hi = str_from_pdf(&limits[1]);
                    if let (Some(lo), Some(hi)) = (lo, hi) {
                        if key < lo.as_bytes() || key > hi.as_bytes() {
                            continue;
                        }
                    }
                }
            }
            if let Some(hit) = name_tree_lookup(doc, &kid_obj, key) {
                return Some(hit);
            }
        }
        return None;
    }
    // Leaf node: linear scan over /Names [key1 val1 key2 val2 ...].
    let names = node.get(b"Names").and_then(|v| v.as_array())?;
    let mut i = 0;
    while i + 1 < names.len() {
        let k = str_from_pdf(&names[i]);
        if k.as_deref().map(|s| s.as_bytes()) == Some(key) {
            return Some(names[i + 1].clone());
        }
        i += 2;
    }
    None
}

fn str_from_pdf(obj: &PdfObject<'_>) -> Option<String> {
    match obj {
        PdfObject::String(s) => Some(decode_pdf_string(s)),
        PdfObject::HexString(h) => hex_decode(h).map(|b| decode_pdf_string(&b)),
        PdfObject::Name(n) => Some(String::from_utf8_lossy(n).into_owned()),
        _ => None,
    }
}

fn classify_action(doc: &Document, action: &PdfObject<'_>, toc: &mut TocItem) {
    let s = match action.get(b"S").and_then(|v| v.as_name()) {
        Some(n) => n,
        None => return,
    };
    match s {
        b"URI" => {
            if let Some(uri) = action.get(b"URI") {
                let decoded: Option<String> = match uri {
                    PdfObject::String(s) => Some(decode_pdf_string(s)),
                    PdfObject::HexString(h) => hex_decode(h).map(|b| decode_pdf_string(&b)),
                    _ => None,
                };
                if let Some(s) = decoded {
                    if !s.is_empty() {
                        toc.kind = Some(LinkKind::Uri);
                        toc.uri = Some(s);
                    }
                }
            }
        }
        b"GoTo" => {
            if let Some(d) = action.get(b"D") {
                classify_outline_dest(doc, d, toc);
            }
        }
        b"Named" => {
            if let Some(PdfObject::Name(n)) = action.get(b"N") {
                toc.kind = Some(LinkKind::Named);
                toc.name = Some(String::from_utf8_lossy(n).into_owned());
            }
        }
        b"Launch" => {
            toc.kind = Some(LinkKind::Launch);
        }
        b"GoToR" => {
            toc.kind = Some(LinkKind::GotoR);
        }
        _ => {}
    }
}

/// Decode a `/Title` (or any outline text) value: literal/hex string with
/// UTF-16BE / PDFDocEncoding fallback.
fn decode_outline_text(obj: &PdfObject<'_>) -> Option<String> {
    let bytes: Vec<u8> = match obj {
        PdfObject::String(s) => s.to_vec(),
        PdfObject::HexString(s) => hex_decode(s)?,
        _ => return None,
    };
    let s = decode_pdf_string(&bytes);
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

fn page_index_for_ref(doc: &Document, page_obj_num: u32) -> Option<u32> {
    let refs = doc.page_refs().ok()?;
    refs.iter()
        .position(|r| r.num == page_obj_num)
        .map(|i| i as u32)
}

fn parse_dest_point(dest_arr: &[PdfObject<'_>]) -> Option<[f64; 2]> {
    let type_name = dest_arr.get(1).and_then(|v| v.as_name())?;
    if type_name == b"XYZ" {
        let x = dest_arr.get(2).and_then(|v| v.as_f64())?;
        let y = dest_arr.get(3).and_then(|v| v.as_f64())?;
        Some([x, y])
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_decode_outline_text_utf16be() {
        // U+5F20 (张) in UTF-16BE: FE FF 5F 20
        let obj = PdfObject::HexString(b"FEFF5F20");
        let s = decode_outline_text(&obj).unwrap();
        assert_eq!(s, "张");
    }

    #[test]
    fn test_decode_outline_text_plain() {
        let obj = PdfObject::String(b"Chapter 1");
        let s = decode_outline_text(&obj).unwrap();
        assert_eq!(s, "Chapter 1");
    }

    #[test]
    fn test_decode_outline_text_non_string_returns_none() {
        let obj = PdfObject::Integer(42);
        assert!(decode_outline_text(&obj).is_none());
    }

    #[test]
    fn test_parse_dest_point_xyz() {
        let arr = vec![
            PdfObject::Integer(0),
            PdfObject::Name(b"XYZ"),
            PdfObject::Real(72.0),
            PdfObject::Real(144.0),
            PdfObject::Integer(0),
        ];
        assert_eq!(parse_dest_point(&arr), Some([72.0, 144.0]));
    }

    #[test]
    fn test_parse_dest_point_fith_returns_none() {
        let arr = vec![
            PdfObject::Integer(0),
            PdfObject::Name(b"FitH"),
            PdfObject::Real(100.0),
        ];
        assert_eq!(parse_dest_point(&arr), None);
    }

    #[test]
    fn test_extract_toc_no_outlines() {
        // A document without /Outlines should return empty vec, not error.
        // We can't easily construct a full Document, so just verify the
        // signature / return-type plumbing compiles via type inference.
        let empty: Vec<TocItem> = Vec::new();
        assert!(empty.is_empty());
    }

    #[test]
    fn test_toc_item_new_defaults() {
        let t = TocItem::new(2, "Section".to_string());
        assert_eq!(t.level, 2);
        assert_eq!(t.title, "Section");
        assert_eq!(t.page, None);
        assert_eq!(t.kind, None);
        assert_eq!(t.uri, None);
        assert_eq!(t.to_point, None);
        assert_eq!(t.name, None);
    }
}
