//! Stable error model for the Git core.
//!
//! Repository data is adversarial input (see PLAN "Security contract"): every
//! parser fails **closed** — it returns a typed [`ForgeError`] rather than
//! panicking on malformed objects, truncated headers, or corrupt compression.

use thiserror::Error;

/// Crate-wide result alias.
pub type Result<T> = std::result::Result<T, ForgeError>;

/// The stable error enum for the Git object core.
#[derive(Debug, Error)]
pub enum ForgeError {
    /// Underlying filesystem / IO failure.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    /// No `.git` directory was found while discovering a repository.
    #[error("not a git repository (or any parent): {0}")]
    NotARepository(String),

    /// An object's on-disk bytes could not be parsed (bad header, truncated
    /// payload, declared length mismatch, non-UTF-8 header, etc.).
    #[error("malformed {kind} object: {reason}")]
    MalformedObject {
        /// The object kind being parsed (or "loose" before the type is known).
        kind: &'static str,
        /// Human-readable reason.
        reason: String,
    },

    /// A requested object does not exist in the loose object database.
    #[error("object not found: {0}")]
    ObjectNotFound(String),

    /// Content verification failed: the SHA-1 of the stored bytes did not match
    /// the object id it was looked up under. Signals repository corruption.
    #[error("object {looked_up} is corrupt: content hashes to {computed}")]
    HashMismatch {
        /// The oid the object was requested by.
        looked_up: String,
        /// The oid actually computed from its bytes.
        computed: String,
    },

    /// A hex object id was the wrong length or contained non-hex characters.
    #[error("invalid object id: {0}")]
    InvalidOid(String),

    /// A ref/revision could not be resolved to an object id.
    #[error("cannot resolve reference: {0}")]
    InvalidRef(String),

    /// An abbreviated object id prefix matched more than one object.
    #[error("ambiguous object id prefix: {0}")]
    AmbiguousPrefix(String),

    /// The object header named a type that is not blob/tree/commit/tag.
    #[error("unknown object type: {0}")]
    UnknownType(String),

    /// zlib decompression of a loose object failed or exceeded the size cap.
    #[error("decompression failed: {0}")]
    Decompress(String),

    /// A packfile could not be opened or an object could not be decoded from a
    /// pack (corrupt pack, broken delta chain, or an error surfaced by the
    /// gitoxide `gix-pack`/`gix-odb` engine). Repository data is adversarial, so
    /// pack decode errors fail **closed** as a typed error rather than a panic.
    #[error("packfile error: {0}")]
    Pack(String),
}

impl ForgeError {
    /// Convenience constructor for a malformed-object error.
    pub fn malformed(kind: &'static str, reason: impl Into<String>) -> Self {
        ForgeError::MalformedObject {
            kind,
            reason: reason.into(),
        }
    }
}
