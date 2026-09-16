//! The policy document: parsing, defaults and the compliance rule engine.
//!
//! The policy lives as JSON under `z:<tid>:policy` / key `current`
//! (`docs/INTERFACE.md` §"Policy JSON"). Reading rules that the spec fixes:
//!
//! * nothing stored → built-in defaults ([`Policy::default`]);
//! * a missing top-level field → that field's default (forward compatible with
//!   older policy documents written by previous contract versions);
//! * a category missing from `categories` → `default_category_limit` with
//!   `allowed = true`, and the informational `unknown_category` rule hit.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::capabilities::Host;
use crate::errors;

/// `z:<tid>:policy` key that holds the active policy document.
pub const POLICY_KEY: &[u8] = b"current";

/// Category explicitly denied by the policy → `rejected`.
pub const RULE_DISALLOWED_CATEGORY: &str = "disallowed_category";
/// Converted amount above the category limit (`limit > 0`) → `needs_approval`.
pub const RULE_OVER_CATEGORY_LIMIT: &str = "over_category_limit";
/// Converted amount above `approval_threshold` → `needs_approval`.
pub const RULE_OVER_APPROVAL_THRESHOLD: &str = "over_approval_threshold";
/// Same employee/amount/currency/vendor inside `duplicate_window_days` → `needs_approval`.
pub const RULE_POSSIBLE_DUPLICATE: &str = "possible_duplicate";
/// No usable FX rate (live lookup failed, no cache) → `needs_approval`.
pub const RULE_FX_UNAVAILABLE: &str = "fx_unavailable";
/// Category absent from the policy map — informational, does not change the verdict.
pub const RULE_UNKNOWN_CATEGORY: &str = "unknown_category";

/// Built-in default base currency.
pub const DEFAULT_BASE_CURRENCY: &str = "USD";
/// Built-in default approval threshold, in base currency.
pub const DEFAULT_APPROVAL_THRESHOLD: f64 = 500.0;
/// Built-in default per-category limit, in base currency.
pub const DEFAULT_CATEGORY_LIMIT: f64 = 250.0;
/// Built-in default duplicate window.
pub const DEFAULT_DUPLICATE_WINDOW_DAYS: u64 = 30;
/// Built-in default FX cache TTL.
pub const DEFAULT_FX_CACHE_TTL_HOURS: u64 = 6;

fn base_currency_default() -> String {
    DEFAULT_BASE_CURRENCY.to_string()
}

fn approval_threshold_default() -> f64 {
    DEFAULT_APPROVAL_THRESHOLD
}

fn category_limit_default() -> f64 {
    DEFAULT_CATEGORY_LIMIT
}

fn duplicate_window_default() -> u64 {
    DEFAULT_DUPLICATE_WINDOW_DAYS
}

fn fx_cache_ttl_default() -> u64 {
    DEFAULT_FX_CACHE_TTL_HOURS
}

fn allowed_default() -> bool {
    true
}

/// One entry of `policy.categories`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CategoryRule {
    /// Per-transaction limit in the policy base currency. `0` disables the
    /// limit check for this category.
    #[serde(default = "category_limit_default")]
    pub limit: f64,
    /// `false` → every claim in this category is rejected.
    #[serde(default = "allowed_default")]
    pub allowed: bool,
}

impl Default for CategoryRule {
    fn default() -> Self {
        Self {
            limit: DEFAULT_CATEGORY_LIMIT,
            allowed: true,
        }
    }
}

/// The policy document. Field order matches `docs/INTERFACE.md` §"Policy JSON".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Policy {
    #[serde(default = "base_currency_default")]
    pub base_currency: String,
    #[serde(default = "approval_threshold_default")]
    pub approval_threshold: f64,
    #[serde(default = "category_limit_default")]
    pub default_category_limit: f64,
    #[serde(default = "duplicate_window_default")]
    pub duplicate_window_days: u64,
    #[serde(default = "fx_cache_ttl_default")]
    pub fx_cache_ttl_hours: u64,
    #[serde(default)]
    pub categories: BTreeMap<String, CategoryRule>,
}

impl Default for Policy {
    /// The built-in fallback used when `z:<tid>:policy` is empty. No category
    /// table: every category falls back to `default_category_limit` and is
    /// allowed, so an unconfigured tenant is governed by the threshold alone.
    fn default() -> Self {
        Self {
            base_currency: base_currency_default(),
            approval_threshold: DEFAULT_APPROVAL_THRESHOLD,
            default_category_limit: DEFAULT_CATEGORY_LIMIT,
            duplicate_window_days: DEFAULT_DUPLICATE_WINDOW_DAYS,
            fx_cache_ttl_hours: DEFAULT_FX_CACHE_TTL_HOURS,
            categories: BTreeMap::new(),
        }
    }
}

/// `true` for a 3-letter uppercase ISO-4217 code.
pub fn is_currency_code(code: &str) -> bool {
    code.len() == 3 && code.bytes().all(|byte| byte.is_ascii_uppercase())
}

/// Round to whole cents — money is compared and reported at 2 decimals so that
/// what a report shows is what the rule engine compared.
pub fn round2(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}

/// Parse a policy document (from `set-policy` input or from KV).
pub fn parse_policy(bytes: &[u8]) -> Result<Policy, String> {
    let mut policy: Policy = serde_json::from_slice(bytes)
        .map_err(|e| errors::prefixed(errors::POLICY, &format!("invalid policy JSON: {e}")))?;
    // Normalise before validating so "usd" and "USD" behave the same.
    policy.base_currency = policy.base_currency.trim().to_uppercase();
    validate_policy(&policy)?;
    Ok(policy)
}

/// Reject nonsense policies instead of storing them.
pub fn validate_policy(policy: &Policy) -> Result<(), String> {
    if !is_currency_code(&policy.base_currency) {
        return Err(errors::prefixed(
            errors::POLICY,
            "base_currency must be a 3-letter ISO-4217 code",
        ));
    }
    for (name, amount) in [
        ("approval_threshold", policy.approval_threshold),
        ("default_category_limit", policy.default_category_limit),
    ] {
        if !amount.is_finite() || amount < 0.0 {
            return Err(errors::prefixed(
                errors::POLICY,
                &format!("{name} must be a finite, non-negative number"),
            ));
        }
    }
    for (category, rule) in &policy.categories {
        if !rule.limit.is_finite() || rule.limit < 0.0 {
            return Err(errors::prefixed(
                errors::POLICY,
                &format!("category \"{category}\": limit must be a finite, non-negative number"),
            ));
        }
    }
    Ok(())
}

/// Load the tenant policy: stored document when present, built-in default otherwise.
pub fn load(host: &mut impl Host, policy_map: &str) -> Result<Policy, String> {
    match host
        .kv_get(policy_map, POLICY_KEY)
        .map_err(|e| errors::prefixed(errors::KV, &format!("policy read: {e}")))?
    {
        Some(bytes) => parse_policy(&bytes),
        None => Ok(Policy::default()),
    }
}

/// The compliance verdict, ordered `Compliant < NeedsApproval < Rejected`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Verdict {
    /// Every rule passed.
    Compliant,
    /// A human has to approve before payment.
    NeedsApproval,
    /// Hard policy denial.
    Rejected,
}

impl Verdict {
    /// The wire value used in JSON (`docs/INTERFACE.md` §"Verdicts").
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Compliant => "compliant",
            Self::NeedsApproval => "needs_approval",
            Self::Rejected => "rejected",
        }
    }

    /// Worst-rule-wins aggregation: the higher-severity verdict survives.
    pub fn worst(self, other: Self) -> Self {
        if other > self {
            other
        } else {
            self
        }
    }
}

/// Rule evaluation result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Evaluation {
    pub verdict: Verdict,
    /// Rule ids in `docs/INTERFACE.md` §"Rule ids" table order, so assertions
    /// and audit records are byte-stable.
    pub rule_hits: Vec<&'static str>,
}

/// Apply the policy to one claim.
///
/// `base_amount` is the claim amount converted to `policy.base_currency`, or
/// `None` when no FX rate could be resolved — in that case the amount-based
/// rules cannot be evaluated and the `fx_unavailable` rule forces approval.
pub fn evaluate(
    policy: &Policy,
    category: &str,
    base_amount: Option<f64>,
    duplicate: bool,
) -> Evaluation {
    let mut rule_hits: Vec<&'static str> = Vec::new();
    let mut verdict = Verdict::Compliant;

    let (limit, allowed, unknown_category) = match policy.categories.get(category) {
        Some(rule) => (rule.limit, rule.allowed, false),
        None => (policy.default_category_limit, true, true),
    };

    if !allowed {
        rule_hits.push(RULE_DISALLOWED_CATEGORY);
        verdict = verdict.worst(Verdict::Rejected);
    }

    match base_amount {
        Some(amount) => {
            if limit > 0.0 && amount > limit {
                rule_hits.push(RULE_OVER_CATEGORY_LIMIT);
                verdict = verdict.worst(Verdict::NeedsApproval);
            }
            if amount > policy.approval_threshold {
                rule_hits.push(RULE_OVER_APPROVAL_THRESHOLD);
                verdict = verdict.worst(Verdict::NeedsApproval);
            }
        }
        None => {
            rule_hits.push(RULE_FX_UNAVAILABLE);
            verdict = verdict.worst(Verdict::NeedsApproval);
        }
    }

    if duplicate {
        rule_hits.push(RULE_POSSIBLE_DUPLICATE);
        verdict = verdict.worst(Verdict::NeedsApproval);
    }

    if unknown_category {
        rule_hits.push(RULE_UNKNOWN_CATEGORY);
    }

    Evaluation { verdict, rule_hits }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy_from(json: serde_json::Value) -> Policy {
        parse_policy(&serde_json::to_vec(&json).unwrap()).unwrap()
    }

    /// The illustrative policy from `docs/INTERFACE.md` §"Policy JSON".
    fn interface_policy() -> Policy {
        policy_from(serde_json::json!({
            "base_currency": "USD",
            "approval_threshold": 500,
            "default_category_limit": 250,
            "duplicate_window_days": 30,
            "fx_cache_ttl_hours": 6,
            "categories": {
                "travel": { "limit": 1500, "allowed": true },
                "software": { "limit": 400, "allowed": true },
                "entertainment": { "limit": 0, "allowed": false }
            }
        }))
    }

    #[test]
    fn built_in_defaults_match_interface_floor() {
        let policy = Policy::default();
        assert_eq!(policy.base_currency, "USD");
        assert_eq!(policy.approval_threshold, 500.0);
        assert_eq!(policy.default_category_limit, 250.0);
        assert_eq!(policy.duplicate_window_days, 30);
        assert_eq!(policy.fx_cache_ttl_hours, 6);
        assert!(policy.categories.is_empty());
    }

    #[test]
    fn empty_policy_object_falls_back_to_defaults() {
        let policy = policy_from(serde_json::json!({}));
        assert_eq!(policy, Policy::default());
    }

    #[test]
    fn missing_top_level_fields_fall_back_individually() {
        let policy = policy_from(serde_json::json!({ "approval_threshold": 100 }));
        assert_eq!(policy.approval_threshold, 100.0);
        assert_eq!(policy.base_currency, DEFAULT_BASE_CURRENCY);
        assert_eq!(policy.default_category_limit, DEFAULT_CATEGORY_LIMIT);
        assert_eq!(policy.duplicate_window_days, DEFAULT_DUPLICATE_WINDOW_DAYS);
        assert_eq!(policy.fx_cache_ttl_hours, DEFAULT_FX_CACHE_TTL_HOURS);
    }

    #[test]
    fn category_rule_defaults_are_lenient() {
        let policy = policy_from(serde_json::json!({
            "categories": { "food": { "limit": 50 }, "gambling": { "allowed": false } }
        }));
        let food = policy.categories.get("food").unwrap();
        assert!(food.allowed, "an omitted `allowed` defaults to true");
        assert_eq!(food.limit, 50.0);
        let gambling = policy.categories.get("gambling").unwrap();
        assert!(!gambling.allowed);
        assert_eq!(gambling.limit, DEFAULT_CATEGORY_LIMIT);
    }

    #[test]
    fn base_currency_is_normalised_and_validated() {
        assert_eq!(
            policy_from(serde_json::json!({"base_currency": " usd "})).base_currency,
            "USD"
        );
        let error = parse_policy(br#"{"base_currency": "US"}"#).unwrap_err();
        assert!(error.starts_with(errors::POLICY), "{error}");
    }

    #[test]
    fn invalid_limits_and_json_are_rejected_with_policy_prefix() {
        let error = parse_policy(br#"{"approval_threshold": -1}"#).unwrap_err();
        assert!(error.starts_with(errors::POLICY), "{error}");
        let error = parse_policy(br#"{"categories": {"travel": {"limit": -5}}}"#).unwrap_err();
        assert!(error.starts_with(errors::POLICY), "{error}");
        let error = parse_policy(b"not json").unwrap_err();
        assert!(error.starts_with(errors::POLICY), "{error}");
    }

    #[test]
    fn policy_round_trips_through_json_with_the_frozen_key_order() {
        let policy = interface_policy();
        let encoded = serde_json::to_vec(&policy).unwrap();
        let text = String::from_utf8(encoded.clone()).unwrap();

        // Struct serialisation preserves declaration order, which is the order
        // INTERFACE.md documents; a `Value` round-trip would sort the keys, so
        // the order is asserted on the raw bytes.
        let mut cursor = 0;
        for key in [
            "base_currency",
            "approval_threshold",
            "default_category_limit",
            "duplicate_window_days",
            "fx_cache_ttl_hours",
            "categories",
        ] {
            let needle = format!("\"{key}\":");
            let position = text[cursor..]
                .find(&needle)
                .unwrap_or_else(|| panic!("{needle} missing or out of order in {text}"));
            cursor += position + needle.len();
        }

        let reparsed = parse_policy(&encoded).unwrap();
        assert_eq!(reparsed, policy, "the policy survives a JSON round trip");
    }

    #[test]
    fn compliant_when_inside_every_limit() {
        let evaluation = evaluate(&interface_policy(), "travel", Some(400.0), false);
        assert_eq!(evaluation.verdict, Verdict::Compliant);
        assert!(evaluation.rule_hits.is_empty());
    }

    #[test]
    fn category_limit_alone_triggers_approval() {
        let evaluation = evaluate(&interface_policy(), "software", Some(450.0), false);
        assert_eq!(evaluation.verdict, Verdict::NeedsApproval);
        assert_eq!(evaluation.rule_hits, vec![RULE_OVER_CATEGORY_LIMIT]);
    }

    #[test]
    fn approval_threshold_alone_triggers_approval() {
        let evaluation = evaluate(&interface_policy(), "travel", Some(884.52), false);
        assert_eq!(evaluation.verdict, Verdict::NeedsApproval);
        assert_eq!(evaluation.rule_hits, vec![RULE_OVER_APPROVAL_THRESHOLD]);
    }

    #[test]
    fn threshold_and_category_limit_can_fire_together() {
        let evaluation = evaluate(&interface_policy(), "software", Some(600.0), false);
        assert_eq!(evaluation.verdict, Verdict::NeedsApproval);
        assert_eq!(
            evaluation.rule_hits,
            vec![RULE_OVER_CATEGORY_LIMIT, RULE_OVER_APPROVAL_THRESHOLD]
        );
    }

    #[test]
    fn amount_exactly_at_the_threshold_is_not_over_it() {
        let policy = interface_policy();
        assert!(evaluate(&policy, "travel", Some(500.0), false)
            .rule_hits
            .is_empty());
        assert_eq!(
            evaluate(&policy, "travel", Some(500.01), false).rule_hits,
            vec![RULE_OVER_APPROVAL_THRESHOLD]
        );
        // Exactly at the category limit is likewise inside the limit.
        assert_eq!(
            evaluate(&policy, "software", Some(400.0), false).rule_hits,
            Vec::<&str>::new()
        );
    }

    #[test]
    fn a_zero_category_limit_never_triggers_the_limit_rule() {
        let policy = policy_from(serde_json::json!({
            "categories": { "petty": { "limit": 0, "allowed": true } }
        }));
        let evaluation = evaluate(&policy, "petty", Some(10_000.0), false);
        assert!(!evaluation.rule_hits.contains(&RULE_OVER_CATEGORY_LIMIT));
        assert_eq!(evaluation.rule_hits, vec![RULE_OVER_APPROVAL_THRESHOLD]);
    }

    #[test]
    fn disallowed_category_is_rejected() {
        let evaluation = evaluate(&interface_policy(), "entertainment", Some(20.0), false);
        assert_eq!(evaluation.verdict, Verdict::Rejected);
        assert_eq!(evaluation.rule_hits, vec![RULE_DISALLOWED_CATEGORY]);
    }

    #[test]
    fn worst_rule_wins_rejected_beats_needs_approval() {
        let evaluation = evaluate(&interface_policy(), "entertainment", Some(9_999.0), false);
        assert_eq!(evaluation.verdict, Verdict::Rejected);
        assert!(evaluation.rule_hits.contains(&RULE_OVER_APPROVAL_THRESHOLD));
        assert_eq!(
            Verdict::Rejected,
            Verdict::NeedsApproval.worst(Verdict::Rejected)
        );
        assert_eq!(
            Verdict::Compliant,
            Verdict::Compliant.worst(Verdict::Compliant)
        );
        assert_eq!(
            Verdict::NeedsApproval,
            Verdict::Compliant.worst(Verdict::NeedsApproval)
        );
    }

    #[test]
    fn unknown_category_uses_the_default_limit_and_is_informational() {
        let policy = interface_policy();
        // Inside default_category_limit → still compliant, rule is informational.
        let evaluation = evaluate(&policy, "misc", Some(200.0), false);
        assert_eq!(evaluation.verdict, Verdict::Compliant);
        assert_eq!(evaluation.rule_hits, vec![RULE_UNKNOWN_CATEGORY]);

        // Above it → the default limit applies.
        let evaluation = evaluate(&policy, "misc", Some(260.0), false);
        assert_eq!(evaluation.verdict, Verdict::NeedsApproval);
        assert_eq!(
            evaluation.rule_hits,
            vec![RULE_OVER_CATEGORY_LIMIT, RULE_UNKNOWN_CATEGORY]
        );
    }

    #[test]
    fn missing_fx_rate_forces_approval_and_skips_amount_rules() {
        let evaluation = evaluate(&interface_policy(), "travel", None, false);
        assert_eq!(evaluation.verdict, Verdict::NeedsApproval);
        assert_eq!(evaluation.rule_hits, vec![RULE_FX_UNAVAILABLE]);
    }

    #[test]
    fn duplicate_forces_approval_and_keeps_rule_order() {
        let evaluation = evaluate(&interface_policy(), "travel", Some(400.0), true);
        assert_eq!(evaluation.verdict, Verdict::NeedsApproval);
        assert_eq!(evaluation.rule_hits, vec![RULE_POSSIBLE_DUPLICATE]);

        let evaluation = evaluate(&interface_policy(), "software", Some(600.0), true);
        assert_eq!(
            evaluation.rule_hits,
            vec![
                RULE_OVER_CATEGORY_LIMIT,
                RULE_OVER_APPROVAL_THRESHOLD,
                RULE_POSSIBLE_DUPLICATE,
            ]
        );
    }

    #[test]
    fn rule_ids_are_the_frozen_wire_strings() {
        assert_eq!(RULE_DISALLOWED_CATEGORY, "disallowed_category");
        assert_eq!(RULE_OVER_CATEGORY_LIMIT, "over_category_limit");
        assert_eq!(RULE_OVER_APPROVAL_THRESHOLD, "over_approval_threshold");
        assert_eq!(RULE_POSSIBLE_DUPLICATE, "possible_duplicate");
        assert_eq!(RULE_FX_UNAVAILABLE, "fx_unavailable");
        assert_eq!(RULE_UNKNOWN_CATEGORY, "unknown_category");
    }

    #[test]
    fn verdict_wire_values_are_the_frozen_strings() {
        assert_eq!(Verdict::Compliant.as_str(), "compliant");
        assert_eq!(Verdict::NeedsApproval.as_str(), "needs_approval");
        assert_eq!(Verdict::Rejected.as_str(), "rejected");
    }

    #[test]
    fn round2_rounds_to_whole_cents() {
        assert_eq!(round2(884.5200001), 884.52);
        assert_eq!(round2(1.005), 1.0);
        assert_eq!(round2(0.0), 0.0);
        assert_eq!(round2(1234.567), 1234.57);
    }

    #[test]
    fn currency_code_validation() {
        assert!(is_currency_code("USD"));
        assert!(is_currency_code("EUR"));
        assert!(!is_currency_code("usd"));
        assert!(!is_currency_code("US"));
        assert!(!is_currency_code("USDD"));
        assert!(!is_currency_code("US1"));
        assert!(!is_currency_code(""));
    }
}
