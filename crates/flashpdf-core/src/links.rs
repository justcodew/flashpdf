/// Link extraction: extract URI / goto / named destination links from PDF
/// pages (PDF spec §12.5.6.5 — Link Annotations, §12.6.4 — Action types).
///
/// Mirrors `fitz.Page.get_links()` semantics: each link has a `kind`, a
/// bounding box, and kind-specific fields (`uri` for external links, `page`
/// + `to` for internal goto, `name` for named destinations).
use crate::document::Document;
use crate::parser::ParseResult;
use crate::types::PdfObject;

/// Link kind, mirroring fitz `LINK_URI` / `LINK_GOTO` / `LINK_NAMED` /
/// `LINK_LAUNCH` / `LINK_GOTO_R`. flashpdf implements the first three
/// (the common subset for static-document link extraction); launch / remote
/// goto are returned as `Launch` / `GotoR` carrying the raw target string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkKind {
    /// External URL link (action type `/S /URI`).
    Uri,
    /// Internal page navigation (`/Dest` array or `/S /GoTo` with array dest).
    Goto,
    /// Named destination (`/Dest /Name` — resolved via `/Names /Dests`).
    Named,
    /// Launch an external application (`/S /Launch`). Target is the raw
    /// `/F` file specification; flashpdf does not execute, only reports.
    Launch,
    /// Remote goto to another PDF (`/S /GoToR`). Target is `/F` + page index.
    GotoR,
}

/// A link extracted from a PDF page.
#[derive(Debug, Clone)]
pub struct PageLink {
    /// Link classification — drives which of the optional fields below are set.
    pub kind: LinkKind,
    /// Bounding box `[x0, y0, x1, y1]` from `/Rect`, in PDF page coordinates
    /// (origin bottom-left). fitz calls this `from`.
    pub bbox: [f64; 4],
    /// 0-based page number this link belongs to.
    pub page: u32,
    /// URL for `Uri` links; empty for other kinds.
    pub uri: String,
    /// Destination page (0-based) for `Goto` links. `None` if the dest
    /// couldn't be resolved to a page (e.g. dangling named dest).
    pub to_page: Option<u32>,
    /// Target point `[x, y]` for `Goto` links with an explicit `/XYZ` etc.
    /// destination; `None` if the dest is a plain `/Fit` or unresolved.
    pub to_point: Option<[f64; 2]>,
    /// Named destination name for `Named` links.
    pub name: String,
    /// File target for `Launch` / `GotoR` links.
    pub file: String,
    /// Page index inside the target file for `GotoR` links.
    pub remote_page: Option<u32>,
}

impl PageLink {
    fn new(kind: LinkKind, bbox: [f64; 4], page: u32) -> Self {
        Self {
            kind,
            bbox,
            page,
            uri: String::new(),
            to_page: None,
            to_point: None,
            name: String::new(),
            file: String::new(),
            remote_page: None,
        }
    }
}

/// Extract all links from a PDF document, mirroring `extract_links` but
/// returning the richer `PageLink` (multi-kind).
pub fn extract_links(doc: &Document) -> ParseResult<Vec<PageLink>> {
    let page_refs = doc.page_refs()?;
    let mut links = Vec::new();
    for (page_idx, page_ref) in page_refs.iter().enumerate() {
        let page = doc.get_object(page_ref.num)?;
        let page_links = extract_page_links(doc, &page, page_idx as u32)?;
        links.extend(page_links);
    }
    Ok(links)
}

/// Extract links from a single page (also exposed for per-page queries).
pub fn extract_page_links(
    doc: &Document,
    page: &PdfObject<'_>,
    page_num: u32,
) -> ParseResult<Vec<PageLink>> {
    let mut links = Vec::new();

    let annots = match page.get(b"Annots") {
        Some(PdfObject::Ref(r)) => {
            let obj = doc.get_object(r.num)?;
            match obj.as_array() {
                Some(arr) => arr.to_vec(),
                None => return Ok(links),
            }
        }
        Some(PdfObject::Array(arr)) => arr.clone(),
        _ => return Ok(links),
    };

    for annot_ref in &annots {
        let annot_obj = match annot_ref {
            PdfObject::Ref(r) => doc.get_object(r.num)?,
            other => other.clone(),
        };

        let subtype = annot_obj.get(b"Subtype").and_then(|v| v.as_name());
        if subtype != Some(b"Link") {
            continue;
        }

        let bbox = extract_rect(&annot_obj);
        if let Some(link) = classify_link(doc, &annot_obj, bbox, page_num)? {
            links.push(link);
        }
    }

    Ok(links)
}

/// Classify a single link annotation into one of the LinkKind variants.
/// Returns `None` for link annotations that don't yield a usable target
/// (e.g. unknown action type).
fn classify_link(
    doc: &Document,
    annot: &PdfObject<'_>,
    bbox: [f64; 4],
    page_num: u32,
) -> ParseResult<Option<PageLink>> {
    // 1) /A action dictionary (most common path)
    if let Some(action) = annot.get(b"A") {
        let action = match action {
            PdfObject::Ref(r) => doc.get_object(r.num)?,
            other => other.clone(),
        };
        if let Some(link) = classify_action(doc, &action, bbox, page_num)? {
            return Ok(Some(link));
        }
    }

    // 2) Direct /Dest (no action wrapper) — implicit GoTo
    if let Some(dest) = annot.get(b"Dest") {
        let dest = match dest {
            PdfObject::Ref(r) => doc.get_object(r.num)?,
            other => other.clone(),
        };
        return Ok(Some(classify_dest(doc, &dest, bbox, page_num)));
    }

    Ok(None)
}

/// Map a `/S` action type to a LinkKind, populating kind-specific fields.
fn classify_action(
    doc: &Document,
    action: &PdfObject<'_>,
    bbox: [f64; 4],
    page_num: u32,
) -> ParseResult<Option<PageLink>> {
    let action_type = action.get(b"S").and_then(|v| v.as_name());
    match action_type {
        Some(b"URI") => {
            if let Some(uri_bytes) = action.get(b"URI").and_then(|v| v.as_str()) {
                let mut link = PageLink::new(LinkKind::Uri, bbox, page_num);
                link.uri = String::from_utf8_lossy(uri_bytes).into_owned();
                return Ok(Some(link));
            }
            Ok(None)
        }
        Some(b"GoTo") => {
            if let Some(dest) = action.get(b"D") {
                let dest = match dest {
                    PdfObject::Ref(r) => doc.get_object(r.num)?,
                    other => other.clone(),
                };
                return Ok(Some(classify_dest(doc, &dest, bbox, page_num)));
            }
            Ok(None)
        }
        Some(b"GoToR") => {
            // Remote goto: /F (file) + /D (page index or named dest)
            let file = action
                .get(b"F")
                .and_then(|v| v.as_str())
                .map(|b| String::from_utf8_lossy(b).into_owned())
                .unwrap_or_default();
            let remote_page = match action.get(b"D") {
                Some(PdfObject::Array(arr)) => {
                    arr.first().and_then(|v| v.as_i64()).map(|n| n as u32)
                }
                Some(PdfObject::Integer(n)) => Some(*n as u32),
                _ => None,
            };
            let mut link = PageLink::new(LinkKind::GotoR, bbox, page_num);
            link.file = file;
            link.remote_page = remote_page;
            Ok(Some(link))
        }
        Some(b"Launch") => {
            let file = action
                .get(b"F")
                .and_then(|v| v.as_str())
                .map(|b| String::from_utf8_lossy(b).into_owned())
                .unwrap_or_default();
            let mut link = PageLink::new(LinkKind::Launch, bbox, page_num);
            link.file = file;
            Ok(Some(link))
        }
        Some(b"Named") => {
            // Named action like /NextPage /PrevPage — we surface the name.
            let name = action
                .get(b"N")
                .and_then(|v| v.as_name())
                .map(|b| String::from_utf8_lossy(b).into_owned())
                .unwrap_or_default();
            let mut link = PageLink::new(LinkKind::Named, bbox, page_num);
            link.name = name;
            Ok(Some(link))
        }
        _ => Ok(None),
    }
}

/// Classify a `/Dest` value. Per PDF spec §12.3.2.2, a destination can be:
/// - An array `[page_ref /Fit | /XYZ x y zoom | /FitH y | ...]` (explicit)
/// - A name `/page1` referring to a named-destination entry (implicit)
fn classify_dest(doc: &Document, dest: &PdfObject<'_>, bbox: [f64; 4], page_num: u32) -> PageLink {
    match dest {
        PdfObject::Array(arr) => {
            // Explicit destination: [page-ref type p1 p2 p3]
            let to_page = arr
                .first()
                .and_then(|p| p.as_ref())
                .and_then(|id| page_index_for_ref(doc, id.num));
            let to_point = parse_dest_point(arr);
            let mut link = PageLink::new(LinkKind::Goto, bbox, page_num);
            link.to_page = to_page;
            link.to_point = to_point;
            link
        }
        PdfObject::Name(n) => {
            let mut link = PageLink::new(LinkKind::Named, bbox, page_num);
            link.name = String::from_utf8_lossy(n).into_owned();
            link
        }
        PdfObject::String(s) => {
            let mut link = PageLink::new(LinkKind::Named, bbox, page_num);
            link.name = String::from_utf8_lossy(s).into_owned();
            link
        }
        _ => PageLink::new(LinkKind::Named, bbox, page_num),
    }
}

/// Map an internal page-object id to its 0-based page index by walking the
/// page tree. Returns `None` if not found (e.g. dangling ref).
fn page_index_for_ref(doc: &Document, page_obj_num: u32) -> Option<u32> {
    let refs = doc.page_refs().ok()?;
    refs.iter()
        .position(|r| r.num == page_obj_num)
        .map(|i| i as u32)
}

/// For `/XYZ x y zoom` destinations, extract the target point. Other dest
/// types (`/Fit`, `/FitH y`, `/FitV x`, etc.) either have no point or have
/// only one coordinate — we surface them as `None` for simplicity, matching
/// fitz behavior (which also often returns `None` for non-XYZ dests).
fn parse_dest_point(dest_arr: &[PdfObject<'_>]) -> Option<[f64; 2]> {
    // arr[0] = page ref, arr[1] = type name, arr[2..] = type params
    let type_name = dest_arr.get(1).and_then(|v| v.as_name())?;
    if type_name == b"XYZ" {
        let x = dest_arr.get(2).and_then(|v| v.as_f64())?;
        let y = dest_arr.get(3).and_then(|v| v.as_f64())?;
        Some([x, y])
    } else {
        None
    }
}

/// Extract bounding box from `/Rect` array.
fn extract_rect(annot: &PdfObject<'_>) -> [f64; 4] {
    if let Some(rect) = annot.get(b"Rect").and_then(|v| v.as_array()) {
        let coords: Vec<f64> = rect.iter().filter_map(|v| v.as_f64()).collect();
        if coords.len() >= 4 {
            return [coords[0], coords[1], coords[2], coords[3]];
        }
    }
    [0.0, 0.0, 0.0, 0.0]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ObjectId;

    #[test]
    fn test_extract_rect() {
        let rect = PdfObject::Array(vec![
            PdfObject::Real(100.0),
            PdfObject::Real(200.0),
            PdfObject::Real(300.0),
            PdfObject::Real(400.0),
        ]);
        let annot = PdfObject::Dict(vec![(b"Rect" as &[u8], rect)]);
        let bbox = extract_rect(&annot);
        assert_eq!(bbox, [100.0, 200.0, 300.0, 400.0]);
    }

    #[test]
    fn test_extract_rect_missing_returns_zero() {
        let annot: PdfObject<'_> = PdfObject::Dict(vec![]);
        assert_eq!(extract_rect(&annot), [0.0, 0.0, 0.0, 0.0]);
    }

    #[test]
    fn test_parse_dest_point_xyz() {
        // [page-ref /XYZ 100 200 0]
        let arr = vec![
            PdfObject::Ref(ObjectId::new(5, 0)),
            PdfObject::Name(b"XYZ"),
            PdfObject::Real(100.0),
            PdfObject::Real(200.0),
            PdfObject::Integer(0),
        ];
        assert_eq!(parse_dest_point(&arr), Some([100.0, 200.0]));
    }

    #[test]
    fn test_parse_dest_point_fit_returns_none() {
        // /Fit has no explicit point
        let arr = vec![PdfObject::Ref(ObjectId::new(5, 0)), PdfObject::Name(b"Fit")];
        assert_eq!(parse_dest_point(&arr), None);
    }

    #[test]
    fn test_parse_dest_point_short_array() {
        // Only one element (page ref, no type) → None
        let arr = vec![PdfObject::Ref(ObjectId::new(5, 0))];
        assert_eq!(parse_dest_point(&arr), None);
    }
}
