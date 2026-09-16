//! # z-expense-guard
//!
//! Enterprise expense & invoice compliance for Trinity z-space tenants, exposed
//! as a T3N WASM component. The contract is deliberately small and auditable:
//!
//! | export | purpose | maps touched |
//! |---|---|---|
//! | `health` | identity + cluster timestamp probe | — |
//! | `set-policy` | store the tenant policy document | `policy` |
//! | `get-policy` | read it back (built-in default when unset) | `policy` |
//! | `check-expense` | FX convert, evaluate rules, append the audit record | `policy`, `fx`, `audit` |
//! | `request-approval` | POST a PII-safe approval request to the tenant webhook | `secrets`, `audit` |
//! | `get-audit` | page the audit ledger, newest first | `audit` |
//!
//! Design notes:
//!
//! * All host access goes through [`capabilities::Host`], which makes the whole
//!   contract natively unit-testable and confines `wasm32`-only code to
//!   [`capabilities::WasmHost`].
//! * PII never enters this component: employee identity stays in the host and is
//!   referenced only through `{{profile.*}}` markers, and the ledger stores an
//!   opaque `employee_ref` only.
//! * Timestamps always come from `tenant-context` cluster time, never from
//!   `std::time` (unavailable on `wasm32-wasip2`).
//! * Errors are returned as `String` with a stable prefix, see [`errors`].

#![warn(clippy::style, missing_debug_implementations)]
#![cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]

extern crate alloc;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::capabilities::Host;

pub mod approval;
pub mod capabilities;
pub mod errors;
pub mod fx;
pub mod ledger;
pub mod maps;
pub mod policy;

#[cfg(test)]
mod test_support;

/// Contract version, reported by `health` and stamped into every audit record.
pub const CONTRACT_VERSION: &str = "0.1.0";
/// Contract tail of the canonical id `z:<tid>:expense-guard`.
pub const CONTRACT_NAME: &str = "expense-guard";
/// Longest accepted opaque identifier (expense id, employee ref, category, vendor).
const MAX_FIELD_LEN: usize = 128;

// The generated bindings follow the vendor reference layout: the world's
// exports land under `exports::z::expense_guard::contracts`, the two imported
// packages under `host::interfaces` and `host::tenant`.
wit_bindgen::generate!({
    world: "expense-guard",
    path: "wit",
    additional_derives: [serde::Deserialize, serde::Serialize],
    generate_all,
});

// ---------------------------------------------------------------------------
// Exported envelopes (JSON shapes frozen by docs/INTERFACE.md)
// ---------------------------------------------------------------------------

/// `health` response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HealthResponse {
    pub ok: bool,
    pub contract: String,
    pub version: String,
    pub tenant_did: String,
    pub cluster_ts: u64,
}

/// `set-policy` response. `categories` is the *count* of category rules, as
/// frozen by INTERFACE.md (note: `get-policy`/`Policy` uses the same key for the
/// rule map itself).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SetPolicyResponse {
    pub stored: bool,
    pub base_currency: String,
    pub categories: usize,
}

/// `request-approval` response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApprovalResponse {
    pub expense_id: String,
    pub status: String,
    pub http_code: u16,
    pub approval_ref: String,
    pub ledger_seq: u64,
}

/// `get-audit` response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AuditResponse {
    pub count: usize,
    pub entries: Vec<ledger::AuditRecord>,
}

/// `check-expense` input payload.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClaimInput {
    pub expense_id: String,
    /// Opaque pseudonymous handle (`emp_7f3a`). Never a name or an e-mail.
    pub employee_ref: String,
    /// Claimed amount, in `currency`.
    pub amount: f64,
    pub currency: String,
    pub category: String,
    pub vendor: String,
    /// Informational only: the ledger stores the evaluation time, not this date.
    #[serde(default)]
    pub incurred_on: Option<String>,
}

/// `check-expense` response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CheckExpenseResponse {
    pub expense_id: String,
    pub verdict: String,
    pub base_currency: String,
    /// `null` when FX was unavailable; `rule_hits` then contains `fx_unavailable`.
    pub base_amount: Option<f64>,
    pub fx_rate: Option<f64>,
    pub fx_source: Option<String>,
    pub rule_hits: Vec<String>,
    pub ledger_seq: u64,
    pub evaluated_at: u64,
}

// ---------------------------------------------------------------------------
// Dispatch
// ---------------------------------------------------------------------------

/// Unwrap the `generic-input` envelope and hand the JSON payload to `handler`.
fn dispatch(
    request: &exports::z::expense_guard::contracts::GenericInput,
    handler: impl FnOnce(&[u8]) -> Result<Vec<u8>, String>,
) -> Result<Vec<u8>, String> {
    let input = request
        .input
        .as_deref()
        .ok_or_else(|| errors::prefixed(errors::INPUT, "generic-input.input is missing"))?;
    handler(input)
}

/// Decode a JSON payload.
fn parse_input<T: DeserializeOwned>(input: &[u8]) -> Result<T, String> {
    serde_json::from_slice(input)
        .map_err(|e| errors::prefixed(errors::INPUT, &format!("invalid JSON input: {e}")))
}

/// Decode a JSON payload that is optional: a blank payload decodes to the type's
/// default. Only `get-audit` (whose filter fields are all optional) uses this.
fn parse_input_or_default<T: DeserializeOwned + Default>(input: &[u8]) -> Result<T, String> {
    if input.iter().all(u8::is_ascii_whitespace) {
        return Ok(T::default());
    }
    parse_input(input)
}

/// Encode a response. The only realistic failure is a non-finite float that got
/// past validation, so it is reported as an `input:` problem.
fn to_bytes<T: Serialize>(value: &T) -> Result<Vec<u8>, String> {
    serde_json::to_vec(value)
        .map_err(|e| errors::prefixed(errors::INPUT, &format!("response encoding failed: {e}")))
}

fn require_field(name: &str, value: &str, max_len: usize) -> Result<(), String> {
    if value.is_empty() {
        return Err(errors::prefixed(
            errors::INPUT,
            &format!("{name} is required"),
        ));
    }
    if value.len() > max_len {
        return Err(errors::prefixed(
            errors::INPUT,
            &format!("{name} is too long (max {max_len})"),
        ));
    }
    Ok(())
}

/// Normalise then validate a claim: trim text fields, upper-case the currency.
fn normalize_claim(mut claim: ClaimInput) -> Result<ClaimInput, String> {
    claim.expense_id = claim.expense_id.trim().to_string();
    claim.employee_ref = claim.employee_ref.trim().to_string();
    claim.category = claim.category.trim().to_string();
    claim.vendor = claim.vendor.trim().to_string();
    claim.currency = claim.currency.trim().to_uppercase();

    require_field("expense_id", &claim.expense_id, MAX_FIELD_LEN)?;
    require_field("employee_ref", &claim.employee_ref, MAX_FIELD_LEN)?;
    require_field("category", &claim.category, MAX_FIELD_LEN)?;
    require_field("vendor", &claim.vendor, MAX_FIELD_LEN)?;
    if claim.employee_ref.contains('@') {
        return Err(errors::prefixed(
            errors::INPUT,
            "employee_ref must be an opaque pseudonymous handle, not an e-mail address",
        ));
    }
    if !claim.amount.is_finite() || claim.amount < 0.0 {
        return Err(errors::prefixed(
            errors::INPUT,
            "amount must be a finite, non-negative number",
        ));
    }
    if !policy::is_currency_code(&claim.currency) {
        return Err(errors::prefixed(
            errors::INPUT,
            "currency must be a 3-letter ISO code",
        ));
    }
    Ok(claim)
}

// ---------------------------------------------------------------------------
// Entry points (host-agnostic, natively unit-tested)
// ---------------------------------------------------------------------------

/// `health` — liveness, contract identity and the tenant's cluster view.
///
/// Touches no KV map and no egress endpoint. The payload is ignored.
pub fn health(host: &mut impl Host, _input: &[u8]) -> Result<Vec<u8>, String> {
    let namespace = maps::Namespace::new(&host.tenant_did());
    to_bytes(&HealthResponse {
        ok: true,
        contract: CONTRACT_NAME.to_string(),
        version: CONTRACT_VERSION.to_string(),
        tenant_did: namespace.tenant_hex().to_string(),
        cluster_ts: host.cluster_ts(),
    })
}

/// `set-policy` — validate and store the policy document in `z:<tid>:policy`.
pub fn set_policy(host: &mut impl Host, input: &[u8]) -> Result<Vec<u8>, String> {
    let policy = policy::parse_policy(input)?;
    let namespace = maps::Namespace::new(&host.tenant_did());
    let encoded = to_bytes(&policy)?;
    host.kv_put(&namespace.policy(), policy::POLICY_KEY, &encoded)
        .map_err(|e| errors::prefixed(errors::KV, &format!("policy write: {e}")))?;
    host.log_info(&format!(
        "set-policy stored {} category rules",
        policy.categories.len()
    ));
    to_bytes(&SetPolicyResponse {
        stored: true,
        base_currency: policy.base_currency.clone(),
        categories: policy.categories.len(),
    })
}

/// `get-policy` — the stored policy, or the built-in default when unset.
pub fn get_policy(host: &mut impl Host, _input: &[u8]) -> Result<Vec<u8>, String> {
    let namespace = maps::Namespace::new(&host.tenant_did());
    let policy = policy::load(host, &namespace.policy())?;
    to_bytes(&policy)
}

/// `check-expense` — convert, evaluate, append and index one claim.
pub fn check_expense(host: &mut impl Host, input: &[u8]) -> Result<Vec<u8>, String> {
    let claim = normalize_claim(parse_input::<ClaimInput>(input)?)?;
    let namespace = maps::Namespace::new(&host.tenant_did());
    let policy = policy::load(host, &namespace.policy())?;
    let now = host.cluster_ts();

    let rate = fx::lookup(
        host,
        &namespace.fx(),
        &claim.currency,
        &policy.base_currency,
        policy.fx_cache_ttl_hours,
    );
    let base_amount = rate.map(|rate| policy::round2(claim.amount * rate.rate));

    let candidate = ledger::DuplicateCandidate {
        employee_ref: claim.employee_ref.clone(),
        amount: claim.amount,
        currency: claim.currency.clone(),
        vendor: claim.vendor.clone(),
    };
    let duplicate = ledger::find_duplicate(
        host,
        &namespace.audit(),
        &candidate,
        policy.duplicate_window_days,
        now,
    )?;

    let evaluation = policy::evaluate(&policy, &claim.category, base_amount, duplicate);
    let rule_hits: Vec<String> = evaluation
        .rule_hits
        .iter()
        .map(|rule| (*rule).to_string())
        .collect();

    let mut record = ledger::AuditRecord {
        seq: 0,
        expense_id: claim.expense_id.clone(),
        employee_ref: claim.employee_ref.clone(),
        verdict: evaluation.verdict.as_str().to_string(),
        base_amount,
        category: claim.category.clone(),
        vendor: claim.vendor.clone(),
        rule_hits: rule_hits.clone(),
        recorded_at: now,
        contract_version: CONTRACT_VERSION.to_string(),
    };
    let ledger_seq = ledger::append(host, &namespace.audit(), &mut record)?;
    ledger::write_pointer(
        host,
        &namespace.audit(),
        &claim.expense_id,
        &ledger::ExpensePointer {
            seq: ledger_seq,
            recorded_at: now,
            employee_ref: claim.employee_ref.clone(),
            amount: claim.amount,
            currency: claim.currency.clone(),
            vendor: claim.vendor.clone(),
        },
    )?;

    host.log_info(&format!(
        "check-expense {} verdict={} seq={ledger_seq}",
        claim.expense_id,
        evaluation.verdict.as_str()
    ));

    to_bytes(&CheckExpenseResponse {
        expense_id: claim.expense_id,
        verdict: evaluation.verdict.as_str().to_string(),
        base_currency: policy.base_currency,
        base_amount,
        fx_rate: rate.map(|rate| rate.rate),
        fx_source: rate.map(|rate| rate.source.to_string()),
        rule_hits,
        ledger_seq,
        evaluated_at: now,
    })
}

/// `request-approval` — POST a PII-safe approval request and record it.
pub fn request_approval(host: &mut impl Host, input: &[u8]) -> Result<Vec<u8>, String> {
    let request: approval::ApprovalInput = parse_input(input)?;
    approval::validate_input(&request)?;

    let namespace = maps::Namespace::new(&host.tenant_did());
    let audit_map = namespace.audit();
    let now = host.cluster_ts();

    // The frozen audit record shape carries no claim fields, so they are
    // inherited from the expense's latest check-expense record.
    let pointer = ledger::pointer_for(host, &audit_map, &request.expense_id)?;
    let previous = match &pointer {
        Some(pointer) => ledger::read_record(host, &audit_map, pointer.seq)?,
        None => None,
    };

    let url = approval::webhook_url(host, &namespace.secrets())?;
    let ledger_seq = ledger::next_seq(host, &audit_map)?;
    let approval_ref = approval::approval_ref(&request.expense_id, ledger_seq, now);
    let outcome = approval::send(host, &url, &request, previous.as_ref(), &approval_ref)?;

    let mut record = approval::audit_record(&request, previous.as_ref(), &outcome, now);
    let ledger_seq = ledger::append_at(host, &audit_map, ledger_seq, &mut record)?;
    if let Some(pointer) = &pointer {
        ledger::refresh_pointer(
            host,
            &audit_map,
            &request.expense_id,
            pointer,
            ledger_seq,
            now,
        )?;
    }

    host.log_info(&format!(
        "request-approval {} status={} seq={ledger_seq}",
        request.expense_id,
        approval::STATUS_SENT
    ));

    to_bytes(&ApprovalResponse {
        expense_id: request.expense_id,
        status: approval::STATUS_SENT.to_string(),
        http_code: outcome.http_code,
        approval_ref: outcome.approval_ref,
        ledger_seq,
    })
}

/// `get-audit` — page the ledger, newest first.
pub fn get_audit(host: &mut impl Host, input: &[u8]) -> Result<Vec<u8>, String> {
    let filter: ledger::AuditFilter = parse_input_or_default(input)?;
    let namespace = maps::Namespace::new(&host.tenant_did());
    let entries = ledger::read_newest(host, &namespace.audit(), &filter)?;
    to_bytes(&AuditResponse {
        count: entries.len(),
        entries,
    })
}

// ---------------------------------------------------------------------------
// WASM component glue
// ---------------------------------------------------------------------------

/// The exported contract. Only the glue is `wasm32`-specific; every line of
/// logic above is compiled and tested natively.
#[derive(Debug)]
struct Component;

#[cfg(target_arch = "wasm32")]
impl exports::z::expense_guard::contracts::Guest for Component {
    fn health(
        request: exports::z::expense_guard::contracts::GenericInput,
    ) -> Result<Vec<u8>, String> {
        let mut host = capabilities::WasmHost;
        dispatch(&request, |input| health(&mut host, input))
    }

    fn set_policy(
        request: exports::z::expense_guard::contracts::GenericInput,
    ) -> Result<Vec<u8>, String> {
        let mut host = capabilities::WasmHost;
        dispatch(&request, |input| set_policy(&mut host, input))
    }

    fn get_policy(
        request: exports::z::expense_guard::contracts::GenericInput,
    ) -> Result<Vec<u8>, String> {
        let mut host = capabilities::WasmHost;
        dispatch(&request, |input| get_policy(&mut host, input))
    }

    fn check_expense(
        request: exports::z::expense_guard::contracts::GenericInput,
    ) -> Result<Vec<u8>, String> {
        let mut host = capabilities::WasmHost;
        dispatch(&request, |input| check_expense(&mut host, input))
    }

    fn request_approval(
        request: exports::z::expense_guard::contracts::GenericInput,
    ) -> Result<Vec<u8>, String> {
        let mut host = capabilities::WasmHost;
        dispatch(&request, |input| request_approval(&mut host, input))
    }

    fn get_audit(
        request: exports::z::expense_guard::contracts::GenericInput,
    ) -> Result<Vec<u8>, String> {
        let mut host = capabilities::WasmHost;
        dispatch(&request, |input| get_audit(&mut host, input))
    }
}

#[cfg(target_arch = "wasm32")]
export!(Component);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::MockHost;
    use serde_json::{json, Value};

    const WEBHOOK: &str = "https://hooks.example.test/expense-approval";

    fn policy_json() -> Value {
        json!({
            "base_currency": "USD",
            "approval_threshold": 500,
            "default_category_limit": 250,
            "duplicate_window_days": 30,
            "fx_cache_ttl_hours": 6,
            "categories": {
                "travel": { "limit": 1500 },
                "software": { "limit": 300 },
                "entertainment": { "limit": 0, "allowed": false }
            }
        })
    }

    fn claim_json(expense_id: &str, amount: f64) -> Value {
        json!({
            "expense_id": expense_id,
            "employee_ref": "emp_7f3a",
            "amount": amount,
            "currency": "USD",
            "category": "software",
            "vendor": "JetBrains",
            "incurred_on": "2026-09-01"
        })
    }

    /// A claim in a specific category (used to isolate one rule at a time).
    fn claim_in(expense_id: &str, amount: f64, category: &str) -> Value {
        let mut claim = claim_json(expense_id, amount);
        claim["category"] = json!(category);
        claim
    }

    fn host_with_policy() -> MockHost {
        let mut host = MockHost::new();
        host.seed_policy(policy_json());
        host
    }

    fn encode(value: &Value) -> Vec<u8> {
        serde_json::to_vec(value).unwrap()
    }

    fn call(
        handler: fn(&mut MockHost, &[u8]) -> Result<Vec<u8>, String>,
        host: &mut MockHost,
        value: &Value,
    ) -> Value {
        let bytes = handler(host, &encode(value)).unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    fn call_raw(
        handler: fn(&mut MockHost, &[u8]) -> Result<Vec<u8>, String>,
        host: &mut MockHost,
        payload: &[u8],
    ) -> Value {
        let bytes = handler(host, payload).unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    fn check(host: &mut MockHost, value: &Value) -> Value {
        call(check_expense, host, value)
    }

    // ----- health / dispatch -------------------------------------------------

    #[test]
    fn health_reports_the_contract_identity() {
        let mut host = MockHost::new();
        let response = call(health, &mut host, &json!({}));

        assert_eq!(response["ok"], true);
        assert_eq!(response["contract"], "expense-guard");
        assert_eq!(response["version"], CONTRACT_VERSION);
        assert_eq!(response["tenant_did"], hex::encode(&host.tid));
        assert_eq!(response["cluster_ts"], host.ts);
    }

    #[test]
    fn dispatch_rejects_a_missing_input_envelope() {
        let request = exports::z::expense_guard::contracts::GenericInput {
            input: None,
            user_profile: None,
            context: None,
        };
        let error = dispatch(&request, |_| Ok(Vec::new())).unwrap_err();
        assert!(error.starts_with(errors::INPUT), "{error}");
    }

    #[test]
    fn dispatch_passes_the_payload_through() {
        let request = exports::z::expense_guard::contracts::GenericInput {
            input: Some(b"{\"a\":1}".to_vec()),
            user_profile: None,
            context: None,
        };
        assert_eq!(
            dispatch(&request, |input| Ok(input.to_vec())).unwrap(),
            b"{\"a\":1}"
        );
    }

    // ----- set-policy / get-policy ------------------------------------------

    #[test]
    fn set_policy_stores_the_document_and_reports_the_rule_count() {
        let mut host = MockHost::new();
        let response = call(set_policy, &mut host, &policy_json());

        assert_eq!(response["stored"], true);
        assert_eq!(response["base_currency"], "USD");
        assert_eq!(response["categories"], 3, "count, not the rule map");

        let namespace = host.namespace();
        let stored = host.json_at(&namespace.policy(), b"current").unwrap();
        assert_eq!(stored["approval_threshold"], 500.0);
        assert_eq!(stored["categories"]["travel"]["limit"], 1500.0);
    }

    #[test]
    fn get_policy_returns_the_built_in_default_when_nothing_is_stored() {
        let mut host = MockHost::new();
        let response = call(get_policy, &mut host, &json!({}));

        assert_eq!(response["base_currency"], policy::DEFAULT_BASE_CURRENCY);
        assert_eq!(
            response["approval_threshold"],
            policy::DEFAULT_APPROVAL_THRESHOLD
        );
        assert_eq!(
            response["default_category_limit"],
            policy::DEFAULT_CATEGORY_LIMIT
        );
        assert_eq!(
            response["duplicate_window_days"],
            policy::DEFAULT_DUPLICATE_WINDOW_DAYS
        );
        assert_eq!(
            response["fx_cache_ttl_hours"],
            policy::DEFAULT_FX_CACHE_TTL_HOURS
        );
        assert_eq!(response["categories"], json!({}));
    }

    #[test]
    fn set_policy_then_get_policy_round_trips() {
        let mut host = MockHost::new();
        call(set_policy, &mut host, &policy_json());
        let response = call(get_policy, &mut host, &json!({}));
        assert_eq!(response["categories"]["entertainment"]["allowed"], false);
        assert_eq!(response["categories"]["software"]["limit"], 300.0);
    }

    #[test]
    fn set_policy_rejects_invalid_policies_with_the_policy_prefix() {
        let mut host = MockHost::new();

        let error = set_policy(&mut host, b"not json").unwrap_err();
        assert!(error.starts_with(errors::POLICY), "{error}");

        let mut policy = policy_json();
        policy["base_currency"] = json!("DOLLARS");
        let error = set_policy(&mut host, &encode(&policy)).unwrap_err();
        assert!(error.starts_with(errors::POLICY), "{error}");

        let mut policy = policy_json();
        policy["approval_threshold"] = json!(-1);
        let error = set_policy(&mut host, &encode(&policy)).unwrap_err();
        assert!(error.starts_with(errors::POLICY), "{error}");
    }

    #[test]
    fn stored_policy_that_no_longer_parses_is_reported_with_the_policy_prefix() {
        let mut host = MockHost::new();
        host.seed_raw_policy(b"{ broken");
        let error = check_expense(&mut host, &encode(&claim_json("EXP-1", 10.0))).unwrap_err();
        assert!(error.starts_with(errors::POLICY), "{error}");
    }

    #[test]
    fn policy_map_read_failures_are_reported_with_the_kv_prefix() {
        let mut host = MockHost::new();
        let namespace = host.namespace();
        host.kv_get_failures
            .insert(namespace.policy(), "map missing".to_string());
        let error = get_policy(&mut host, b"{}").unwrap_err();
        assert!(error.starts_with(errors::KV), "{error}");
    }

    // ----- check-expense ----------------------------------------------------

    #[test]
    fn check_expense_marks_a_compliant_claim_and_appends_it() {
        let mut host = host_with_policy();
        let response = check(&mut host, &claim_json("EXP-1001", 120.0));

        assert_eq!(response["verdict"], "compliant");
        assert_eq!(response["base_currency"], "USD");
        assert_eq!(response["base_amount"], 120.0);
        assert_eq!(response["fx_rate"], 1.0);
        assert_eq!(response["fx_source"], "identity");
        assert_eq!(response["rule_hits"], json!([]));
        assert_eq!(response["ledger_seq"], 1);
        assert_eq!(response["evaluated_at"], host.ts);

        let namespace = host.namespace();
        let record = host.json_at(&namespace.audit(), b"seq:000001").unwrap();
        assert_eq!(record["seq"], 1);
        assert_eq!(record["expense_id"], "EXP-1001");
        assert_eq!(record["verdict"], "compliant");
        assert_eq!(record["contract_version"], CONTRACT_VERSION);
        assert_eq!(record["recorded_at"], host.ts);
        let pointer = host.json_at(&namespace.audit(), b"exp:EXP-1001").unwrap();
        assert_eq!(pointer["seq"], 1);
        assert_eq!(pointer["currency"], "USD");
    }

    #[test]
    fn check_expense_raises_the_approval_threshold_rule() {
        let mut host = host_with_policy();
        // travel has a 1500 limit, so only the approval threshold trips.
        let response = check(&mut host, &claim_in("EXP-1002", 600.0, "travel"));
        assert_eq!(response["verdict"], "needs_approval");
        assert_eq!(response["rule_hits"], json!(["over_approval_threshold"]));
    }

    #[test]
    fn check_expense_raises_the_category_limit_rule() {
        let mut host = host_with_policy();
        let response = check(&mut host, &claim_json("EXP-1003", 320.0));
        assert_eq!(response["verdict"], "needs_approval");
        assert_eq!(response["rule_hits"], json!(["over_category_limit"]));
    }

    #[test]
    fn check_expense_rejects_a_disallowed_category() {
        let mut host = host_with_policy();
        let mut claim = claim_json("EXP-1004", 480.0);
        claim["category"] = json!("entertainment");

        let response = check(&mut host, &claim);
        assert_eq!(response["verdict"], "rejected");
        assert_eq!(response["rule_hits"], json!(["disallowed_category"]));
    }

    #[test]
    fn check_expense_falls_back_to_the_default_limit_for_unknown_categories() {
        let mut host = host_with_policy();
        let mut claim = claim_json("EXP-1005", 260.0);
        claim["category"] = json!("hardware");

        let response = check(&mut host, &claim);
        assert_eq!(response["verdict"], "needs_approval");
        assert_eq!(
            response["rule_hits"],
            json!(["over_category_limit", "unknown_category"])
        );
    }

    #[test]
    fn check_expense_converts_with_the_live_rate_and_caches_it() {
        let mut host = host_with_policy();
        host.queue_http_json(200, json!({"result": "success", "rates": {"USD": 1.0888}}));

        let mut claim = claim_in("EXP-1006", 812.40, "travel");
        claim["currency"] = json!("EUR");
        let response = check(&mut host, &claim);

        assert_eq!(response["fx_source"], "open.er-api.com");
        assert_eq!(response["fx_rate"], 1.0888);
        assert_eq!(response["base_amount"], 884.54, "812.40 EUR x 1.0888, 2dp");
        assert_eq!(response["verdict"], "needs_approval");
        assert_eq!(response["rule_hits"], json!(["over_approval_threshold"]));

        assert_eq!(host.http_get_calls, vec![fx::lookup_url("EUR")]);
        let cached = host.json_at(&host.namespace().fx(), b"EUR:USD").unwrap();
        assert_eq!(cached["rate"], 1.0888);
        assert_eq!(cached["fetched_at"], host.ts);
    }

    #[test]
    fn check_expense_reuses_a_fresh_cached_rate_without_egress() {
        let mut host = host_with_policy();
        let namespace = host.namespace();
        host.seed(
            &namespace.fx(),
            b"EUR:USD",
            &encode(&json!({"rate": 1.25, "fetched_at": host.ts - 3_600})),
        );

        let mut claim = claim_json("EXP-1007", 100.0);
        claim["currency"] = json!("eur");
        let response = check(&mut host, &claim);

        assert_eq!(response["fx_source"], "cache");
        assert_eq!(response["base_amount"], 125.0);
        assert!(
            host.http_get_calls.is_empty(),
            "no egress for a fresh entry"
        );
    }

    #[test]
    fn check_expense_raises_fx_unavailable_when_fetching_fails_without_a_cache() {
        let mut host = host_with_policy();
        host.queue_http_error("egress denied");

        let mut claim = claim_json("EXP-1008", 900.0);
        claim["currency"] = json!("EUR");
        let response = check(&mut host, &claim);

        assert_eq!(response["verdict"], "needs_approval");
        assert_eq!(response["rule_hits"], json!(["fx_unavailable"]));
        assert_eq!(response["base_amount"], Value::Null);
        assert_eq!(response["fx_rate"], Value::Null);
        assert_eq!(response["fx_source"], Value::Null);
        assert!(host.logs.iter().any(|line| line.contains(errors::FX)));
    }

    #[test]
    fn check_expense_honours_the_cache_ttl() {
        let mut host = host_with_policy();
        let namespace = host.namespace();
        let seven_hours = 7 * 3_600;
        host.seed(
            &namespace.fx(),
            b"EUR:USD",
            &encode(&json!({"rate": 1.25, "fetched_at": host.ts - seven_hours})),
        );
        host.queue_http_json(200, json!({"result": "success", "rates": {"USD": 1.5}}));

        let mut claim = claim_json("EXP-1009", 100.0);
        claim["currency"] = json!("EUR");
        let response = check(&mut host, &claim);

        assert_eq!(response["fx_source"], "open.er-api.com");
        assert_eq!(response["base_amount"], 150.0);
        assert_eq!(host.http_get_calls.len(), 1);
    }

    #[test]
    fn check_expense_survives_a_corrupt_cache_entry() {
        let mut host = host_with_policy();
        let namespace = host.namespace();
        host.seed(&namespace.fx(), b"EUR:USD", b"{ not json");
        host.queue_http_json(200, json!({"result": "success", "rates": {"USD": 2.0}}));

        let mut claim = claim_json("EXP-1010", 100.0);
        claim["currency"] = json!("EUR");
        let response = check(&mut host, &claim);
        assert_eq!(response["base_amount"], 200.0);
    }

    #[test]
    fn check_expense_flags_a_duplicate_and_keeps_appending() {
        let mut host = host_with_policy();
        let first = check(&mut host, &claim_in("EXP-1011", 600.0, "travel"));
        assert_eq!(first["rule_hits"], json!(["over_approval_threshold"]));

        let second = check(&mut host, &claim_in("EXP-1012", 600.0, "travel"));
        assert_eq!(
            second["rule_hits"],
            json!(["over_approval_threshold", "possible_duplicate"])
        );
        assert_eq!(second["verdict"], "needs_approval");
        assert_eq!(second["ledger_seq"], 2, "the duplicate is still recorded");
    }

    #[test]
    fn a_claim_with_a_different_amount_is_not_a_duplicate() {
        let mut host = host_with_policy();
        check(&mut host, &claim_in("EXP-1013", 600.0, "travel"));
        let third = check(&mut host, &claim_in("EXP-1014", 601.0, "travel"));
        assert!(
            !third["rule_hits"]
                .as_array()
                .unwrap()
                .contains(&json!("possible_duplicate")),
            "different amount"
        );
    }

    #[test]
    fn check_expense_ignores_duplicates_outside_the_window() {
        let mut host = host_with_policy();
        check(&mut host, &claim_in("EXP-1015", 600.0, "travel"));

        // Move the cluster clock 31 days forward: the policy window is 30 days.
        host.ts += 31 * 86_400;
        let later = check(&mut host, &claim_in("EXP-1016", 600.0, "travel"));
        assert_eq!(later["rule_hits"], json!(["over_approval_threshold"]));
    }

    #[test]
    fn check_expense_validates_its_input() {
        let mut host = host_with_policy();

        let error = check_expense(&mut host, b"{ nope").unwrap_err();
        assert!(error.starts_with(errors::INPUT), "{error}");

        let mut claim = claim_json("EXP-1017", 10.0);
        claim["employee_ref"] = json!("ada@example.com");
        let error = check_expense(&mut host, &encode(&claim)).unwrap_err();
        assert!(error.starts_with(errors::INPUT), "{error}");
        assert!(error.contains("pseudonymous"), "{error}");

        let mut claim = claim_json("EXP-1018", -5.0);
        claim["expense_id"] = json!("EXP-1018");
        let error = check_expense(&mut host, &encode(&claim)).unwrap_err();
        assert!(error.starts_with(errors::INPUT), "{error}");

        let mut claim = claim_json("EXP-1019", 10.0);
        claim["currency"] = json!("EUROS");
        let error = check_expense(&mut host, &encode(&claim)).unwrap_err();
        assert!(error.starts_with(errors::INPUT), "{error}");

        let mut claim = claim_json("", 10.0);
        claim["expense_id"] = json!("");
        let error = check_expense(&mut host, &encode(&claim)).unwrap_err();
        assert!(error.starts_with(errors::INPUT), "{error}");
    }

    #[test]
    fn check_expense_reports_ledger_write_failures_with_the_kv_prefix() {
        let mut host = host_with_policy();
        let namespace = host.namespace();
        host.kv_put_failures
            .insert(namespace.audit(), "write conflict".to_string());
        let error = check_expense(&mut host, &encode(&claim_json("EXP-1020", 10.0))).unwrap_err();
        assert!(error.starts_with(errors::KV), "{error}");
    }

    #[test]
    fn check_expense_response_key_order_is_frozen() {
        let mut host = host_with_policy();
        let bytes = check_expense(&mut host, &encode(&claim_json("EXP-1021", 10.0))).unwrap();
        let text = String::from_utf8(bytes).unwrap();
        let expected = [
            "expense_id",
            "verdict",
            "base_currency",
            "base_amount",
            "fx_rate",
            "fx_source",
            "rule_hits",
            "ledger_seq",
            "evaluated_at",
        ];
        let mut cursor = 0;
        for key in expected {
            let needle = format!("\"{key}\":");
            let position = text[cursor..]
                .find(&needle)
                .unwrap_or_else(|| panic!("{needle} missing or out of order in {text}"));
            cursor += position + needle.len();
        }
    }

    // ----- get-audit --------------------------------------------------------

    #[test]
    fn get_audit_defaults_to_an_empty_filter_and_pages_newest_first() {
        let mut host = host_with_policy();
        check(&mut host, &claim_json("EXP-2001", 10.0));
        check(&mut host, &claim_json("EXP-2002", 20.0));
        check(&mut host, &claim_json("EXP-2003", 30.0));

        let response = call(get_audit, &mut host, &json!({}));
        assert_eq!(response["count"], 3);
        let entries = response["entries"].as_array().unwrap();
        assert_eq!(entries[0]["expense_id"], "EXP-2003");
        assert_eq!(entries[1]["expense_id"], "EXP-2002");
        assert_eq!(entries[2]["expense_id"], "EXP-2001");
        assert_eq!(entries[0]["seq"], 3);
    }

    #[test]
    fn get_audit_honours_limit_and_filters() {
        let mut host = host_with_policy();
        check(&mut host, &claim_json("EXP-3001", 10.0));
        check(&mut host, &claim_json("EXP-3002", 20.0));
        let response = call(get_audit, &mut host, &json!({ "limit": 1 }));
        assert_eq!(response["count"], 1);
        assert_eq!(response["entries"][0]["expense_id"], "EXP-3002");

        let response = call(get_audit, &mut host, &json!({ "expense_id": "EXP-3001" }));
        assert_eq!(response["count"], 1);
        assert_eq!(response["entries"][0]["expense_id"], "EXP-3001");

        let response = call(get_audit, &mut host, &json!({ "employee_ref": "nobody" }));
        assert_eq!(response["count"], 0);
        assert_eq!(response["entries"], json!([]));

        let response = call_raw(get_audit, &mut host, b"");
        assert_eq!(response["count"], 2, "blank payload means no filter");
    }

    #[test]
    fn get_audit_rejects_bad_filters_with_the_input_prefix() {
        let mut host = MockHost::new();
        let error = get_audit(&mut host, b"{\"limit\": \"many\"}").unwrap_err();
        assert!(error.starts_with(errors::INPUT), "{error}");
    }

    // ----- request-approval -------------------------------------------------

    #[test]
    fn request_approval_posts_the_webhook_and_records_it() {
        let mut host = host_with_policy();
        host.seed_webhook_secret(WEBHOOK);
        check(&mut host, &claim_in("EXP-4001", 600.0, "travel"));
        host.queue_webhook_ok(202, b"{\"ok\":true}");

        let response = call(
            request_approval,
            &mut host,
            &json!({ "expense_id": "EXP-4001", "use_profile": true, "note": "client dinner" }),
        );

        assert_eq!(response["status"], "sent");
        assert_eq!(response["http_code"], 202);
        assert_eq!(response["ledger_seq"], 2);
        assert!(response["approval_ref"]
            .as_str()
            .unwrap()
            .starts_with("appr-"));

        assert_eq!(host.webhook_calls.len(), 1);
        assert_eq!(host.webhook_calls[0].0, WEBHOOK);
        let body = String::from_utf8(host.webhook_calls[0].1.clone()).unwrap();
        assert!(body.contains("{{profile.first_name}}"), "{body}");

        let namespace = host.namespace();
        let record = host.json_at(&namespace.audit(), b"seq:000002").unwrap();
        assert_eq!(record["expense_id"], "EXP-4001");
        assert_eq!(record["verdict"], "needs_approval");
        assert_eq!(record["employee_ref"], "emp_7f3a");
        assert_eq!(record["base_amount"], 600.0);
        assert_eq!(record["category"], "travel");
        assert_eq!(record["vendor"], "JetBrains");
        assert_eq!(record["rule_hits"], json!([]));

        let pointer = host.json_at(&namespace.audit(), b"exp:EXP-4001").unwrap();
        assert_eq!(pointer["seq"], 2, "the index follows the newest record");
        assert_eq!(pointer["amount"], 600.0, "fingerprint preserved");
    }

    #[test]
    fn request_approval_records_the_placeholder_fallback() {
        let mut host = host_with_policy();
        host.seed_webhook_secret(WEBHOOK);
        check(&mut host, &claim_in("EXP-4002", 600.0, "travel"));
        host.queue_webhook_err(errors::WebhookError::PlaceholderUnknown(
            "verified_contacts.email.value".to_string(),
        ));
        host.queue_webhook_ok(200, b"{}");

        let response = call(
            request_approval,
            &mut host,
            &json!({ "expense_id": "EXP-4002", "use_profile": true }),
        );
        assert_eq!(response["status"], "sent");
        assert_eq!(host.webhook_calls.len(), 2);

        let record = host
            .json_at(&host.namespace().audit(), b"seq:000002")
            .unwrap();
        assert_eq!(
            record["rule_hits"],
            json!([approval::RULE_PROFILE_PLACEHOLDER_FALLBACK])
        );
    }

    #[test]
    fn request_approval_without_the_secret_fails_and_writes_nothing() {
        let mut host = host_with_policy();
        let error =
            request_approval(&mut host, &encode(&json!({ "expense_id": "EXP-4003" }))).unwrap_err();

        assert!(error.starts_with(errors::APPROVAL), "{error}");
        assert!(host.webhook_calls.is_empty());
        assert!(host.keys(&host.namespace().audit()).is_empty());
    }

    #[test]
    fn request_approval_reports_webhook_egress_failures() {
        let mut host = host_with_policy();
        host.seed_webhook_secret(WEBHOOK);
        host.queue_webhook_err(errors::WebhookError::EgressDenied(
            "hooks.example.test".to_string(),
        ));

        let error =
            request_approval(&mut host, &encode(&json!({ "expense_id": "EXP-4004" }))).unwrap_err();
        assert!(error.starts_with(errors::APPROVAL), "{error}");
        assert!(host.keys(&host.namespace().audit()).is_empty());
    }

    #[test]
    fn request_approval_validates_its_input() {
        let mut host = host_with_policy();
        host.seed_webhook_secret(WEBHOOK);

        let error = request_approval(&mut host, b"nope").unwrap_err();
        assert!(error.starts_with(errors::INPUT), "{error}");

        let error = request_approval(
            &mut host,
            &encode(&json!({ "expense_id": "EXP-1", "note": "{{profile.first_name}}" })),
        )
        .unwrap_err();
        assert!(error.starts_with(errors::INPUT), "{error}");
    }

    #[test]
    fn contract_version_is_semver() {
        let mut parts = CONTRACT_VERSION.split('.');
        let major = parts.next().unwrap().parse::<u32>().unwrap();
        let minor = parts.next().unwrap().parse::<u32>().unwrap();
        let patch = parts.next().unwrap().parse::<u32>().unwrap();
        assert_eq!(parts.next(), None);
        assert_eq!(CONTRACT_NAME, "expense-guard");
        assert_eq!(major + minor + patch, 1);
    }
}
