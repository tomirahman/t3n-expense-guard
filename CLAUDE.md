# AGENTS.md — z-expense-guard

## Overview

ExpenseGuard is an enterprise expense-compliance agent that runs inside a T3N
confidential-computing enclave: a Rust/WIT TEE contract holds the policy, detects
duplicate claims and converts currency, and a TypeScript harness deploys it,
grants a scoped agent access to it, and demonstrates the flow end to end.
Contract: Rust 2021, `wit-bindgen` 0.49, target `wasm32-wasip2`, cargo.
Harness: TypeScript 5.9 on Node >= 20 (ESM), `@terminal3/t3n-sdk` 5.17.0, `tsx`
for execution, npm as the package manager. No Python, no Docker.

## Commands

```bash
# contract
cd contract
cargo test                                        # native unit tests (fast, no cluster)
cargo build --target wasm32-wasip2 --release      # WASM component (or: cargo wasm)
wasm-tools component wit target/wasm32-wasip2/release/z_expense_guard.wasm  # inspect ABI

# harness
cd client
npm ci
cp .env.example .env                              # then fill in the keys
npm run typecheck
npm run demo                                      # full flow against testnet
```

## Conventions

Every exported contract function takes the same `generic-input` envelope and
returns JSON bytes; failures are `Err(String)` with a stable prefix (`policy:`,
`kv:`, `fx:`, `approval:`, `input:`). Business decisions are reported as stable
rule ids (`over_category_limit`), never as prose, so the harness can assert.

KV map names are always derived from the tenant DID — never hard-coded:

```rust
pub fn canonical(&self, tail: &str) -> String {
    format!("z:{}:{}", self.tenant_hex, tail)
}
```

## Boundaries

- **NEVER** commit `client/.env`, a `0x…` key, or a `t3n_key_…` bearer token.
  Keys are shown once by the platform and are not recoverable.
- **NEVER** edit `contract/wit/deps/**` by hand — it is vendored from the host
  ABI, and a drifted import set makes the component fail to load.
- **ALWAYS** update `docs/INTERFACE.md` before renaming a function, map or JSON
  key; it is the contract between the two halves of the build.
- `contract/target/**` is generated. Do not hand-edit or commit it.

## Dependencies

`t3n-sdk` is the only runtime client dependency and it is obfuscated — see
BUGLOG BUG-10. `wit-bindgen` generates the host bindings; its version must track
the vendored WIT versions. Keep the contract's dependency list minimal: every
extra crate is both attack surface and enclave load time.

## Config

| variable | meaning |
|---|---|
| `T3N_ENVIRONMENT` | `testnet` |
| `TENANT_API_KEY` | tenant identity: owns the contract, maps and policy |
| `TENANT_DID` | tenant DID; read back from a session, never constructed |
| `ORG_DID` | organisation that owns the provisioned agent |
| `AGENT_DID` | the agent that calls the contract |
| `AGENT_INVOKE_KEY` | opaque `t3n_key_<id>.<secret>` bearer token for the agent |

## Error Handling

Platform errors surface as structured SDK errors and must be classified, not
swallowed: authorization failures (`host/http.egress_denied`) are a missing
grant, metering failures (`InsufficientCreditError`) are an unfunded identity,
and `AccessDenied` on a KV read is a map missing its `readers` list. Print the
verbatim error and continue — a demo that hides a failure is worse than one that
reports it.

## Troubleshooting

1. `AccessDenied` reading a map → the KV governor defaults to deny; set both
   `readers` and `writers` explicitly when creating the map.
2. `host/http.egress_denied` → the host is missing from the caller's
   `member-delegation-update` grant `allowed_hosts`.
3. `InsufficientCreditError` as the agent → an agent DID's balance is separate
   from the tenant's and starts at zero; fund it before invoking.
4. `cargo test` tries to run a `.wasm` binary → pass
   `--target x86_64-unknown-linux-gnu`; see BUGLOG BUG-09.
5. Component rejected at registration → the WIT import set drifted from the host
   ABI; compare with `wasm-tools component wit` against the reference sample.
