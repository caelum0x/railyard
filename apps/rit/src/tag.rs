//! Annotated tag objects.
//!
//! Payload layout:
//!
//! ```text
//! object <40-hex>\n
//! type <object-type>\n
//! tag <name>\n
//! tagger <signature>\n
//! \n
//! <message>
//! ```

use crate::error::{ForgeError, Result};
use crate::headers::{self, Header};
use crate::object::ObjectType;
use crate::oid::ObjectId;
use crate::signature::Signature;

/// A parsed annotated tag.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tag {
    /// The tagged object's id.
    pub object: ObjectId,
    /// The tagged object's type.
    pub object_type: ObjectType,
    /// The tag name (e.g. `v1.0.0`).
    pub tag: String,
    /// The tagger identity (Git always writes one; lightweight tags are refs,
    /// not tag objects, so this is expected to be present).
    pub tagger: Option<Signature>,
    /// Any other headers, preserved in order for byte-exact serialization.
    pub extra_headers: Vec<Header>,
    /// The tag message body.
    pub message: Vec<u8>,
}

impl Tag {
    /// Parse a tag payload, failing closed on missing required headers.
    pub fn parse(payload: &[u8]) -> Result<Self> {
        let block = headers::parse(payload, "tag")?;

        let mut object = None;
        let mut object_type = None;
        let mut tag = None;
        let mut tagger = None;
        let mut extra_headers = Vec::new();

        for h in block.headers {
            match h.key.as_str() {
                "object" => {
                    let hex = std::str::from_utf8(&h.value)
                        .map_err(|_| ForgeError::malformed("tag", "object oid not ASCII"))?;
                    object = Some(ObjectId::from_hex(hex)?);
                }
                "type" => {
                    let s = std::str::from_utf8(&h.value)
                        .map_err(|_| ForgeError::malformed("tag", "type not ASCII"))?;
                    object_type = Some(ObjectType::parse(s)?);
                }
                "tag" => {
                    tag = Some(
                        std::str::from_utf8(&h.value)
                            .map_err(|_| ForgeError::malformed("tag", "tag name not UTF-8"))?
                            .to_string(),
                    );
                }
                "tagger" => {
                    let v = std::str::from_utf8(&h.value)
                        .map_err(|_| ForgeError::malformed("tag", "tagger not UTF-8"))?;
                    tagger = Some(Signature::parse(v, "tag")?);
                }
                _ => extra_headers.push(h),
            }
        }

        Ok(Self {
            object: object.ok_or_else(|| ForgeError::malformed("tag", "missing object header"))?,
            object_type: object_type
                .ok_or_else(|| ForgeError::malformed("tag", "missing type header"))?,
            tag: tag.ok_or_else(|| ForgeError::malformed("tag", "missing tag header"))?,
            tagger,
            extra_headers,
            message: block.message,
        })
    }

    /// Serialize back to the exact on-disk payload.
    pub fn serialize(&self) -> Vec<u8> {
        let mut out = Vec::new();
        push_header(&mut out, "object", self.object.to_hex().as_bytes());
        push_header(&mut out, "type", self.object_type.as_str().as_bytes());
        push_header(&mut out, "tag", self.tag.as_bytes());
        if let Some(tagger) = &self.tagger {
            push_header(&mut out, "tagger", tagger.to_raw().as_bytes());
        }
        for h in &self.extra_headers {
            push_header(&mut out, &h.key, &h.value);
        }
        out.push(b'\n');
        out.extend_from_slice(&self.message);
        out
    }

    /// The message as a lossy UTF-8 string.
    pub fn message_str(&self) -> std::borrow::Cow<'_, str> {
        String::from_utf8_lossy(&self.message)
    }
}

fn push_header(out: &mut Vec<u8>, key: &str, value: &[u8]) {
    out.extend_from_slice(key.as_bytes());
    out.push(b' ');
    out.extend_from_slice(&headers::serialize_value(value));
    out.push(b'\n');
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_serialize_roundtrips() {
        let payload = concat!(
            "object e69de29bb2d1d6434b8b29ae775ad8c2e48c5391\n",
            "type commit\n",
            "tag v1.0.0\n",
            "tagger Alice <a@b.c> 1700000000 +0000\n",
            "\n",
            "release one\n",
        )
        .as_bytes()
        .to_vec();

        let tag = Tag::parse(&payload).unwrap();
        assert_eq!(tag.tag, "v1.0.0");
        assert_eq!(tag.object_type, ObjectType::Commit);
        assert!(tag.tagger.is_some());
        assert_eq!(tag.serialize(), payload);
    }

    #[test]
    fn rejects_missing_object() {
        let payload = b"type commit\ntag v1\ntagger A <a@b.c> 1 +0000\n\nmsg\n";
        assert!(Tag::parse(payload).is_err());
    }
}
