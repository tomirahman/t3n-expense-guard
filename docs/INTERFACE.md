# Frozen interface — z-expense-guard

Single source of truth for both halves of the build. The Rust contract in
`contract/` and the TypeScript harness in `client/` must match this exactly.
Do not rename functions, maps or JSON keys without updating this file first.

## Identity

| item | value |
|---|---|
| WIT package | `z:expense-guard@0.1.0` |
| world | `expense-guard` |
| contract tail (registration) | `expense-guard` |
| canonical contract id | `z:<tid>:expense-guard` |
| wasm artefact | `contract/target/wasm32-wasip2/release/z_expense_guard.wasm` |
| host interfaces vendored | `host-interfaces-2.1.0`, `host-tenant-1.0.0` |

The vendored WIT versions must match what the reference implementation
(`reference/z-tenant-flight`) ships, because the host rejects a component that
imports an interface its world does not provide.

## KV maps (all under the tenant namespace, built at runtime)

`tenant_context::tenant_did()` returns raw bytes and must be hex-encoded exactly
once: `format!("z:{}:{}", hex::encode(tid), tail)`.

| map | keys | written by |
|---|---|---|
| `z:<tid>:policy` | `current` | `set-policy` |
| `z:<tid>:audit` | `seq:<000001>` (write-once record), `exp:<expense_id>` (pointer) | `check-expense`, `request-approval` |
| `z:<tid>:fx` | `<FROM>:<TO>` → `{ rate, fetched_at }` | `check-expense` (cache) |
| `z:<tid>:secrets` | `approval_webhook_url` | seeded by the tenant SDK (read-only for the contract) |

Audit sequence numbers are derived by reading the highest existing `seq:` key
and incrementing, zero-padded to 6 digits.

## Policy JSON

```json
{
  "base_currency": "USD",
  "approval_threshold": 500,
  "default_category_limit": 250,
  "duplicate_window_days": 30,
  "fx_cache_ttl_hours": 6,
  "categories": {
    "travel":        { "limit": 1500, "allowed": true },
    "software":      { "limit": 600,  "allowed": true },
    "entertainment": { "limit": 0,    "allowed": false }
  }
}
```

`approval_threshold` and per-category `limit` are expressed in
`base_currency`. A missing `categories` entry falls back to
`default_category_limit` with `allowed = true`.

## Verdicts and rule ids

`verdict` is one of `compliant`, `needs_approval`, `rejected`. `rule_hits` is a
list of stable ids (never localised prose) so downstream tooling can assert on
them:

| rule id | trigger | effect on verdict |
|---|---|---|
| `disallowed_category` | category present with `allowed: false` | `rejected` |
| `over_category_limit` | converted amount > category limit (limit > 0) | `needs_approval` |
| `over_approval_threshold` | converted amount > `approval_threshold` | `needs_approval` |
| `possible_duplicate` | same `employee_ref` + amount + currency + vendor inside `duplicate_window_days` | `needs_approval` |
| `fx_unavailable` | FX lookup failed and no usable cache entry | forces `needs_approval`, `base_amount` is `null` |
| `unknown_category` | category absent from the policy map | informational only |

Worst rule wins: `rejected` > `needs_approval` > `compliant`.

## Function I/O

All functions take the uniform `generic-input` envelope and return JSON bytes
or an error string. `input` is the JSON payload below; `user-profile` and
`context` are resolved host-side.

### `health`
input `{}` → `{ "ok": true, "contract": "expense-guard", "version": "0.1.0",
"tenant_did": "<hex>", "cluster_ts": 1234567890 }`

### `set-policy`
input: Policy → `{ "stored": true, "base_currency": "USD", "categories": 3 }`

### `get-policy`
input `{}` → Policy (or the built-in default when nothing is stored).

### `check-expense`
input:
```json
{ "expense_id": "EXP-1042", "employee_ref": "emp_7f3a", "amount": 812.40,
  "currency": "EUR", "category": "travel", "vendor": "Lufthansa",
  "incurred_on": "2026-09-02" }
```
output:
```json
{ "expense_id": "EXP-1042", "verdict": "needs_approval", "base_currency": "USD",
  "base_amount": 884.52, "fx_rate": 1.0888, "fx_source": "open.er-api.com",
  "rule_hits": ["over_approval_threshold"], "ledger_seq": 7,
  "evaluated_at": 1234567890 }
```

`employee_ref` is an opaque pseudonymous handle, never a name or email. The
contract must not require or store direct identifiers.

### `request-approval`
input: `{ "expense_id": "EXP-1042", "use_profile": true, "note": "client dinner" }`
output:
```json
{ "expense_id": "EXP-1042", "status": "sent", "http_code": 200,
  "approval_ref": "appr-...", "ledger_seq": 8 }
```

When `use_profile` is true the webhook body is built with
`{{profile.first_name}}`, `{{profile.last_name}}` and
`{{profile.verified_contacts.email.value}}` markers; when the host rejects a
marker with `PlaceholderUnknown` the contract retries once without the optional
markers and records `profile_placeholder_fallback` in the audit record. The
webhook URL is read from `z:<tid>:secrets` key `approval_webhook_url`.

### `get-audit`
input: `{ "expense_id": "EXP-1042", "limit": 20 }` (all fields optional)
output: `{ "count": 1, "entries": [ <record> ] }`, newest first.

Audit record shape:
```json
{ "seq": 7, "expense_id": "EXP-1042", "employee_ref": "emp_7f3a",
  "verdict": "needs_approval", "base_amount": 884.52, "category": "travel",
  "vendor": "Lufthansa", "rule_hits": ["over_approval_threshold"],
  "recorded_at": 1234567890, "contract_version": "0.1.0" }
```

## Egress hosts (granted per-call by the calling user)

| host | purpose |
|---|---|
| `open.er-api.com` | FX rates, no key, no PII |
| webhook host from `z:<tid>:secrets` | approval notification, PII via placeholders |

Both must appear in the `member-delegation-update` grant's `allowed_hosts`, or
outbound calls fail with `host/http.egress_denied`.

## Error strings

Failures return `Err(String)` with a stable prefix so the harness can assert on
them: `policy:`, `kv:`, `fx:`, `approval:`, `input:`.
