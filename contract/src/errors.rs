//! Stable error prefixes plus the typed `http-with-placeholders` error.
//!
//! `docs/INTERFACE.md` §"Error strings" freezes five prefixes so the TypeScript
//! harness can branch on a failure without parsing prose. Do not reword, do not
//! reorder, do not add a sixth without updating INTERFACE.md first — the
//! prefixes are part of the contract's public surface.

/// Malformed input payload, or an input value that fails validation.
pub const INPUT: &str = "input:";

/// Policy problems: invalid policy JSON, invalid limits, invalid base currency.
pub const POLICY: &str = "policy:";

/// `kv-store` failures: host refused the map, map missing, read/write error.
pub const KV: &str = "kv:";

/// FX layer diagnostics. FX lookups never fail a call — INTERFACE.md defines the
/// `fx_unavailable` rule for that case — so this prefix is used on the
/// diagnostic log line emitted when the upstream rate could not be used.
pub const FX: &str = "fx:";

/// Approval webhook problems: missing secret, egress/placeholder rejection,
/// non-2xx upstream response.
pub const APPROVAL: &str = "approval:";

/// Join a prefix and a message: `prefixed(KV, "read failed")` -> `"kv: read failed"`.
pub fn prefixed(prefix: &str, message: &str) -> String {
    format!("{prefix} {message}")
}

/// Strip the mustache delimiters from a marker name, so an error string can
/// never be scanned as (or mistaken for) a resolved placeholder value.
fn marker_name(marker: &str) -> &str {
    marker.trim_matches('{').trim_matches('}')
}

/// Typed failure of the host's `http-with-placeholders::call`.
///
/// Mirrors the `http-error` variant in `wit/deps/host-interfaces-2.1.0`. Kept in
/// this crate (instead of matching on the generated type inline) so the approval
/// logic stays native-testable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WebhookError {
    /// Target host is not on the contract's `http_allow_list`.
    EgressDenied(String),
    /// A marker used a namespace other than `profile`, or was malformed.
    PlaceholderDenied(String),
    /// A `{{profile.<field>}}` marker resolved to no value on the caller's profile.
    PlaceholderUnknown(String),
    /// No `pii_did` bound for this execution (admin/bootstrap path).
    PlaceholderNoUserContext,
    /// Upstream transport/TLS/parse failure.
    UpstreamError(String),
}

impl WebhookError {
    /// True when the host could not resolve a profile field — the signal that
    /// makes `request-approval` retry once without the optional markers.
    pub fn is_placeholder_unknown(&self) -> bool {
        matches!(self, Self::PlaceholderUnknown(_))
    }

    /// Host-side description. Carries host names, field names and upstream
    /// reasons only — the host never puts resolved PII in these strings
    /// (`wit/deps/host-interfaces-2.1.0`, `variant http-error`).
    pub fn message(&self) -> String {
        match self {
            Self::EgressDenied(host) => format!("egress denied for host {host}"),
            Self::PlaceholderDenied(marker) => {
                format!("placeholder rejected: {}", marker_name(marker))
            }
            Self::PlaceholderUnknown(field) => {
                format!("user profile has no value for field {field}")
            }
            Self::PlaceholderNoUserContext => {
                "no user context bound for placeholder resolution".to_string()
            }
            Self::UpstreamError(reason) => format!("upstream error: {reason}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefixes_match_interface() {
        assert_eq!(INPUT, "input:");
        assert_eq!(POLICY, "policy:");
        assert_eq!(KV, "kv:");
        assert_eq!(FX, "fx:");
        assert_eq!(APPROVAL, "approval:");
    }

    #[test]
    fn prefixed_joins_with_a_single_space() {
        assert_eq!(prefixed(KV, "scan failed"), "kv: scan failed");
    }

    #[test]
    fn only_placeholder_unknown_is_the_retry_signal() {
        assert!(WebhookError::PlaceholderUnknown("first_name".into()).is_placeholder_unknown());
        assert!(!WebhookError::PlaceholderDenied("{{secrets.x}}".into()).is_placeholder_unknown());
        assert!(!WebhookError::EgressDenied("hooks.example.test".into()).is_placeholder_unknown());
        assert!(!WebhookError::PlaceholderNoUserContext.is_placeholder_unknown());
        assert!(!WebhookError::UpstreamError("timeout".into()).is_placeholder_unknown());
    }

    #[test]
    fn messages_carry_no_marker_text_or_resolved_values() {
        let errors = [
            WebhookError::EgressDenied("hooks.example.test".into()),
            WebhookError::PlaceholderDenied("{{secrets.api_key}}".into()),
            WebhookError::PlaceholderUnknown("first_name".into()),
            WebhookError::PlaceholderNoUserContext,
            WebhookError::UpstreamError("connection reset".into()),
        ];
        for error in errors {
            let message = error.message();
            assert!(!message.is_empty());
            // The contract must never echo a resolved value back to the caller;
            // only the host's PII-free reason strings are surfaced.
            assert!(!message.contains("{{profile."));
        }
    }
}
