//! Native test double for the [`crate::capabilities::Host`] boundary.
//!
//! The contract talks to the cluster exclusively through `Host`, so a tiny
//! in-memory implementation of it is enough to exercise every statement of the
//! contract — policy evaluation, the FX cache, ledger sequencing, duplicate
//! detection and the approval retry — with plain `cargo test`, no cluster
//! round-trip and no WASM toolchain.
//!
//! This module is compiled for tests only (`#[cfg(test)]` in `lib.rs`).

use std::collections::BTreeMap;
use std::ops::Bound;

use serde_json::Value;

use crate::capabilities::{Host, HttpResponse};
use crate::errors::WebhookError;
use crate::maps::Namespace;

/// An `open.er-api.com` style payload for a fixed rate to `USD`.
pub fn fx_body(rate: f64) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "result": "success",
        "base_code": "EUR",
        "rates": { "USD": rate }
    }))
    .unwrap()
}

/// A recording, in-memory host.
#[derive(Debug)]
pub struct MockHost {
    /// Raw tenant DID returned by `tenant-context`.
    pub tid: Vec<u8>,
    /// Cluster timestamp returned by `tenant-context` (unix seconds).
    pub ts: u64,
    /// KV state: map name → key → value.
    pub maps: BTreeMap<String, BTreeMap<Vec<u8>, Vec<u8>>>,
    /// Maps whose reads should fail (fault injection).
    pub kv_get_failures: BTreeMap<String, String>,
    /// Maps whose scans should fail (fault injection).
    pub kv_scan_failures: BTreeMap<String, String>,
    /// Maps whose writes should fail (fault injection).
    pub kv_put_failures: BTreeMap<String, String>,
    /// Canned `http.call` outcomes, consumed in order.
    pub http_get_queue: Vec<Result<HttpResponse, String>>,
    /// URLs passed to `http.call`, in order.
    pub http_get_calls: Vec<String>,
    /// Canned `http-with-placeholders.call` outcomes, consumed in order.
    pub webhook_queue: Vec<Result<HttpResponse, WebhookError>>,
    /// `(url, body)` pairs passed to `http-with-placeholders.call`, in order.
    pub webhook_calls: Vec<(String, Vec<u8>)>,
    /// Everything the contract logged.
    pub logs: Vec<String>,
}

impl Default for MockHost {
    fn default() -> Self {
        Self {
            tid: vec![0x8f; 20],
            ts: 1_700_000_000,
            maps: BTreeMap::new(),
            kv_get_failures: BTreeMap::new(),
            kv_scan_failures: BTreeMap::new(),
            kv_put_failures: BTreeMap::new(),
            http_get_queue: Vec::new(),
            http_get_calls: Vec::new(),
            webhook_queue: Vec::new(),
            webhook_calls: Vec::new(),
            logs: Vec::new(),
        }
    }
}

impl MockHost {
    pub fn new() -> Self {
        Self::default()
    }

    /// The `z:<tid>:*` map names for this mock's tenant.
    pub fn namespace(&self) -> Namespace {
        Namespace::new(&self.tid)
    }

    pub fn seed(&mut self, map: &str, key: &[u8], value: &[u8]) {
        self.maps
            .entry(map.to_string())
            .or_default()
            .insert(key.to_vec(), value.to_vec());
    }

    /// Seed a JSON value (serialised) into a map.
    pub fn seed_json(&mut self, map: &str, key: &[u8], value: Value) {
        let bytes = serde_json::to_vec(&value).unwrap();
        self.seed(map, key, &bytes);
    }

    /// Seed the tenant policy document (`null` = store an empty policy object).
    pub fn seed_policy(&mut self, policy: Value) {
        let map = self.namespace().policy();
        self.seed(&map, b"current", &serde_json::to_vec(&policy).unwrap());
    }

    pub fn seed_raw_policy(&mut self, bytes: &[u8]) {
        let map = self.namespace().policy();
        self.seed(&map, b"current", bytes);
    }

    /// Provision the approval webhook secret.
    pub fn seed_webhook_secret(&mut self, url: &str) {
        let map = self.namespace().secrets();
        self.seed(&map, b"approval_webhook_url", url.as_bytes());
    }

    pub fn value_at(&self, map: &str, key: &[u8]) -> Option<Vec<u8>> {
        self.maps
            .get(map)
            .and_then(|entries| entries.get(key))
            .cloned()
    }

    pub fn json_at(&self, map: &str, key: &[u8]) -> Option<Value> {
        self.value_at(map, key)
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
    }

    /// Keys of a map, lexicographically ordered (KV order).
    pub fn keys(&self, map: &str) -> Vec<String> {
        self.maps
            .get(map)
            .map(|entries| {
                entries
                    .keys()
                    .map(|key| String::from_utf8_lossy(key).to_string())
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn queue_http_json(&mut self, code: u16, body: Value) {
        self.http_get_queue.push(Ok(HttpResponse {
            code,
            body: serde_json::to_vec(&body).unwrap(),
        }));
    }

    /// Queue a raw response for `http.call`.
    pub fn queue_http_status(&mut self, code: u16, body: &[u8]) {
        self.http_get_queue.push(Ok(HttpResponse {
            code,
            body: body.to_vec(),
        }));
    }

    /// Queue a `200 OK` response with a raw body for `http.call`.
    pub fn queue_http_ok(&mut self, body: &[u8]) {
        self.queue_http_status(200, body);
    }

    pub fn queue_http_error(&mut self, message: &str) {
        self.http_get_queue.push(Err(message.to_string()));
    }

    pub fn queue_webhook_ok(&mut self, code: u16, body: &[u8]) {
        self.webhook_queue.push(Ok(HttpResponse {
            code,
            body: body.to_vec(),
        }));
    }

    pub fn queue_webhook_err(&mut self, error: WebhookError) {
        self.webhook_queue.push(Err(error));
    }
}

impl Host for MockHost {
    fn tenant_did(&self) -> Vec<u8> {
        self.tid.clone()
    }

    fn cluster_ts(&self) -> u64 {
        self.ts
    }

    fn kv_get(&mut self, map: &str, key: &[u8]) -> Result<Option<Vec<u8>>, String> {
        if let Some(error) = self.kv_get_failures.get(map) {
            return Err(error.clone());
        }
        Ok(self.value_at(map, key))
    }

    fn kv_put(&mut self, map: &str, key: &[u8], value: &[u8]) -> Result<(), String> {
        if let Some(error) = self.kv_put_failures.get(map) {
            return Err(error.clone());
        }
        self.seed(map, key, value);
        Ok(())
    }

    fn kv_scan(
        &mut self,
        map: &str,
        start: &[u8],
        end: &[u8],
        limit: u32,
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>, String> {
        if let Some(error) = self.kv_scan_failures.get(map) {
            return Err(error.clone());
        }
        let Some(entries) = self.maps.get(map) else {
            return Ok(Vec::new());
        };
        // Half-open range, lexicographic order, truncated by `limit` — the same
        // semantics the cluster's kv-store guarantees.
        Ok(entries
            .range((
                Bound::Included(start.to_vec()),
                Bound::Excluded(end.to_vec()),
            ))
            .take(limit as usize)
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect())
    }

    fn http_get(&mut self, url: &str) -> Result<HttpResponse, String> {
        self.http_get_calls.push(url.to_string());
        if self.http_get_queue.is_empty() {
            return Err("mock has no canned http.call response".to_string());
        }
        self.http_get_queue.remove(0)
    }

    fn http_post_with_placeholders(
        &mut self,
        url: &str,
        _headers: &[(String, String)],
        body: &[u8],
    ) -> Result<HttpResponse, WebhookError> {
        self.webhook_calls.push((url.to_string(), body.to_vec()));
        if self.webhook_queue.is_empty() {
            return Err(WebhookError::UpstreamError(
                "mock has no canned webhook response".to_string(),
            ));
        }
        self.webhook_queue.remove(0)
    }

    fn log_info(&mut self, message: &str) {
        self.logs.push(message.to_string());
    }

    fn log_error(&mut self, message: &str) {
        self.logs.push(message.to_string());
    }
}
