use std::fmt;

/// A PDF indirect object reference: `N G R`
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ObjectId {
    pub num: u32,
    pub gen: u16,
}

impl ObjectId {
    pub fn new(num: u32, gen: u16) -> Self {
        Self { num, gen }
    }
}

impl fmt::Display for ObjectId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {} R", self.num, self.gen)
    }
}

/// Zero-copy PDF object representation.
/// All string-like data references the original mmap bytes directly.
#[derive(Debug, Clone)]
pub enum PdfObject<'a> {
    /// null
    Null,
    /// boolean
    Bool(bool),
    /// integer number
    Integer(i64),
    /// real (floating-point) number
    Real(f64),
    /// byte string `(...)` — raw bytes, no decoding
    String(&'a [u8]),
    /// hex string `<...>` — raw hex bytes
    HexString(&'a [u8]),
    /// name `/Name` — raw bytes (after `#XX` decoding)
    Name(&'a [u8]),
    /// array `[...]`
    Array(Vec<PdfObject<'a>>),
    /// dictionary `<< /Key Value ... >>`
    Dict(Vec<(&'a [u8], PdfObject<'a>)>),
    /// indirect reference `N G R`
    Ref(ObjectId),
    /// stream: the dict header + slice of compressed data (not yet decoded)
    Stream {
        dict: Vec<(&'a [u8], PdfObject<'a>)>,
        data: &'a [u8],
    },
}

impl<'a> PdfObject<'a> {
    pub fn type_name(&self) -> &'static str {
        match self {
            PdfObject::Null => "null",
            PdfObject::Bool(_) => "boolean",
            PdfObject::Integer(_) => "integer",
            PdfObject::Real(_) => "real",
            PdfObject::String(_) => "string",
            PdfObject::HexString(_) => "hexstring",
            PdfObject::Name(_) => "name",
            PdfObject::Array(_) => "array",
            PdfObject::Dict(_) => "dictionary",
            PdfObject::Ref(_) => "reference",
            PdfObject::Stream { .. } => "stream",
        }
    }

    pub fn as_i64(&self) -> Option<i64> {
        match self {
            PdfObject::Integer(n) => Some(*n),
            _ => None,
        }
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self {
            PdfObject::Real(f) => Some(*f),
            PdfObject::Integer(n) => Some(*n as f64),
            _ => None,
        }
    }

    pub fn as_name(&self) -> Option<&'a [u8]> {
        match self {
            PdfObject::Name(n) => Some(n),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&'a [u8]> {
        match self {
            PdfObject::String(s) => Some(s),
            PdfObject::HexString(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_dict(&self) -> Option<&[(&'a [u8], PdfObject<'a>)]> {
        match self {
            PdfObject::Dict(d) => Some(d),
            PdfObject::Stream { dict, .. } => Some(dict),
            _ => None,
        }
    }

    pub fn as_array(&self) -> Option<&[PdfObject<'a>]> {
        match self {
            PdfObject::Array(a) => Some(a),
            _ => None,
        }
    }

    pub fn as_ref(&self) -> Option<ObjectId> {
        match self {
            PdfObject::Ref(r) => Some(*r),
            _ => None,
        }
    }

    /// Look up a key in a Dict or Stream object (linear scan, dict is small).
    pub fn get(&self, key: &[u8]) -> Option<&PdfObject<'a>> {
        self.as_dict()?
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, v)| v)
    }

    pub fn is_null(&self) -> bool {
        matches!(self, PdfObject::Null)
    }

    pub fn is_stream(&self) -> bool {
        matches!(self, PdfObject::Stream { .. })
    }
}
