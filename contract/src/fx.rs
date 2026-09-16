//! FX rate lookup with a tenant-local cache.
//!
//! * source: `https://open.er-api.com/v6/latest/<FROM>` — keyless and PII-free;
//!   payload is `{"result":"success","rates":{"USD":1.08,...}}`;
//! * cache: `z:<tid>:fx`, key `<FROM>:<TO>`, value
//!   `{"rate":1.08,"fetched_at":<cluster secs>}`, honoured for
//!   `policy.fx_cache_ttl_hours`;
//! * same-currency claims short-circuit to rate `1.0` (`fx_source: "identity"`),
//!   which avoids both a KV round-trip and egress entirely.
//!
//! The FX layer never fails a call. A cache hit that is still fresh is used
//! without any egress; when the live lookup fails and no non-expired cache entry
//! exists, [`lookup`] returns `None` and the caller raises the `fx_unavailable`
//! rule with `base_amount: null` (and `fx_rate`/`fx_source` null).

use serde::{Deserialize, Serialize};

use crate::capabilities::Host;
use crate::errors;

/// `fx_source` on a live lookup.
pub const SOURCE_LIVE: &str = "open.er-api.com";
/// `fx_source` on a cache hit.
pub const SOURCE_CACHE: &str = "cache";
/// `fx_source` when the claim is already in the base currency.
pub const SOURCE_IDENTITY: &str = "identity";
/// Upstream base URL; the currency code is appended at call time.
pub const BASE_URL: &str = "https://open.er-api.com/v6/latest/";
/// `fx_source`/rate for an identity conversion.
const IDENTITY_RATE: f64 = 1.0;
const SECONDS_PER_HOUR: u64 = 3_600;

/// A cached rate as stored in the `fx` map.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct CacheEntry {
    /// Units of `TO` per one unit of `FROM`.
    pub rate: f64,
    /// Cluster timestamp (seconds) when the rate was fetched.
    pub fetched_at: u64,
}

/// A resolved rate plus its provenance (`fx_source` in the response JSON).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rate {
    pub rate: f64,
    pub source: &'static str,
}

/// `fx` map key for a currency pair, formatted `<FROM>:<TO>`.
pub fn cache_key(from: &str, to: &str) -> Vec<u8> {
    format!("{from}:{to}").into_bytes()
}

/// Upstream URL for a base currency.
pub fn lookup_url(from: &str) -> String {
    format!("{BASE_URL}{from}")
}

/// Parse an open.er-api.com payload and return the rate for `to`.
///
/// Pure, so the JSON contract of the upstream is unit-tested without a host.
pub fn parse_rate(body: &[u8], to: &str) -> Result<f64, String> {
    let value: serde_json::Value =
        serde_json::from_slice(body).map_err(|e| format!("invalid JSON: {e}"))?;
    if value.get("result").and_then(|result| result.as_str()) != Some("success") {
        return Err("upstream result is not \"success\"".to_string());
    }
    let rate = value
        .get("rates")
        .and_then(|rates| rates.get(to))
        .and_then(|rate| rate.as_f64())
        .ok_or_else(|| format!("payload carries no rate for {to}"))?;
    if !rate.is_finite() || rate <= 0.0 {
        return Err(format!("rate for {to} is not a positive number"));
    }
    Ok(rate)
}

/// `true` while `fetched_at` is at most `ttl_hours` old.
///
/// `ttl_hours == 0` disables caching; a `fetched_at` in the future (should not
/// happen with cluster time) counts as expired rather than fresh.
pub fn is_fresh(fetched_at: u64, now: u64, ttl_hours: u64) -> bool {
    fetched_at <= now && now - fetched_at < ttl_hours.saturating_mul(SECONDS_PER_HOUR)
}

/// Read a cache entry. A missing, unreadable or malformed entry is a miss —
/// a broken FX cache must never fail a compliance check.
fn read_cache(host: &mut impl Host, map: &str, from: &str, to: &str) -> Option<CacheEntry> {
    match host.kv_get(map, &cache_key(from, to)) {
        Ok(Some(bytes)) => match serde_json::from_slice::<CacheEntry>(&bytes) {
            Ok(entry) => Some(entry),
            Err(_) => {
                host.log_error(&errors::prefixed(
                    errors::FX,
                    "cache entry is not a valid rate; ignoring it",
                ));
                None
            }
        },
        Ok(None) => None,
        Err(error) => {
            host.log_error(&errors::prefixed(
                errors::FX,
                &format!("cache read failed: {error}"),
            ));
            None
        }
    }
}

/// Store a fetched rate. A write failure is logged, never fatal: the call can
/// still answer with the rate it just fetched.
fn write_cache(host: &mut impl Host, map: &str, from: &str, to: &str, entry: CacheEntry) {
    let encoded = match serde_json::to_vec(&entry) {
        Ok(encoded) => encoded,
        Err(error) => {
            host.log_error(&errors::prefixed(
                errors::FX,
                &format!("could not encode cache entry: {error}"),
            ));
            return;
        }
    };
    if let Err(error) = host.kv_put(map, &cache_key(from, to), &encoded) {
        host.log_error(&errors::prefixed(
            errors::FX,
            &format!("cache write failed: {error}"),
        ));
    }
}

/// Fetch a fresh rate and refresh the cache.
fn fetch_live(host: &mut impl Host, map: &str, from: &str, to: &str, now: u64) -> Option<Rate> {
    let url = lookup_url(from);
    let response = match host.http_get(&url) {
        Ok(response) => response,
        Err(error) => {
            host.log_error(&errors::prefixed(
                errors::FX,
                &format!("live lookup failed: {error}"),
            ));
            return None;
        }
    };
    if !response.is_success() {
        host.log_error(&errors::prefixed(
            errors::FX,
            &format!("live lookup returned HTTP {}", response.code),
        ));
        return None;
    }
    match parse_rate(&response.body, to) {
        Ok(rate) => {
            write_cache(
                host,
                map,
                from,
                to,
                CacheEntry {
                    rate,
                    fetched_at: now,
                },
            );
            Some(Rate {
                rate,
                source: SOURCE_LIVE,
            })
        }
        Err(reason) => {
            host.log_error(&errors::prefixed(errors::FX, &reason));
            None
        }
    }
}

/// Resolve `from` → `to`: identity, non-expired cache entry, then live lookup.
///
/// `None` means "no usable rate": the caller raises `fx_unavailable`.
pub fn lookup(
    host: &mut impl Host,
    map: &str,
    from: &str,
    to: &str,
    ttl_hours: u64,
) -> Option<Rate> {
    if from == to {
        return Some(Rate {
            rate: IDENTITY_RATE,
            source: SOURCE_IDENTITY,
        });
    }

    let now = host.cluster_ts();

    // Cache first: a non-expired entry is the documented preference, and it is
    // also the fallback the interface spec asks for when egress fails — a fresh
    // entry means egress is never attempted at all.
    if let Some(entry) = read_cache(host, map, from, to) {
        if is_fresh(entry.fetched_at, now, ttl_hours) {
            return Some(Rate {
                rate: entry.rate,
                source: SOURCE_CACHE,
            });
        }
    }

    if let Some(rate) = fetch_live(host, map, from, to, now) {
        return Some(rate);
    }

    host.log_error(&errors::prefixed(
        errors::FX,
        "no usable rate: live lookup failed and no non-expired cache entry exists",
    ));
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{fx_body, MockHost};

    const TTL: u64 = 6;
    const HOUR: u64 = 3_600;

    #[test]
    fn cache_key_and_url_follow_the_frozen_formats() {
        assert_eq!(cache_key("EUR", "USD"), b"EUR:USD".to_vec());
        assert_eq!(lookup_url("EUR"), "https://open.er-api.com/v6/latest/EUR");
    }

    #[test]
    fn parse_rate_reads_a_successful_payload() {
        assert_eq!(parse_rate(&fx_body(1.0888), "USD").unwrap(), 1.0888);
    }

    #[test]
    fn parse_rate_rejects_bad_payloads() {
        assert!(parse_rate(b"not json", "USD").is_err());
        assert!(parse_rate(br#"{"result":"error","rates":{}}"#, "USD").is_err());
        assert!(parse_rate(br#"{"result":"success","rates":{}}"#, "USD").is_err());
        assert!(parse_rate(br#"{"result":"success","rates":{"USD":-1}}"#, "USD").is_err());
        assert!(parse_rate(br#"{"result":"success","rates":{"USD":"1.1"}}"#, "USD").is_err());
    }

    #[test]
    fn freshness_boundary_is_exclusive_at_the_ttl() {
        let now = 1_700_000_000;
        assert!(is_fresh(now, now, TTL));
        assert!(is_fresh(now - TTL * HOUR + 1, now, TTL));
        assert!(!is_fresh(now - TTL * HOUR, now, TTL));
        assert!(
            !is_fresh(now + 1, now, TTL),
            "future timestamp is not fresh"
        );
        assert!(!is_fresh(now, now, 0), "ttl 0 disables caching");
    }

    #[test]
    fn identity_conversion_needs_neither_kv_nor_egress() {
        let mut host = MockHost::new();
        let rate = lookup(&mut host, "z:t:fx", "USD", "USD", TTL).unwrap();
        assert_eq!(rate.rate, 1.0);
        assert_eq!(rate.source, SOURCE_IDENTITY);
        assert!(host.http_get_calls.is_empty());
        assert!(host.logs.is_empty());
    }

    #[test]
    fn fresh_cache_entry_is_used_without_egress() {
        let mut host = MockHost::new();
        host.seed_json(
            "z:t:fx",
            b"EUR:USD",
            serde_json::json!({ "rate": 1.25, "fetched_at": host.ts - HOUR }),
        );
        let rate = lookup(&mut host, "z:t:fx", "EUR", "USD", TTL).unwrap();
        assert_eq!(rate.rate, 1.25);
        assert_eq!(rate.source, SOURCE_CACHE);
        assert!(host.http_get_calls.is_empty(), "no egress on a cache hit");
    }

    #[test]
    fn stale_cache_entry_triggers_a_live_lookup_and_rewrites_the_cache() {
        let mut host = MockHost::new();
        host.seed_json(
            "z:t:fx",
            b"EUR:USD",
            serde_json::json!({ "rate": 1.25, "fetched_at": host.ts - 7 * HOUR }),
        );
        host.queue_http_ok(&fx_body(1.0888));

        let rate = lookup(&mut host, "z:t:fx", "EUR", "USD", TTL).unwrap();
        assert_eq!(rate.rate, 1.0888);
        assert_eq!(rate.source, SOURCE_LIVE);
        assert_eq!(host.http_get_calls, vec![lookup_url("EUR")]);

        let cached: CacheEntry =
            serde_json::from_slice(&host.value_at("z:t:fx", b"EUR:USD").unwrap()).unwrap();
        assert_eq!(cached.rate, 1.0888);
        assert_eq!(cached.fetched_at, host.ts, "refreshed with cluster time");
    }

    #[test]
    fn egress_failure_without_a_usable_cache_entry_reports_unavailable() {
        let mut host = MockHost::new();
        // No queued response: the mock refuses the egress call.
        assert_eq!(lookup(&mut host, "z:t:fx", "EUR", "USD", TTL), None);
        assert_eq!(host.http_get_calls.len(), 1);
        assert!(host.logs.iter().any(|line| line.starts_with(errors::FX)));
    }

    #[test]
    fn egress_failure_still_serves_a_fresh_cache_entry() {
        let mut host = MockHost::new();
        host.seed_json(
            "z:t:fx",
            b"EUR:USD",
            serde_json::json!({ "rate": 1.25, "fetched_at": host.ts }),
        );
        let rate = lookup(&mut host, "z:t:fx", "EUR", "USD", TTL).unwrap();
        assert_eq!(rate.source, SOURCE_CACHE);
        assert!(
            host.http_get_calls.is_empty(),
            "a fresh entry means egress is never attempted"
        );
    }

    #[test]
    fn egress_failure_with_a_stale_entry_reports_unavailable() {
        let mut host = MockHost::new();
        host.seed_json(
            "z:t:fx",
            b"EUR:USD",
            serde_json::json!({ "rate": 1.25, "fetched_at": host.ts - 30 * HOUR }),
        );
        host.queue_http_error("connection reset");
        assert_eq!(lookup(&mut host, "z:t:fx", "EUR", "USD", TTL), None);
    }

    #[test]
    fn non_2xx_upstream_is_treated_as_unavailable() {
        let mut host = MockHost::new();
        host.queue_http_status(503, b"upstream busy");
        assert_eq!(lookup(&mut host, "z:t:fx", "EUR", "USD", TTL), None);
        assert!(host.logs.iter().any(|line| line.contains("HTTP 503")));
    }

    #[test]
    fn a_broken_cache_map_does_not_fail_the_lookup() {
        let mut host = MockHost::new();
        host.kv_get_failures
            .insert("z:t:fx".to_string(), "map not found".to_string());
        host.kv_put_failures
            .insert("z:t:fx".to_string(), "map not found".to_string());
        host.queue_http_ok(&fx_body(1.5));

        let rate = lookup(&mut host, "z:t:fx", "EUR", "USD", TTL).unwrap();
        assert_eq!(rate.rate, 1.5);
        assert_eq!(rate.source, SOURCE_LIVE);
        assert!(host
            .logs
            .iter()
            .any(|line| line.contains("cache read failed")));
    }

    #[test]
    fn malformed_cache_entry_is_treated_as_a_miss() {
        let mut host = MockHost::new();
        host.seed("z:t:fx", b"EUR:USD", b"{not json");
        host.queue_http_ok(&fx_body(1.7));
        let rate = lookup(&mut host, "z:t:fx", "EUR", "USD", TTL).unwrap();
        assert_eq!(rate.rate, 1.7);
        assert!(host
            .logs
            .iter()
            .any(|line| line.contains("not a valid rate")));
    }
}
