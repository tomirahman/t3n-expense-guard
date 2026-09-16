//! The host capability boundary.
//!
//! Every host call the contract makes is funnelled through the [`Host`] trait.
//! That keeps the compliance logic (policy evaluation, FX cache handling, ledger
//! writes, approval flow) native-testable — `cargo test` exercises the real code
//! paths against an in-memory host, while [`WasmHost`] stays the only
//! `wasm32`-gated code in the crate and simply forwards to the generated
//! `host:interfaces` / `host:tenant` bindings.

use crate::errors::WebhookError;

/// A host HTTP response: status code plus raw body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpResponse {
    pub code: u16,
    pub body: Vec<u8>,
}

impl HttpResponse {
    /// True for 2xx.
    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.code)
    }
}

/// The host capabilities `z-expense-guard` requires.
///
/// Manifest declaration (`host_capabilities`):
/// `["kv_store", "logging", "tenant_context", "http", "http_with_placeholders"]`.
pub trait Host {
    /// Raw (non-hex) 20-byte tenant DID from `tenant-context`.
    /// Hex-encode it once via [`crate::maps::Namespace`].
    fn tenant_did(&self) -> Vec<u8>;

    /// Cluster-wide timestamp in seconds (`tenant-context.cluster-timestamp-secs`).
    /// The only clock the contract may use — `std::time` does not exist here.
    fn cluster_ts(&self) -> u64;

    /// `kv-store::get(map_name, key)`.
    fn kv_get(&mut self, map: &str, key: &[u8]) -> Result<Option<Vec<u8>>, String>;

    /// `kv-store::put(map_name, key, value)`.
    fn kv_put(&mut self, map: &str, key: &[u8], value: &[u8]) -> Result<(), String>;

    /// `kv-store::scan(map_name, start, end, limit)` — half-open `[start, end)`,
    /// lexicographic, one-shot, `limit > 0`.
    fn kv_scan(
        &mut self,
        map: &str,
        start: &[u8],
        end: &[u8],
        limit: u32,
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>, String>;

    /// `http::call` with `verb::get` (no placeholders, no PII).
    fn http_get(&mut self, url: &str) -> Result<HttpResponse, String>;

    /// `http-with-placeholders::call` with `verb::post`. `body` may contain
    /// `{{profile.<field>}}` markers; the host resolves them at dispatch time,
    /// so plaintext PII never enters WASM memory.
    fn http_post_with_placeholders(
        &mut self,
        url: &str,
        headers: &[(String, String)],
        body: &[u8],
    ) -> Result<HttpResponse, WebhookError>;

    /// `logging::info`.
    fn log_info(&mut self, message: &str);

    /// `logging::error`.
    fn log_error(&mut self, message: &str);
}

#[cfg(target_arch = "wasm32")]
mod wasm {
    //! The only wasm-gated module in the crate: thin forwarders to the
    //! bindings generated from `wit/`.

    use super::{Host, HttpResponse};
    use crate::errors::WebhookError;
    use crate::host::{
        interfaces::{http, http_with_placeholders as hwp, kv_store, logging},
        tenant::tenant_context,
    };

    /// Host capabilities backed by the real T3N host functions.
    #[derive(Debug, Clone, Copy, Default)]
    pub struct WasmHost;

    impl Host for WasmHost {
        fn tenant_did(&self) -> Vec<u8> {
            tenant_context::tenant_did()
        }

        fn cluster_ts(&self) -> u64 {
            tenant_context::cluster_timestamp_secs()
        }

        fn kv_get(&mut self, map: &str, key: &[u8]) -> Result<Option<Vec<u8>>, String> {
            kv_store::get(map, key)
        }

        fn kv_put(&mut self, map: &str, key: &[u8], value: &[u8]) -> Result<(), String> {
            kv_store::put(map, key, value)
        }

        fn kv_scan(
            &mut self,
            map: &str,
            start: &[u8],
            end: &[u8],
            limit: u32,
        ) -> Result<Vec<(Vec<u8>, Vec<u8>)>, String> {
            kv_store::scan(map, start, end, limit)
        }

        fn http_get(&mut self, url: &str) -> Result<HttpResponse, String> {
            let response = http::call(&http::Request {
                method: http::Verb::Get,
                url: url.to_string(),
                headers: None,
                payload: None,
            })?;
            Ok(HttpResponse {
                code: response.code,
                body: response.payload,
            })
        }

        fn http_post_with_placeholders(
            &mut self,
            url: &str,
            headers: &[(String, String)],
            body: &[u8],
        ) -> Result<HttpResponse, WebhookError> {
            let response = hwp::call(&hwp::Request {
                method: hwp::Verb::Post,
                url: url.to_string(),
                headers: Some(headers.to_vec()),
                payload: Some(body.to_vec()),
            })
            .map_err(WebhookError::from)?;
            Ok(HttpResponse {
                code: response.code,
                body: response.payload,
            })
        }

        fn log_info(&mut self, message: &str) {
            let _ = logging::info(message);
        }

        fn log_error(&mut self, message: &str) {
            let _ = logging::error(message);
        }
    }

    impl From<hwp::HttpError> for WebhookError {
        fn from(error: hwp::HttpError) -> Self {
            match error {
                hwp::HttpError::EgressDenied(host) => Self::EgressDenied(host),
                hwp::HttpError::PlaceholderDenied(marker) => Self::PlaceholderDenied(marker),
                hwp::HttpError::PlaceholderUnknown(field) => Self::PlaceholderUnknown(field),
                hwp::HttpError::PlaceholderNoUserContext => Self::PlaceholderNoUserContext,
                hwp::HttpError::UpstreamError(reason) => Self::UpstreamError(reason),
            }
        }
    }
}

#[cfg(target_arch = "wasm32")]
pub use wasm::WasmHost;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn success_range_is_2xx() {
        for code in [200_u16, 201, 204, 299] {
            assert!(HttpResponse { code, body: vec![] }.is_success(), "{code}");
        }
        for code in [199_u16, 300, 400, 500] {
            assert!(!HttpResponse { code, body: vec![] }.is_success(), "{code}");
        }
    }
}
