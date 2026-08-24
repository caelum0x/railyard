//! Runtime ingress bounds.

/// Default maximum JSON request size: 2 MiB.
pub const DEFAULT_MAX_REQUEST_BYTES: usize = 2 * 1024 * 1024;
/// Keep operator configuration within a deliberate 1 KiB–16 MiB range.
pub const MIN_MAX_REQUEST_BYTES: usize = 1024;
pub const MAX_MAX_REQUEST_BYTES: usize = 16 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RuntimeLimits {
    pub max_request_bytes: usize,
}

impl Default for RuntimeLimits {
    fn default() -> Self {
        Self {
            max_request_bytes: DEFAULT_MAX_REQUEST_BYTES,
        }
    }
}

impl RuntimeLimits {
    pub fn new(max_request_bytes: usize) -> Result<Self, &'static str> {
        if !(MIN_MAX_REQUEST_BYTES..=MAX_MAX_REQUEST_BYTES).contains(&max_request_bytes) {
            return Err("request body limit must be between 1024 and 16777216 bytes");
        }
        Ok(Self { max_request_bytes })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_disabled_or_excessive_body_limits() {
        assert!(RuntimeLimits::new(0).is_err());
        assert!(RuntimeLimits::new(MAX_MAX_REQUEST_BYTES + 1).is_err());
        assert_eq!(
            RuntimeLimits::new(DEFAULT_MAX_REQUEST_BYTES).unwrap(),
            RuntimeLimits::default()
        );
    }
}
