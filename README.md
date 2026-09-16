# ExpenseGuard — an enterprise expense-compliance agent on T3N

ExpenseGuard is a small, production-shaped agent that audits employee expense
claims inside a Terminal 3 confidential-computing enclave: it applies the
company's policy, converts foreign-currency claims to the reporting currency,
detects duplicates, and writes a tamper-evident audit record — while employee
PII never enters the contract's memory and the company's policy never leaves
the enclave.

It is deliberately narrow. One tenant, one policy, one ledger, six functions.
Everything the agent is allowed to do is declared in its WIT imports and in the
data owner's delegation grant, so the blast radius is inspectable by a
compliance team instead of implied by a server-side config.

```
                     ┌─────────────────────── T3N enclave (Intel TDX) ───────────────────────┐
  employee claim ──▶ │  z:<tid>:expense-guard   (Rust → WASM component, tenant contract)     │
  (no PII)           │                                                                      │
                     │   check-expense ──▶ policy eval ──▶ FX lookup (open.er-api.com)       │
  agent (scoped      │        │                    │                                        │
  delegation grant) ─┼────────┘                    └──▶ z:<tid>:fx      (rate cache)        │
                     │                                                                      │
                     │   request-approval ──▶ http-with-placeholders ──▶ approver webhook   │
                     │        │                {{profile.*}} resolved host-side,            │
                     │        │                plaintext PII never enters WASM               │
                     │        └──▶ z:<tid>:audit   (append-only, seq-numbered ledger)        │
                     └──────────────────────────────────────────────────────────────────────┘
```

## What it does

| function | purpose | egress |
|---|---|---|
| `health` | liveness + self-description, no KV or egress | — |
| `set-policy` | install or replace the tenant's expense policy | — |
| `get-policy` | read the installed policy back | — |
| `check-expense` | evaluate a claim: category limits, approval threshold, duplicate window, FX conversion; write an audit record | `open.er-api.com` |
| `request-approval` | ask a human approver for sign-off, with PII resolved host-side from `{{profile.*}}` markers | approver webhook |
| `get-audit` | read the ledger back, newest first | — |

A verdict is `compliant`, `needs_approval` or `rejected`, always accompanied by
stable rule ids (`over_category_limit`, `possible_duplicate`, …) rather than
prose, so downstream systems can assert on them.

## Why this needs an enclave

A compliance agent is a strange thing to trust: it sees every claim, it knows
the policy thresholds, and its output decides who gets reimbursed. Running it
inside T3N buys three concrete properties:

1. **Policy confidentiality.** Thresholds and category rules live in
   `z:<tid>:policy`, readable only by the tenant's own contract. The cloud
   operator running the node cannot read them.
2. **PII minimisation that is architectural, not procedural.** The contract
   never receives a name or email. Approval notifications are templated with
   `{{profile.<field>}}` markers and the host substitutes them inside the
   enclave at dispatch time — so the PII exists only in the outbound request,
   never in WASM linear memory, never in our logs.
3. **Capability scoping by construction.** The contract's entire capability set
   is its WIT import list (`log`, `kv-store`, `http`, `http-with-placeholders`,
   `tenant-context`). Outbound hosts are authorised per-call by the data owner's
   delegation grant, not by the contract, so an approved agent can reach
   `open.er-api.com` and nowhere else.

## Repository layout

```
contract/            Rust TEE contract (own crate, compiled to a WASM component)
  wit/world.wit        exported `contracts` interface + host imports = capabilities
  wit/deps/            vendored host ABI packages (must match the cluster)
  src/                 policy.rs, fx.rs, ledger.rs, approval.rs, maps.rs, errors.rs
client/              Node/TypeScript harness (separate project, as the docs require)
  src/01-register-contract.ts … src/04-demo.ts
docs/INTERFACE.md    frozen interface: functions, JSON payloads, maps, rule ids
docs/ARCHITECTURE.md threat model and data flow
docs/BUGLOG.md       every defect, doc mismatch and rough edge found while building
docs/HANDOVER.md     how to run this after the challenge, or take it over
```

## Requirements

- Rust 1.98 + `rustup target add wasm32-wasip2`
- Node 22+ and npm
- A T3N developer key with test credits — one per identity (tenant, agent, data
  owner). Agents do **not** share the tenant's credits.

## Build the contract

```bash
cd contract
cargo test                                        # policy/ledger unit tests
cargo build --target wasm32-wasip2 --release      # -> target/wasm32-wasip2/release/z_expense_guard.wasm
```

## Run the client

```bash
cd client
cp .env.example .env && $EDITOR .env              # three keys + webhook URL
npm install
npm run doctor                                    # 21 preflight checks: env, SDK, wasm, hosts, PII_DID
npm run register                                  # tenant: upload the wasm, create the 4 maps + ACLs,
                                                  #         seed the policy and the secrets row
npm run delegate                                  # data owner: scope the agent on 6 functions, 2 hosts
npm run demo                                      # agent: 8 checks end to end, writes a transcript
```

`npm run seed` re-seeds only the `secrets` row on an already-deployed contract —
idempotent, and the safe entry point once `register` has run.

`npm run demo` prints one line per check and writes the full transcript to
`client/demo-output/<timestamp>.json` for auditability and screenshots.

## Policy example

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

## Status

Built for the T3N Agent Build Challenge. See `docs/BUGLOG.md` for every defect
and documentation mismatch found during the build, and `docs/HANDOVER.md` for
what we would do next in production.
