/// Link extraction: extract URI annotations from PDF pages.
use crate::document::Document;
use crate::parser::ParseResult;
use crate::types::PdfObject;

/// A link extracted from a PDF page.
#[derive(Debug, Clone)]
pub struct PageLink {
    /// URL or destination string.
    pub uri: String,
    /// Bounding box [x1, y1, x2, y2] in page coordinates.
    pub bbox: [f64; 4],
    /// Page number (0-indexed).
    pub page: u32,
}

/// Extract all URI links from a PDF document.
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

/// Extract links from a single page.
fn extract_page_links(
    doc: &Document,
    page: &PdfObject<'_>,
    page_num: u32,
) -> ParseResult<Vec<PageLink>> {
    let mut links = Vec::new();

    // Get /Annots array (may be indirect reference)
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
        // Resolve indirect reference
        let annot_obj = match annot_ref {
            PdfObject::Ref(r) => doc.get_object(r.num)?,
            other => other.clone(),
        };

        // Check if it's a Link annotation
        let subtype = annot_obj.get(b"Subtype").and_then(|v| v.as_name());
        if subtype != Some(b"Link") {
            continue;
        }

        // Extract URI from /A (action) dictionary
        let uri = extract_uri(&annot_obj, doc);
        let uri = match uri {
            Some(u) => u,
            None => continue,
        };

        // Extract bounding box from /Rect
        let bbox = extract_rect(&annot_obj);

        links.push(PageLink {
            uri,
            bbox,
            page: page_num,
        });
    }

    Ok(links)
}

/// Extract URI from annotation's action dictionary.
fn extract_uri(annot: &PdfObject<'_>, doc: &Document) -> Option<String> {
    // Try /A (action dictionary) first
    if let Some(action) = annot.get(b"A") {
        let action = match action {
            PdfObject::Ref(r) => doc.get_object(r.num).ok()?,
            other => other.clone(),
        };

        let action_type = action.get(b"S").and_then(|v| v.as_name());
        if action_type == Some(b"URI") {
            if let Some(uri_bytes) = action.get(b"URI").and_then(|v| v.as_str()) {
                return Some(String::from_utf8_lossy(uri_bytes).to_string());
            }
        }
    }

    // Try /Dest (direct destination) - could be a URI string
    if let Some(dest) = annot.get(b"Dest") {
        match dest {
            PdfObject::String(s) => {
                let text = String::from_utf8_lossy(s).to_string();
                if text.starts_with("http://") || text.starts_with("https://") {
                    return Some(text);
                }
            }
            PdfObject::Array(_arr) => {
                // Array destination: [page /Fit] or similar
                // First element might be a page reference, skip for now
            }
            _ => {}
        }
    }

    None
}

/// Extract bounding box from /Rect array.
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
}
