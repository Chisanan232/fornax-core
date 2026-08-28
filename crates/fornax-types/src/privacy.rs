//! Cloud-egress policy gate (FORNX-33). No cloud sync code exists yet
//! (FORNX-41), but the policy primitive is introduced now, ahead of it, so
//! that ticket has no path to silently uploading anything: it must consult
//! this gate, which defaults to closed.

/// Whether any Fornax-originated data may leave this machine. Defaults to
/// `false` — cloud sync is opt-in, never assumed. FORNX-41's uploader must
/// check this before any network call, not just at startup, so a user can
/// disable sync mid-session and have it take effect immediately.
pub fn cloud_sync_allowed() -> bool {
    std::env::var("FORNAX_CLOUD_SYNC_ENABLED")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    // One test, not several: std::env::set_var is process-global and cargo
    // runs tests in parallel by default, so splitting these across tests
    // would race on the same env var.
    #[test]
    fn cloud_sync_gate_defaults_closed_and_requires_an_explicit_true_value() {
        std::env::remove_var("FORNAX_CLOUD_SYNC_ENABLED");
        assert!(!cloud_sync_allowed(), "must default to disabled when unset");

        std::env::set_var("FORNAX_CLOUD_SYNC_ENABLED", "1");
        assert!(cloud_sync_allowed());

        std::env::set_var("FORNAX_CLOUD_SYNC_ENABLED", "true");
        assert!(cloud_sync_allowed());

        std::env::set_var("FORNAX_CLOUD_SYNC_ENABLED", "yes");
        assert!(
            !cloud_sync_allowed(),
            "only '1'/'true' enable sync, not other truthy-looking strings"
        );

        std::env::remove_var("FORNAX_CLOUD_SYNC_ENABLED");
    }
}
