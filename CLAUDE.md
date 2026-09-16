# AGENTS.md — z-expense-guard

## Overview

ExpenseGuard is an enterprise expense-compliance agent that runs inside a T3N
confidential-computing enclave: a Rust/WIT TEE contract holds the policy, detects
duplicate claims and converts currency, and a TypeScript harness registers it,
grants a scoped agent access to it, and drives the flow end to end.
Contract: Rust 2021, `wit-bindgen`, target `wasm32-wasip2`, cargo.
Harness: TypeScript 5.9 on Node >= 20 (ESM), `@terminal3/t3n-sdk` 5.17.0, `tsx`
for execution, npm as the package manager. No Python runtime dependency — the one
Python file (`tools/terminal-to-png.py`) is an offline evidence tool.

## Commands

```bash
# contract
cd contract
cargo test                                        # 115 native tests + 1 doctest (no cluster)
cargo build --target wasm32-wasip2 --release      # WASM component (or: cargo wasm)
wasm-tools validate target/wasm32-wasip2/release/z_expense_guard.wasm
wasm-tools component wit target/wasm32-wasip2/release/z_expense_guard.wasm   # inspect ABI

# harness — each step runs as a different identity, in this order
cd client
npm ci
cp .env.example .env && chmod 600 .env            # then fill in the credentials
npm run doctor                                    # 21 preflight checks; stops at the first failure
npm run register                                  # tenant: register, create maps, seed policy + secrets
npm run seed                                      # tenant: re-seed the secrets row only (idempotent)
npm run delegate                                  # data owner: grant the agent its scope (6 functions)
npm run demo                                      # agent: the expense flow + transcript (8 checks)
npm run typecheck                                 # tsc --noEmit
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

Evidence is a transcript, not a screenshot: `npm run demo` writes
`client/demo-output/<timestamp>-invoke.json`, and `tools/terminal-to-png.py`
renders a real `script(1)` capture into an image when a write-up needs one.

## Boundaries

- **NEVER** commit `client/.env`, a `0x…` key, or a `t3n_key_…` bearer token.
  Keys are shown once by the platform and are not recoverable.
- **NEVER** edit `contract/wit/deps/**` by hand — it is vendored from the host
  ABI, and a drifted import set makes the component fail to load.
- **ALWAYS** update `docs/INTERFACE.md` before renaming a function, map or JSON
  key; it is the contract between the two halves of the build.
- `contract/target/**` and `client/artifacts/**` are generated. Do not hand-edit
  or commit them; `client/artifacts/registration.json` holds the numeric
  `contract_id` that a later step needs (`BUG-11`).

## Dependencies

`@terminal3/t3n-sdk` is the only runtime client dependency and it ships
obfuscated — see BUGLOG BUG-10. `wit-bindgen` generates the host bindings; its
version must track the vendored WIT versions. Keep the contract's dependency list
minimal: every extra crate is both attack surface and enclave load time.

## Config

| variable | meaning |
|---|---|
| `T3N_ENVIRONMENT` | `testnet` |
| `TENANT_API_KEY` | tenant identity: owns the contract, maps and policy |
| `TENANT_DID` | tenant DID; read back from a session, never constructed |
| `USER_API_KEY` | optional data owner who signs the grant (the delegator here is the tenant identity) |
| `PII_DID` | the identity a delegated call acts for; unset means a self call with no egress grant (`BUG-14`) |
| `ORG_DID` | organisation that owns the provisioned agent |
| `AGENT_DID` | the agent that calls the contract |
| `AGENT_KEY_ID` | the public half of the agent's bearer token |
| `AGENT_INVOKE_KEY` | opaque `t3n_key_<id>.<secret>` token, sent as `X-T3N-Api-Key` |
| `CONTRACT_TAIL`, `CONTRACT_VERSION` | registration parameters, checked against `docs/INTERFACE.md` |
| `FX_BASE_CURRENCY` | the policy's base currency for conversion |
| `APPROVAL_WEBHOOK_URL` | fallback for the `z:<tid>:secrets` row the contract reads |

## Error Handling

Platform errors surface as structured SDK errors and must be classified, not
swallowed: authorization failures (`host/http.egress_denied`) are a missing
grant, metering failures (`InsufficientCreditError`) are an unfunded identity,
and `AccessDenied` on a KV read is a map missing its `readers` list. The demo
classifies each step `PASS` / `MISMATCH` against the verdict the policy should
produce and prints the raw response either way — a demo that hides a failure is
worse than one that reports it.

## Troubleshooting

1. `<NAME> is not set` from doctor → `.env` missing or still placeholder-filled.
2. `AccessDenied` reading a map → the KV governor defaults to deny; set both
   `readers` and `writers`, and remember the ACL takes the **numeric**
   `contract_id` (`BUG-11`).
3. `host/http.egress_denied` → the host is missing from the caller's
   `member-delegation-update` grant `allowed_hosts`.
4. `InsufficientCreditError` as the agent → an agent DID's balance is separate
   from the tenant's and starts at zero; fund it before invoking.
5. `cargo test` tries to run a `.wasm` binary → the sample's `.cargo/config.toml`
   pins the wasm target as default; ours keeps the host default (`BUG-09`).
6. Component rejected at registration → the WIT import set drifted from the host
   ABI; compare with `wasm-tools component wit` against a known-good component.
