//! Approval webhook: PII-safe POST through `http-with-placeholders`.
//!
//! The webhook URL is tenant-provisioned — `z:<tid>:secrets` / key
//! `approval_webhook_url`. There is no default and no fallback URL: when the
//! secret is absent the call fails with an `approval:` error.
//!
//! The POST body carries `{{profile.*}}` markers instead of resolved identity
//! data, so plaintext PII never enters WASM memory or the audit ledger. When the
//! host reports a marker as unresolvable (`placeholder-unknown`) the contract
//! retries exactly once with the optional markers dropped, and records
//! `profile_placeholder_fallback` in the audit record.

use serde::{Deserialize, Serialize};

use crate::capabilities::{Host, HttpResponse};
use crate::errors;
use crate::ledger::AuditRecord;
use crate::policy::Verdict;

/// `z:<tid>:secrets` key holding the approval webhook URL.
pub const SECRET_KEY: &[u8] = b"approval_webhook_url";
/// Recorded in the audit `rule_hits` when the fallback retry happened.
pub const RULE_PROFILE_PLACEHOLDER_FALLBACK: &str = "profile_placeholder_fallback";
/// Marker for the requester's given name.
pub const MARKER_FIRST_NAME: &str = "{{profile.first_name}}";
/// Marker for the requester's family name.
pub const MARKER_LAST_NAME: &str = "{{profile.last_name}}";
/// Marker for the requester's verified e-mail.
pub const MARKER_EMAIL: &str = "{{profile.verified_contacts.email.value}}";
/// `status` value returned when the webhook accepted the request.
pub const STATUS_SENT: &str = "sent";
/// Body type discriminator sent to the webhook.
const BODY_TYPE: &str = "expense_approval_request";
/// Correlation header carrying the approval reference.
const HEADER_APPROVAL_REF: &str = "x-approval-ref";
/// Longest accepted free-form note.
const MAX_NOTE_LEN: usize = 500;

/// `request-approval` input payload.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApprovalInput {
    pub expense_id: String,
    /// Ask the host to resolve `{{profile.*}}` markers for this call.
    #[serde(default)]
    pub use_profile: bool,
    #[serde(default)]
    pub note: Option<String>,
}

/// Result of a delivered approval request.
#[derive(Debug, Clone, PartialEq)]
pub struct ApprovalOutcome {
    pub http_code: u16,
    pub approval_ref: String,
    pub placeholder_fallback: bool,
}

/// The JSON body posted to the webhook. Field order is stable so the request is
/// byte-for-byte reproducible in tests.
#[derive(Debug, Serialize)]
struct WebhookPayload<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    approval_ref: &'a str,
    expense_id: &'a str,
    verdict: &'a str,
    base_amount: Option<f64>,
    category: &'a str,
    vendor: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    note: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    requested_by: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    requested_by_email: Option<&'static str>,
    contract_version: &'a str,
}

/// Validate the `request-approval` payload.
///
/// The note is rejected when it contains `{{`: caller-supplied prose must not be
/// able to smuggle placeholder markers into a request that the host resolves.
pub fn validate_input(input: &ApprovalInput) -> Result<(), String> {
    if input.expense_id.trim().is_empty() {
        return Err(errors::prefixed(errors::INPUT, "expense_id is required"));
    }
    if input.expense_id.len() > 128 {
        return Err(errors::prefixed(errors::INPUT, "expense_id is too long"));
    }
    if let Some(note) = &input.note {
        if note.contains("{{") {
            return Err(errors::prefixed(
                errors::INPUT,
                "note must not contain placeholder markers",
            ));
        }
        if note.len() > MAX_NOTE_LEN {
            return Err(errors::prefixed(errors::INPUT, "note is too long"));
        }
    }
    Ok(())
}

/// Read the tenant's approval webhook URL from the secrets map.
///
/// Fails with `approval:` when the key is missing (never invents a default) and
/// with `kv:` when the secrets map itself cannot be read.
pub fn webhook_url(host: &mut impl Host, secrets_map: &str) -> Result<String, String> {
    let value = host
        .kv_get(secrets_map, SECRET_KEY)
        .map_err(|e| errors::prefixed(errors::KV, &format!("secrets read: {e}")))?;
    let bytes = value.ok_or_else(|| {
        errors::prefixed(
            errors::APPROVAL,
            "approval_webhook_url is not provisioned in z:<tid>:secrets",
        )
    })?;
    let url = String::from_utf8(bytes).map_err(|_| {
        errors::prefixed(errors::APPROVAL, "approval_webhook_url is not valid UTF-8")
    })?;
    let url = url.trim().to_string();
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return Err(errors::prefixed(
            errors::APPROVAL,
            "approval_webhook_url must be an http(s) URL",
        ));
    }
    Ok(url)
}

/// Build the webhook body. `with_profile` adds the optional `{{profile.*}}`
/// markers; the retry after `placeholder-unknown` passes `false`.
pub fn build_body(
    input: &ApprovalInput,
    previous: Option<&AuditRecord>,
    with_profile: bool,
    approval_ref: &str,
) -> Result<Vec<u8>, String> {
    let payload = WebhookPayload {
        kind: BODY_TYPE,
        approval_ref,
        expense_id: &input.expense_id,
        verdict: Verdict::NeedsApproval.as_str(),
        base_amount: previous.and_then(|record| record.base_amount),
        category: previous
            .map(|record| record.category.as_str())
            .unwrap_or_default(),
        vendor: previous
            .map(|record| record.vendor.as_str())
            .unwrap_or_default(),
        note: input.note.as_deref(),
        requested_by: with_profile.then(|| format!("{MARKER_FIRST_NAME} {MARKER_LAST_NAME}")),
        requested_by_email: with_profile.then_some(MARKER_EMAIL),
        contract_version: crate::CONTRACT_VERSION,
    };
    serde_json::to_vec(&payload).map_err(|e| {
        errors::prefixed(
            errors::APPROVAL,
            &format!("could not encode webhook body: {e}"),
        )
    })
}

/// Deterministic approval reference: `appr-<16 hex>` derived from the expense,
/// its ledger sequence and the cluster time (FNV-1a, no external dependency).
pub fn approval_ref(expense_id: &str, ledger_seq: u64, recorded_at: u64) -> String {
    let seed = format!("{expense_id}|{ledger_seq}|{recorded_at}");
    format!("appr-{:016x}", fnv1a64(seed.as_bytes()))
}

fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// POST the approval request, retrying once without profile markers when the
/// host refuses to resolve them.
///
/// `Content-Type` is intentionally not set: the host's HTTP function applies
/// `application/json` itself, and an explicit header would duplicate it.
pub fn send(
    host: &mut impl Host,
    url: &str,
    input: &ApprovalInput,
    previous: Option<&AuditRecord>,
    approval_ref: &str,
) -> Result<ApprovalOutcome, String> {
    let headers = vec![(HEADER_APPROVAL_REF.to_string(), approval_ref.to_string())];

    let body = build_body(input, previous, input.use_profile, approval_ref)?;
    match host.http_post_with_placeholders(url, &headers, &body) {
        Ok(response) => finish(response, approval_ref, false),
        Err(error) if error.is_placeholder_unknown() && input.use_profile => {
            host.log_info(&errors::prefixed(
                errors::APPROVAL,
                &format!("{}; retrying once without profile markers", error.message()),
            ));
            let body = build_body(input, previous, false, approval_ref)?;
            let response = host
                .http_post_with_placeholders(url, &headers, &body)
                .map_err(|error| errors::prefixed(errors::APPROVAL, &error.message()))?;
            finish(response, approval_ref, true)
        }
        Err(error) => Err(errors::prefixed(errors::APPROVAL, &error.message())),
    }
}

fn finish(
    response: HttpResponse,
    approval_ref: &str,
    placeholder_fallback: bool,
) -> Result<ApprovalOutcome, String> {
    if !response.is_success() {
        return Err(errors::prefixed(
            errors::APPROVAL,
            &format!("webhook returned HTTP {}", response.code),
        ));
    }
    Ok(ApprovalOutcome {
        http_code: response.code,
        approval_ref: approval_ref.to_string(),
        placeholder_fallback,
    })
}

/// Build the audit record for an approval request.
///
/// The frozen record shape has no claim fields of its own, so they are inherited
/// from the expense's latest `check-expense` record; without one they stay empty
/// (see `README.md` for the INTERFACE.md gap this works around).
pub fn audit_record(
    input: &ApprovalInput,
    previous: Option<&AuditRecord>,
    outcome: &ApprovalOutcome,
    recorded_at: u64,
) -> AuditRecord {
    AuditRecord {
        seq: 0,
        expense_id: input.expense_id.clone(),
        employee_ref: previous
            .map(|record| record.employee_ref.clone())
            .unwrap_or_default(),
        verdict: Verdict::NeedsApproval.as_str().to_string(),
        base_amount: previous.and_then(|record| record.base_amount),
        category: previous
            .map(|record| record.category.clone())
            .unwrap_or_default(),
        vendor: previous
            .map(|record| record.vendor.clone())
            .unwrap_or_default(),
        rule_hits: if outcome.placeholder_fallback {
            vec![RULE_PROFILE_PLACEHOLDER_FALLBACK.to_string()]
        } else {
            Vec::new()
        },
        recorded_at,
        contract_version: crate::CONTRACT_VERSION.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::errors::WebhookError;
    use crate::test_support::MockHost;

    const SECRETS: &str = "z:t:secrets";
    const NOW: u64 = 1_700_000_000;
    const URL: &str = "https://hooks.example.test/expense-approval";

    fn input() -> ApprovalInput {
        ApprovalInput {
            expense_id: "EXP-1042".to_string(),
            use_profile: true,
            note: Some("client dinner".to_string()),
        }
    }

    fn previous_record() -> AuditRecord {
        AuditRecord {
            seq: 7,
            expense_id: "EXP-1042".to_string(),
            employee_ref: "emp_7f3a".to_string(),
            verdict: "needs_approval".to_string(),
            base_amount: Some(884.52),
            category: "travel".to_string(),
            vendor: "Lufthansa".to_string(),
            rule_hits: vec!["over_approval_threshold".to_string()],
            recorded_at: NOW,
            contract_version: crate::CONTRACT_VERSION.to_string(),
        }
    }

    fn host_with_secret() -> MockHost {
        let mut host = MockHost::new();
        host.seed(SECRETS, SECRET_KEY, URL.as_bytes());
        host
    }

    #[test]
    fn approval_ref_is_deterministic_and_prefixed() {
        let first = approval_ref("EXP-1042", 8, NOW);
        assert_eq!(first, approval_ref("EXP-1042", 8, NOW));
        assert!(first.starts_with("appr-"), "{first}");
        assert_eq!(first.len(), "appr-".len() + 16);
        assert_ne!(first, approval_ref("EXP-1042", 9, NOW));
        assert_ne!(first, approval_ref("EXP-1043", 8, NOW));
    }

    #[test]
    fn body_carries_placeholders_not_plaintext_identity() {
        let record = previous_record();
        let body = build_body(&input(), Some(&record), true, "appr-1").unwrap();
        let body = String::from_utf8(body).unwrap();

        assert!(body.contains(MARKER_FIRST_NAME), "{body}");
        assert!(body.contains(MARKER_LAST_NAME), "{body}");
        assert!(body.contains(MARKER_EMAIL), "{body}");
        assert!(body.contains("\"base_amount\":884.52"), "{body}");
        assert!(body.contains("\"category\":\"travel\""), "{body}");
        assert!(body.contains("\"note\":\"client dinner\""), "{body}");
        assert!(body.contains("\"approval_ref\":\"appr-1\""), "{body}");
    }

    #[test]
    fn body_without_profile_has_no_markers() {
        let record = previous_record();
        let body = build_body(&input(), Some(&record), false, "appr-1").unwrap();
        let body = String::from_utf8(body).unwrap();
        assert!(!body.contains("{{"), "{body}");
        assert!(!body.contains("requested_by"), "{body}");
        assert!(body.contains("\"expense_id\":\"EXP-1042\""), "{body}");
    }

    #[test]
    fn body_tolerates_an_unknown_expense() {
        let body = build_body(&input(), None, false, "appr-1").unwrap();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(value["base_amount"].is_null());
        assert_eq!(value["category"], "");
        assert_eq!(value["verdict"], "needs_approval");
    }

    #[test]
    fn validate_input_rejects_marker_injection_and_empty_ids() {
        let mut bad = input();
        bad.note = Some("thanks {{profile.verified_contacts.email.value}}".to_string());
        assert!(validate_input(&bad).unwrap_err().starts_with(errors::INPUT));

        bad.note = None;
        bad.expense_id = "  ".to_string();
        assert!(validate_input(&bad).unwrap_err().starts_with(errors::INPUT));

        bad.expense_id = "EXP-1".to_string();
        bad.note = Some("x".repeat(MAX_NOTE_LEN + 1));
        assert!(validate_input(&bad).unwrap_err().starts_with(errors::INPUT));

        assert!(validate_input(&input()).is_ok());
    }

    #[test]
    fn webhook_url_comes_from_the_secrets_map_only() {
        let mut host = host_with_secret();
        assert_eq!(webhook_url(&mut host, SECRETS).unwrap(), URL);

        let mut empty = MockHost::new();
        let error = webhook_url(&mut empty, SECRETS).unwrap_err();
        assert!(error.starts_with(errors::APPROVAL), "{error}");
        assert!(error.contains("approval_webhook_url"));

        let mut plain = MockHost::new();
        plain.seed(SECRETS, SECRET_KEY, b"ftp://example.test/hook");
        let error = webhook_url(&mut plain, SECRETS).unwrap_err();
        assert!(error.starts_with(errors::APPROVAL), "{error}");
    }

    #[test]
    fn webhook_url_reports_kv_failures_with_the_kv_prefix() {
        let mut host = MockHost::new();
        host.kv_get_failures
            .insert(SECRETS.to_string(), "map unavailable".to_string());
        let error = webhook_url(&mut host, SECRETS).unwrap_err();
        assert!(error.starts_with(errors::KV), "{error}");
    }

    #[test]
    fn send_posts_the_body_and_reports_the_status_code() {
        let mut host = host_with_secret();
        host.queue_webhook_ok(202, b"{\"ok\":true}");
        let outcome = send(&mut host, URL, &input(), Some(&previous_record()), "appr-1").unwrap();

        assert_eq!(outcome.http_code, 202);
        assert_eq!(outcome.approval_ref, "appr-1");
        assert!(!outcome.placeholder_fallback);
        assert_eq!(host.webhook_calls.len(), 1);
        assert_eq!(host.webhook_calls[0].0, URL);
        assert!(String::from_utf8_lossy(&host.webhook_calls[0].1).contains("{{profile."));
    }

    #[test]
    fn send_retries_once_without_markers_on_placeholder_unknown() {
        let mut host = host_with_secret();
        host.queue_webhook_err(WebhookError::PlaceholderUnknown(
            "verified_contacts.email.value".to_string(),
        ));
        host.queue_webhook_ok(200, b"{}");

        let outcome = send(&mut host, URL, &input(), Some(&previous_record()), "appr-1").unwrap();
        assert!(outcome.placeholder_fallback);
        assert_eq!(host.webhook_calls.len(), 2, "exactly one retry");
        assert!(String::from_utf8_lossy(&host.webhook_calls[0].1).contains("{{profile."));
        assert!(!String::from_utf8_lossy(&host.webhook_calls[1].1).contains("{{"));
        assert!(host
            .logs
            .iter()
            .any(|line| line.contains(RULE_PROFILE_PLACEHOLDER_FALLBACK)
                || line.contains("retrying")));
    }

    #[test]
    fn send_does_not_retry_when_profile_resolution_was_not_requested() {
        let mut host = host_with_secret();
        host.queue_webhook_err(WebhookError::PlaceholderUnknown("first_name".to_string()));
        let mut request = input();
        request.use_profile = false;

        let error = send(&mut host, URL, &request, None, "appr-1").unwrap_err();
        assert!(error.starts_with(errors::APPROVAL), "{error}");
        assert_eq!(host.webhook_calls.len(), 1);
    }

    #[test]
    fn send_does_not_retry_on_egress_or_placeholder_denial() {
        for failure in [
            WebhookError::EgressDenied("hooks.example.test".to_string()),
            WebhookError::PlaceholderDenied("{{profile.first_name}}".to_string()),
            WebhookError::PlaceholderNoUserContext,
            WebhookError::UpstreamError("connect timeout".to_string()),
        ] {
            let mut host = host_with_secret();
            host.queue_webhook_err(failure);
            let error = send(&mut host, URL, &input(), None, "appr-1").unwrap_err();
            assert!(error.starts_with(errors::APPROVAL), "{error}");
            assert_eq!(host.webhook_calls.len(), 1);
        }
    }

    #[test]
    fn send_fails_on_a_non_success_status() {
        let mut host = host_with_secret();
        host.queue_webhook_ok(500, b"boom");
        let error = send(&mut host, URL, &input(), None, "appr-1").unwrap_err();
        assert!(error.starts_with(errors::APPROVAL), "{error}");
        assert!(error.contains("500"), "{error}");
    }

    #[test]
    fn error_messages_never_contain_resolved_identity_data() {
        let messages = [
            WebhookError::EgressDenied("hooks.example.test".to_string()).message(),
            WebhookError::PlaceholderDenied(MARKER_EMAIL.to_string()).message(),
            WebhookError::PlaceholderUnknown("verified_contacts.email.value".to_string()).message(),
            WebhookError::PlaceholderNoUserContext.message(),
            WebhookError::UpstreamError("502 from upstream".to_string()).message(),
        ];
        for message in messages {
            assert!(!message.contains("{{"), "{message}");
            assert!(!message.contains('@'), "{message}");
        }
    }

    #[test]
    fn audit_record_inherits_the_claim_and_flags_the_fallback() {
        let record = previous_record();
        let outcome = ApprovalOutcome {
            http_code: 200,
            approval_ref: "appr-1".to_string(),
            placeholder_fallback: true,
        };
        let written = audit_record(&input(), Some(&record), &outcome, NOW + 5);

        assert_eq!(written.seq, 0, "the ledger assigns the sequence");
        assert_eq!(written.expense_id, "EXP-1042");
        assert_eq!(written.employee_ref, "emp_7f3a");
        assert_eq!(written.base_amount, Some(884.52));
        assert_eq!(written.category, "travel");
        assert_eq!(written.vendor, "Lufthansa");
        assert_eq!(written.verdict, "needs_approval");
        assert_eq!(
            written.rule_hits,
            vec![RULE_PROFILE_PLACEHOLDER_FALLBACK.to_string()]
        );
        assert_eq!(written.recorded_at, NOW + 5);
        assert_eq!(written.contract_version, crate::CONTRACT_VERSION);
    }

    #[test]
    fn audit_record_without_a_prior_claim_keeps_the_shape() {
        let outcome = ApprovalOutcome {
            http_code: 200,
            approval_ref: "appr-1".to_string(),
            placeholder_fallback: false,
        };
        let written = audit_record(&input(), None, &outcome, NOW);
        assert_eq!(written.employee_ref, "");
        assert_eq!(written.base_amount, None);
        assert_eq!(written.category, "");
        assert_eq!(written.vendor, "");
        assert!(written.rule_hits.is_empty());
        assert_eq!(written.expense_id, "EXP-1042");
    }
}
