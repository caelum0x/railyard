//! Blob objects: opaque file contents. Git applies no interpretation, so a blob
//! is simply its bytes; the payload *is* the data.

/// A blob object.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Blob {
    /// Raw file contents.
    pub data: Vec<u8>,
}

impl Blob {
    /// Wrap raw bytes as a blob.
    pub fn new(data: Vec<u8>) -> Self {
        Self { data }
    }

    /// The contents as a lossy UTF-8 string, for display.
    pub fn as_str_lossy(&self) -> std::borrow::Cow<'_, str> {
        String::from_utf8_lossy(&self.data)
    }

    /// Byte length of the blob.
    pub fn len(&self) -> usize {
        self.data.len()
    }

    /// Whether the blob is empty.
    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }
}
