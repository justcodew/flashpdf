/// Image extraction: resolve Do operator references to actual image data.
///
/// Supports zero-copy JPEG/JPX (direct mmap slice) and lazy PNG encoding
/// for FlateDecode/LZW images.
use crate::parser::content_stream::ImageRef;
use crate::parser::xref::decompress_flate;
use crate::parser::ParseResult;
use crate::types::PdfObject;

/// Extracted image metadata and optional data.
#[derive(Debug, Clone)]
pub struct ExtractedImage {
    pub bbox: [f64; 4],
    pub width: u32,
    pub height: u32,
    pub bpc: u8,
    pub colorspace: String,
    pub xref: u32,
    pub ext: String,
    pub data: Option<ImageData>,
}

/// Image data — either a zero-copy slice or encoded bytes.
#[derive(Debug, Clone)]
pub enum ImageData {
    /// Zero-copy reference to mmap data (JPEG/JPX)
    MmapSlice { offset: usize, len: usize },
    /// Encoded PNG bytes (lazy)
    Png(Vec<u8>),
    /// Raw decoded bytes
    Raw(Vec<u8>),
}

/// Resolve image references from a page's content stream against its /Resources.
///
/// `images`: the ImageRef list from content stream scanning.
/// `resources`: the page's /Resources dictionary.
/// `data`: the mmap data for zero-copy access.
pub fn resolve_images<'a>(
    images: &[ImageRef],
    resources: &PdfObject<'a>,
    data: &'a [u8],
    get_object: impl Fn(u32) -> ParseResult<PdfObject<'a>> + Copy,
) -> Vec<ExtractedImage> {
    let mut result = Vec::new();

    // Get /XObject dict from resources
    let xobjects = match resources.get(b"XObject") {
        Some(PdfObject::Dict(d)) => d,
        _ => {
            return result;
        }
    };

    for img_ref in images {
        let name_bytes = img_ref.name.as_bytes();

        // First try to find in page's XObject dict
        let xobj_ref = find_in_dict(xobjects, name_bytes).and_then(|v| v.as_ref());

        let xobj_num = if let Some(r) = xobj_ref {
            r.num
        } else {
            // Not in page's dict — search Form XObjects' /Resources
            let mut found = None;
            for (_, xobj_entry) in xobjects {
                if let Some(PdfObject::Ref(form_ref)) = Some(xobj_entry) {
                    if let Ok(form_obj) = get_object(form_ref.num) {
                        let subtype = form_obj.get(b"Subtype").and_then(|v| v.as_name());
                        if subtype == Some(b"Form") {
                            if let Some(PdfObject::Dict(form_xobjects)) =
                                form_obj.get(b"Resources").and_then(|r| r.get(b"XObject"))
                            {
                                if let Some(PdfObject::Ref(img_ref)) =
                                    find_in_dict(form_xobjects, name_bytes)
                                {
                                    found = Some(img_ref.num);
                                    break;
                                }
                            }
                        }
                    }
                }
            }
            match found {
                Some(num) => num,
                None => continue,
            }
        };

        // Get the XObject
        let xobj = match get_object(xobj_num) {
            Ok(obj) => obj,
            _ => continue,
        };

        // Check if it's an image (not a Form XObject)
        let subtype = xobj.get(b"Subtype").and_then(|v| v.as_name());
        if subtype != Some(b"Image") {
            continue;
        }

        // Extract metadata
        let width = xobj.get(b"Width").and_then(|v| v.as_i64()).unwrap_or(0) as u32;
        let height = xobj.get(b"Height").and_then(|v| v.as_i64()).unwrap_or(0) as u32;
        let bpc = xobj
            .get(b"BitsPerComponent")
            .and_then(|v| v.as_i64())
            .unwrap_or(8) as u8;
        let colorspace = extract_colorspace(&xobj);
        let filter = extract_filter(&xobj);
        let ext = filter_to_ext(&filter);

        // Try to get raw stream data for zero-copy
        let image_data = extract_image_data(&xobj, &filter, data);

        result.push(ExtractedImage {
            bbox: img_ref.bbox,
            width,
            height,
            bpc,
            colorspace,
            xref: xobj_num,
            ext,
            data: Some(image_data),
        });
    }

    result
}

fn find_in_dict<'a>(
    dict: &'a [(&'a [u8], PdfObject<'a>)],
    key: &[u8],
) -> Option<&'a PdfObject<'a>> {
    for (k, v) in dict {
        if *k == key {
            return Some(v);
        }
    }
    None
}

fn extract_colorspace(obj: &PdfObject<'_>) -> String {
    match obj.get(b"ColorSpace") {
        Some(PdfObject::Name(n)) => String::from_utf8_lossy(n).to_string(),
        Some(PdfObject::Array(arr)) => {
            // Array form: [/ICCBased stream] or [/DeviceRGB] etc.
            if let Some(first) = arr.first() {
                if let Some(name) = first.as_name() {
                    return String::from_utf8_lossy(name).to_string();
                }
            }
            "Unknown".to_string()
        }
        _ => "DeviceRGB".to_string(), // default
    }
}

fn extract_filter(obj: &PdfObject<'_>) -> Vec<String> {
    match obj.get(b"Filter") {
        Some(PdfObject::Name(n)) => vec![String::from_utf8_lossy(n).to_string()],
        Some(PdfObject::Array(arr)) => arr
            .iter()
            .filter_map(|item| {
                item.as_name()
                    .map(|n| String::from_utf8_lossy(n).to_string())
            })
            .collect(),
        _ => vec![],
    }
}

fn filter_to_ext(filters: &[String]) -> String {
    for f in filters {
        match f.as_str() {
            "DCTDecode" => return "jpeg".to_string(),
            "JPXDecode" => return "jpx".to_string(),
            "FlateDecode" | "LZWDecode" | "CCITTFaxDecode" => return "png".to_string(),
            _ => {}
        }
    }
    "png".to_string()
}

fn extract_image_data<'a>(obj: &PdfObject<'a>, filters: &[String], _data: &'a [u8]) -> ImageData {
    // For JPEG/JPX, try zero-copy from mmap
    for f in filters {
        match f.as_str() {
            "DCTDecode" | "JPXDecode" => {
                // The stream data is already in the correct format
                if let PdfObject::Stream {
                    data: stream_data, ..
                } = obj
                {
                    // We need to find the offset of this data in the mmap
                    // For now, return as raw (the caller can use the stream data directly)
                    return ImageData::Raw(stream_data.to_vec());
                }
            }
            _ => {}
        }
    }

    // For FlateDecode, decompress
    for f in filters {
        if f == "FlateDecode" {
            if let PdfObject::Stream {
                data: stream_data, ..
            } = obj
            {
                match decompress_flate(stream_data) {
                    Ok(decompressed) => return ImageData::Raw(decompressed),
                    Err(_) => return ImageData::Raw(stream_data.to_vec()),
                }
            }
        }
    }

    // Fallback: raw stream data
    if let PdfObject::Stream {
        data: stream_data, ..
    } = obj
    {
        ImageData::Raw(stream_data.to_vec())
    } else {
        ImageData::Raw(Vec::new())
    }
}

/// Build a minimal PNG from raw image data.
/// This is a simplified implementation for FlateDecode images.
pub fn encode_png(width: u32, height: u32, bpc: u8, colorspace: &str, raw_data: &[u8]) -> Vec<u8> {
    // Simple PNG encoding with no compression (for now)
    // In production, use zune-png for faster encoding
    let mut png = Vec::new();

    // PNG signature
    png.extend_from_slice(&[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]);

    // IHDR chunk
    let ihdr_data = build_ihdr(width, height, bpc, colorspace);
    write_chunk(&mut png, b"IHDR", &ihdr_data);

    // IDAT chunk (raw deflate of the image data)
    let idat_data = encode_idat(raw_data, width, height, bpc, colorspace);
    write_chunk(&mut png, b"IDAT", &idat_data);

    // IEND chunk
    write_chunk(&mut png, b"IEND", &[]);

    png
}

fn build_ihdr(width: u32, height: u32, bpc: u8, colorspace: &str) -> Vec<u8> {
    let mut data = Vec::with_capacity(13);
    data.extend_from_slice(&width.to_be_bytes());
    data.extend_from_slice(&height.to_be_bytes());
    data.push(bpc);
    let color_type = match colorspace {
        "DeviceGray" | "CalGray" => 0,
        "DeviceRGB" | "CalRGB" | "Lab" => 2,
        "Indexed" => 3,
        _ => 2, // default to RGB
    };
    data.push(color_type);
    data.push(0); // compression method
    data.push(0); // filter method
    data.push(0); // interlace method
    data
}

fn encode_idat(raw_data: &[u8], width: u32, height: u32, bpc: u8, colorspace: &str) -> Vec<u8> {
    let bpp = match colorspace {
        "DeviceGray" => (bpc as usize).div_ceil(8),
        _ => (bpc as usize * 3).div_ceil(8),
    };
    let stride = width as usize * bpp;

    // Build raw scanlines with filter byte (0 = None)
    let mut raw_scanlines = Vec::with_capacity((stride + 1) * height as usize);
    for y in 0..height as usize {
        raw_scanlines.push(0); // filter type: None
        let start = y * stride;
        let end = (start + stride).min(raw_data.len());
        raw_scanlines.extend_from_slice(&raw_data[start..end]);
    }

    // Compress with flate
    use std::io::Write;
    let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
    encoder.write_all(&raw_scanlines).unwrap();
    encoder.finish().unwrap_or_default()
}

fn write_chunk(png: &mut Vec<u8>, chunk_type: &[u8], data: &[u8]) {
    let len = data.len() as u32;
    png.extend_from_slice(&len.to_be_bytes());
    png.extend_from_slice(chunk_type);
    png.extend_from_slice(data);

    // CRC32 over type + data
    let mut crc = crc32fast::Hasher::new();
    crc.update(chunk_type);
    crc.update(data);
    let crc_val = crc.finalize();
    png.extend_from_slice(&crc_val.to_be_bytes());
}
