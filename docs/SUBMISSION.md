# ExpenseGuard — a confidential expense-compliance agent on T3N

**T3N Agent Build Challenge submission.** Repository: `<REPO_URL>` · Demo transcript: `client/demo-output/<TIMESTAMP>-invoke.json`

ExpenseGuard is an enterprise expense/invoice compliance agent whose policy, decision and audit trail never leave an attested enclave. It is a T3N TEE contract written in Rust, compiled to a WASM/WIT component, registered on T3N testnet, and driven end-to-end by an agent that holds its own scoped delegation.

## The problem it solves

Every company above a few dozen employees runs the same loop: an employee submits an expense, a rulebook decides whether it is allowed, and the result lands in an audit trail. Both halves are sensitive. The rulebook *is* the company's internal control environment — category limits, approval thresholds, vendor restrictions — and the ledger contains employees, vendors and amounts. Today that lives in a SaaS database where the vendor, and anyone who compromises them, can read all of it. Regulated buyers (finance, healthcare, defence) cannot put it there at all.

Putting the rulebook and the ledger inside a TEE changes the trust story: the policy is enforced by code running in an enclave, the audit record is written there, and neither is visible to the host, the operator, or the platform.

## What actually runs

The contract (`contract/`, Rust → `z_expense_guard.wasm`) exports six functions and holds all of the above:

| Function | What it does |
| --- | --- |
| `health` | liveness + the enclave's view of the contract identity |
| `set-policy` / `get-policy` | the rulebook: base currency, approval threshold, per-category limits, weekday restriction |
| `check-expense` | evaluates one expense against the policy and returns a verdict with the rule ids that fired |
| `request-approval` | posts the flagged expense to the configured webhook — through the host's HTTP egress, with `{{profile.*}}` placeholders resolved *inside* the enclave so PII never enters contract memory |
| `get-audit` | the append-only ledger inside the TEE |

Five T3N host capabilities carry the weight, and each one is load-bearing rather than decorative:

- **`kv-store`** — the policy, the audit ledger, the FX cache and the webhook URL. Maps are namespaced `z:<tid>:<tail>` and ACL'd so only the contract can write them.
- **`http` + `http-with-placeholders`** — live FX rates from `open.er-api.com`, and the approval webhook. This is why the FX conversion cannot be faked by the caller: the rate is fetched by the enclave, not supplied by the agent.
- **`tenant-context`** — the contract learns which tenant it is running for.
- **`logging`** — host-side logs, deliberately free of expense contents.
- **`token` / `ledger`** — metering, so an enterprise can price this per decision.

## The authority model

Three credentials, three jobs, no shared blast radius:

| Identity | Credential | Authority |
| --- | --- | --- |
| tenant | private key | registers the contract, creates the maps, seeds the policy |
| user (data owner) | private key | signs one scoped delegation grant |
| agent | opaque `t3n_key_…` bearer token | can call exactly six functions on exactly one contract, with egress to exactly two hosts |

The agent's credential is not a private key and never touches the session handshake: it goes to the invoke endpoint in an `X-T3N-Api-Key` header, and the contract's private material is generated inside the enclave and never leaves it.

## Deployed and verified

| Fact | Value |
| --- | --- |
| contract | `z:f817f49837375d99b44cbf8907becc154f2a5fc9:expense-guard` v0.1.0 |
| numeric contract id (what map ACLs take) | `1051` |
| wasm | 255,734 bytes, sha256 `5dd3a964bc2e0b3f…` |
| component validation | `wasm-tools validate` OK; import set identical to the vendor sample |
| native tests | 115 unit tests + 1 doctest, green |
| tenant DID | `did:t3n:f817f49837375d99b44cbf8907becc154f2a5fc9` |
| org / agent DID | `did:t3n:85c188ff697c5c7aae96b495bdc8c087dcdae4c9` / `did:t3n:a13591f52ba98b9068801c80719e56d03c74ee81` |

**The demo transcript is the evidence**: `client/demo-output/<TIMESTAMP>-invoke.json` is the raw output of `npm run demo` — a real agent session, against the registered contract, on testnet. Every step records what was asked, what came back, and how it was classified.

## What we found while building it

`docs/BUGLOG.md` documents 12 defects we hit and reproduced, each with the file and line, a verbatim error, and a proposed fix — among them: a duplicate import that stops the documented first paste from compiling, an undocumented property in the invoke example, a `.cargo/config.toml` that makes the vendor sample's own `cargo test` fail, four host-ABI capabilities (`kv-store.scan`, `set-claims-digest`, `token.get-balance`, `seq-no`) that exist in the vendored WIT but in none of the 48 published docs, an SDK shipped obfuscated with no source maps while the source repository is private, and a `contract_id` split — numeric for map ACLs, canonical for grants — whose numeric half cannot be read back from the node after a re-registration, so a re-register silently orphans a working ACL.

## Reproducing it

```bash
# contract
cd contract && cargo test && cargo build --target wasm32-wasip2 --release

# client (three credentials in client/.env, 0600)
cd ../client && npm install && npm run doctor && npm run register
npm run delegate && npm run demo
```

`docs/RUNBOOK.md` has the full sequence, the expected output and the failure modes; `docs/ARCHITECTURE.md` explains why the boundaries sit where they do; `docs/INTERFACE.md` is the frozen contract interface.
