//! Scopes.

pub const READ: &str = "tools:read";
pub const CALL: &str = "tools:call";
pub const WRITE: &str = "tools:write";
pub const SEND: &str = "tools:send";
pub const SPEND: &str = "tools:spend";
pub const ADMIN: &str = "admin";

pub const ALL: &[&str] = &[READ, CALL, WRITE, SEND, SPEND, ADMIN];
/// Scopes pre-ticked on the consent screen: write, send and spend are opt-in.
pub const DEFAULT_CONSENT: &[&str] = &[READ, CALL];

pub fn is_known(s: &str) -> bool {
    ALL.contains(&s)
}

/// Parse a space- or comma-separated scope string, dropping unknown scopes.
pub fn parse(s: &str) -> Vec<String> {
    let mut v: Vec<String> = s.split([' ', ',']).map(str::trim).filter(|x| is_known(x)).map(str::to_string).collect();
    v.sort();
    v.dedup();
    v
}

/// `admin` implies every other scope.
pub fn has(granted: &[String], needed: &str) -> bool {
    granted.iter().any(|g| g == needed || g == ADMIN)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_and_has() {
        let s = parse("tools:read tools:call,bogus tools:read");
        assert_eq!(s, vec!["tools:call".to_string(), "tools:read".to_string()]);
        assert!(has(&s, CALL));
        assert!(!has(&s, SEND));
        assert!(has(&["admin".to_string()], SPEND));
    }
}
