# z-expense-guard — the TEE contract

The confidential half of ExpenseGuard. It runs inside a T3N enclave as a
WASM/WIT component and holds the parts of the system that must never leave the
attested boundary: the policy document, the duplicate-detection window and the
audit ledger.

`docs/INTERFACE.md` is the normative specification — function names, JSON keys,
verdict strings and rule ids are frozen there. This file describes how the
implementation is organised and what it touches at runtime.

## Build and test

```bash
cargo test                                     # 115 native unit tests, no cluster needed
cargo build --target wasm32-wasip2 --release   # the deployable component
wasm-tools component wit target/wasm32-wasip2/release/z_expense_guard.wasm  # verify the ABI
```

The default cargo target is the host triple on purpose: it keeps `cargo test`
working natively instead of trying to execute a `.wasm` test binary. Use the
explicit `--target wasm32-wasip2` (or the `cargo wasm` alias) for the component.
See `docs/RUNBOOK.md` and BUGLOG BUG-09.

## Layout

| file | responsibility |
|---|---|
| `src/lib.rs` | WIT glue, the `generic-input` envelope, the 6 exported entry points |
| `src/capabilities.rs` | the `Host` trait — every side effect goes through it, so all logic is host-agnostic and testable natively |
| `src/policy.rs` | policy parsing, defaults, verdict aggregation |
| `src/fx.rs` | exchange-rate lookup and TTL cache |
| `src/ledger.rs` | audit append, sequence allocation, duplicate detection |
| `src/approval.rs` | webhook POST with `{{profile.*}}` placeholders |
| `src/maps.rs` | map-name construction from the tenant DID |
| `src/errors.rs` | the stable error prefixes |
| `src/test_support.rs` | in-memory mock host: KV, scan, queued HTTP responses, fault injection |

## Host capabilities used

The component imports exactly five host interfaces, and nothing else:

```
host:tenant/tenant-context@1.0.0        cluster time + the tenant DID
host:interfaces/logging@2.1.0
host:interfaces/kv-store@2.1.0          get / put / scan
host:interfaces/http@2.1.0             egress for FX rates
host:interfaces/http-with-placeholders@2.1.0   egress with {{profile.*}} substitution
```

They are vendored under `wit/deps/` and must not be edited: the host refuses to
load a component whose import set drifts from the ABI it provides. `wasm-tools
component wit` prints the import set of the built artefact, which is the fastest
way to check that.

Egress is authorised per call by the *calling user's* delegation grant, not by
the contract, so `allowed_hosts` has to name `open.er-api.com` and the approval
webhook host or the call fails with `host/http.egress_denied`.

## Functions

Every export takes the same envelope and returns JSON bytes or an error string:

```wit
record generic-input {
  input: option<list<u8>>,          // the JSON payload
  user-profile: option<list<u8>>,   // resolved host-side, PII never enters contract memory
  context: option<list<u8>>,
}
```

| function | input | output | touches |
|---|---|---|---|
| `health` | `{}` | `{ ok, contract, version, tenant_did, cluster_ts }` | none |
| `set-policy` | a policy document | `{ stored, base_currency, categories }` | writes `policy/current` |
| `get-policy` | `{}` | the policy, or the built-in default | reads `policy/current` |
| `check-expense` | one claim | verdict, converted amount, rule hits, ledger seq | reads `policy/current`, `fx/<FROM>:<TO>`, `audit/exp:<id>`; writes the FX cache, an `audit/seq:<n>` record and an `audit/exp:<id>` pointer |
| `request-approval` | `{ expense_id, use_profile, note }` | `{ status, http_code, approval_ref, ledger_seq }` | reads `secrets/approval_webhook_url`, `audit/exp:<id>`; appends an audit record |
| `get-audit` | `{ expense_id?, limit? }` | `{ count, entries[] }`, newest first | scans `audit/seq:<n>` |

Map names are always `z:<tenant-did-hex>:<tail>`. The tenant DID arrives as raw
bytes from `tenant_context::tenant_did()` and is hex-encoded exactly once, in
`maps.rs`; encoding it twice (or not at all) silently matches no map at all.

## Audit and duplicate detection

The ledger is append-only by construction: records are written under `seq:<n>`
with the sequence allocated by scanning for the current maximum, and the
`exp:<expense_id>` pointer is a separate key that only ever holds the most recent
record for that claim. A verdict of `possible_duplicate` comes from matching
employee, amount, currency and vendor inside the policy's
`duplicate_window_days`, which is why the window has to be read from the policy
rather than baked into the code.

## Verification posture

Everything that touches the outside world is behind the `Host` trait, so the
business rules are covered natively (115 tests) without a cluster. The WASM-only
code path is deliberately thin — `WasmHost` and the WIT glue — because it is the
part that can only be exercised against a live enclave.

## Two interface notes we are aware of

Both are recorded in `docs/INTERFACE.md` and were implemented as written rather
than silently changed:

1. `set-policy` returns `categories` as a **count** (`{ "categories": 3 }`) while
   the policy document itself uses `categories` for the rule **map**. The same
   key name means two different things one level apart.
2. `request-approval` carries only `expense_id`, so it cannot supply
   `employee_ref`, `base_amount`, `category` and `vendor`, which the audit record
   requires. It inherits them from the expense's latest `check-expense` record,
   leaving them empty when the claim was never evaluated first.
