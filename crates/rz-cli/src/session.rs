//! Session identity — scopes all NATS subjects, streams, and registry keys
//! so identically-named agents in different sessions (or on different
//! machines sharing one NATS server) never collide.
//!
//! Precedence: `RZ_SESSION` (explicit override, e.g. for cross-machine
//! teams) > `CMUX_WORKSPACE_ID` > `ZELLIJ_SESSION_NAME` > `"default"`.

/// The current session ID. Cached for the lifetime of the process.
pub fn session_id() -> String {
    use std::sync::OnceLock;
    static SID: OnceLock<String> = OnceLock::new();
    SID.get_or_init(|| {
        for var in ["RZ_SESSION", "CMUX_WORKSPACE_ID", "ZELLIJ_SESSION_NAME"] {
            if let Ok(v) = std::env::var(var) {
                let v = v.trim().to_string();
                if !v.is_empty() {
                    return v;
                }
            }
        }
        "default".to_string()
    })
    .clone()
}

/// Registry key for an agent name within the current session.
pub fn scoped_key(name: &str) -> String {
    format!("{}.{}", session_id(), name)
}

/// Strip the current session prefix from a scoped key.
/// Returns `None` if the key belongs to a different session.
pub fn unscope_key(key: &str) -> Option<&str> {
    key.strip_prefix(&format!("{}.", session_id()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scoped_roundtrip() {
        let key = scoped_key("worker");
        assert!(key.ends_with(".worker"));
        assert_eq!(unscope_key(&key), Some("worker"));
        assert_eq!(unscope_key("other-session.worker"), None);
    }
}
