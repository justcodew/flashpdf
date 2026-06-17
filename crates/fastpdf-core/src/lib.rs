#![allow(
    clippy::type_complexity,
    clippy::collapsible_match,
    clippy::manual_clamp,
    clippy::needless_range_loop,
    clippy::if_same_then_else,
    clippy::unnecessary_cast,
    clippy::manual_range_contains,
    clippy::needless_return
)]

pub mod document;
pub mod extract;
pub mod font;
pub mod image;
pub mod layout;
pub mod links;
pub mod parser;
pub mod types;

pub use document::Document;
pub use extract::{extract, extract_doc, extract_many, ExtractOptions, ExtractResult, PageResult};
pub use font::{build_font_map, parse_cmap, CIDFontInfo, CIDWidthRange, CMap, FontInfo};
pub use image::{encode_png, resolve_images, ExtractedImage, ImageData};
pub use layout::cluster_chars;
pub use links::{extract_links, PageLink};
pub use parser::{
    content_stream, parse_object, parse_object_from_bytes, scan_content_stream, ContentResult,
    Cursor, ImageRef, ParseError, ParseResult, TextBlock, TextLine, TextSpan,
};
pub use types::{ObjectId, PdfObject};
