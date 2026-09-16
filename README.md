# ExpenseGuard

### Confidential expense-compliance agent for Terminal 3

ExpenseGuard reviews employee expense claims inside a Terminal 3
confidential-computing enclave: it applies the company's expense policy,
converts foreign-currency claims to the reporting currency at a rate fetched
from inside the enclave, detects duplicate submissions, routes
approval-required claims to a human approver, and appends every decision to a
TEE-resident append-only audit ledger. Employee PII never enters contract
memory, and the policy itself never leaves the enclave.

Repository: <https://github.com/tomirahman/t3n-expense-guard> ·
Submission document: [`docs/SUBMISSION.md`](docs/SUBMISSION.md) ·
Bug log: [`docs/BUGLOG.md`](docs/BUGLOG.md) ·
Handover: [`docs/HANDOVER.md`](docs/HANDOVER.md)

It is deliberately narrow: one tenant, one policy, one ledger, six contract
functions. Everything the agent is allowed to do is declared in its WIT imports
and in the data owner's delegation grant, so the blast radius is inspectable by
a compliance team instead of implied by a server-side config.

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

## What It Does

| Function | Purpose | Egress |
|---|---|---|
| `health` | liveness + self-description, no KV, no egress | — |
| `set-policy` | install or replace the tenant's expense policy | — |
| `get-policy` | read the installed policy back | — |
| `check-expense` | evaluate a claim: category limits, approval threshold, duplicate window, FX conversion; append an audit record | `open.er-api.com` |
| `request-approval` | ask a human approver for sign-off, with PII resolved host-side from `{{profile.*}}` markers | approver webhook |
| `get-audit` | read the ledger back, newest first | — |

A verdict is `compliant`, `needs_approval` or `rejected`, always accompanied by
stable rule ids (`over_category_limit`, `over_approval_threshold`,
`possible_duplicate`, `disallowed_category`, `fx_unavailable`) rather than prose,
so downstream systems can assert on them.

## Why This Is an Agent

ExpenseGuard is not a script that calls an API once; it is an actor with its own
identity that works a workflow end to end.

- The agent has **its own identity** (`did:t3n:a13591f52ba98b9068801c80719e56d03c74ee81`)
  and its own credential — an opaque `t3n_key_…` bearer token, not the tenant's
  private key.
- It operates under a **scoped delegation** signed by the data owner: exactly six
  contract functions and exactly two egress hosts. A call outside that scope is
  refused by the host, not by our conventions.
- It **invokes only authorized contract functions** — the delegation grant is the
  agent's entire authority, and the contract's WIT import list is its entire
  capability set.
- It **orchestrates the workflow**: preflight, policy retrieval, claim
  submission, verdict handling, approval routing and audit read-back.
- It **submits claims for evaluation** (`check-expense`) instead of deciding
  anything itself.
- It **handles the policy result**: `compliant` is done, `rejected` is recorded,
  `needs_approval` moves to the next step.
- It **routes approval-required cases to a human** (`request-approval`), and
  **retrieves the audit result** (`get-audit`) so the decision trail is part of
  the agent session, not a side effect.

ExpenseGuard intentionally does not use an LLM for compliance decisions. The T3N
agent orchestrates the controlled workflow, while policy evaluation executes
deterministically inside the TEE. This makes decisions reproducible, auditable,
and easier to integrate with enterprise systems: the same claim, policy and
contract version produce the same verdict and the same rule ids every time.

## Why T3N / TEE

A compliance agent is a strange thing to trust: it sees every claim, it knows the
policy thresholds, and its output decides who gets reimbursed. Running it inside
T3N buys three concrete properties:

1. **Policy confidentiality.** Thresholds and category rules live in
   `z:<tid>:policy`, readable only by the tenant's own contract. The cloud
   operator running the node cannot read them. For regulated buyers that is the
   difference between using a SaaS rulebook and running this at all.
2. **PII minimisation that is architectural, not procedural.** The contract never
   receives a name or an email — only `employee_ref` placeholders. Approval
   notifications are templated with `{{profile.<field>}}` markers and the host
   substitutes them inside the enclave at dispatch time, so the PII is resolved
   on the dispatch path instead of being stored in contract memory or in our logs.
3. **Capability scoping by construction.** The contract's capability set is its
   WIT import list (`log`, `kv-store`, `http`, `http-with-placeholders`,
   `tenant-context`). Outbound hosts are authorised per call from the data
   owner's delegation grant, so an approved agent can reach `open.er-api.com` and
   the approver webhook, and nothing else.

## Architecture

The diagram above is the whole data flow. `docs/ARCHITECTURE.md` has the threat
model, the KV layout and the failure-mode table; `docs/INTERFACE.md` is the
frozen interface — function payloads, map keys, verdicts, rule ids and error
strings. If a document and the code disagree anywhere, the code wins — the
numbers in this README were measured, not asserted.

## Live Demo

```bash
cd client
cp .env.example .env && $EDITOR .env   # TENANT_API_KEY, AGENT_INVOKE_KEY, TENANT_DID, PII_DID
npm install
npm run doctor                         # 21 preflight checks: env, SDK, wasm, hosts, identity
npm run register                       # tenant: upload the wasm, create the 4 maps + ACLs, seed policy and secrets
npm run delegate                       # data owner: scope the agent on 6 functions, 2 hosts
npm run demo                           # agent: the 8-step session below, writes a transcript
```

`npm run seed` re-seeds only the `secrets` row on an already-deployed contract;
`npm run delegate` is idempotent; `npm run verify` runs the type checker and the
build.

`npm run demo` is the acceptance test. It prints one line per step and writes the
full transcript to `client/demo-output/<timestamp>-invoke.json`.

| Step | Purpose | What a pass proves |
|---|---|---|
| `health` | agent/contract connectivity | the enclave answers and reports its own identity |
| `delegation` | scope before use | the agent's grant covers the five functions it is about to call |
| `get-policy` | confidential policy retrieval | the policy is readable by the contract and returns structured rules |
| `check-expense` (EUR claim) | normal path + FX | the verdict is `compliant` and the rate came from the enclave's live egress |
| `check-expense` (same claim again) | duplicate detection | the resubmission is caught and escalated |
| `check-expense` (above threshold) | approval routing decision | verdict `needs_approval` with `over_approval_threshold` — this is the case the agent hands to a human |
| `check-expense` (disallowed category) | policy enforcement | verdict `rejected` with `disallowed_category` |
| `get-audit` | persisted decision record | one record per evaluation is readable back |

The demo exercises four contract functions end to end (`health`, `get-policy`,
`check-expense`, `get-audit`). `request-approval` — the webhook dispatch that
carries a `needs_approval` claim to a human — is **not** part of the committed
transcript; it is covered by 15 unit tests (body templating, retry on an unknown
placeholder, status reporting). Treat that as the first thing to exercise against
a real endpoint.

Screenshots of two of those runs are in `docs/evidence/` (`doctor.png`,
`demo.png`), and the decision samples are quoted verbatim from a committed
transcript in [`docs/evidence/decision-samples.md`](docs/evidence/decision-samples.md).

## Verification

All rows below were produced on 2026-09-16 against the registered testnet
contract; commands are copy-pasteable from the repository root.

| Verification | Command | Result |
|---|---|---|
| Rust unit tests | `cd contract && cargo test` | **115 passed, 0 failed** + 1 doctest |
| WASM build | `cargo build --target wasm32-wasip2 --release` | OK — 255,734 bytes, sha256 `5dd3a964bc2e0b3f75419fc7dae645779dd68a0b33c91e68562c61fbd075db19` |
| Component validity | `wasm-tools validate --features all <wasm>` | OK |
| Deployed artifact = source | rebuild + `sha256sum` | identical hash to the artifact registered on testnet |
| Type check | `cd client && npm run typecheck` | exit 0 |
| T3N preflight | `npm run doctor` | **21 passed, 0 warned, 0 failed** (exit 0) — includes `contract_id 1051` and live host reachability |
| End-to-end demo | `npm run demo` | **8 of 8 steps green on four consecutive runs** (transcripts in `client/demo-output/`) |
| FX conversion | demo step 2 | live rate `fx_rate 1.153788`, `fx_source open.er-api.com`/`cache` for a EUR claim |
| Duplicate detection | demo step 3 | `needs_approval`, rule `possible_duplicate` |
| Approval routing (decision) | demo step 4 | `needs_approval` with rule `over_approval_threshold` |
| Approval dispatch (`request-approval`) | `cargo test` (`approval.rs`) | 15 unit tests green — body templating, retry on unknown placeholder, status reporting; **not** exercised end-to-end in the demo (see Known Limitations) |
| Policy enforcement | demo step 5 | `rejected` with rule `disallowed_category` |
| Audit read-back | demo step 6 | ledger records returned newest-first (count 20) |
| Secret scan | `git grep -nEi "(api[_-]?key|secret|private[_-]?key|token)\s*[:=]\s*['\"][A-Za-z0-9]"` | 0 committed credentials; `client/.env` is git-ignored |
| Judge-path check | isolated clone → `npm ci` → `typecheck` → `doctor` | green from a clean clone with only the documented variables |

## Repository Structure

```
contract/            Rust TEE contract (own crate, compiled to a WASM component)
  src/               lib.rs, policy.rs, fx.rs, ledger.rs, approval.rs, maps.rs,
                     capabilities.rs, errors.rs, test_support.rs   (~4,000 lines)
  wit/world.wit      exported `contracts` interface + host imports = capabilities
  wit/deps/          vendored host ABI packages (must match the cluster)
client/              Node 22 + TypeScript operator harness
  src/doctor.ts      21 preflight checks: env, SDK, wasm, egress hosts, acting identity
  src/deploy.ts      register the contract, create the maps, seed policy + secrets
  src/grant.ts       write the data owner's scoped delegation grant
  src/invoke.ts      the 8-step agent session; writes a transcript
  src/lib/           env, session, interface and artifact helpers
  .env.example       every variable, including the two traps in HANDOVER.md
  demo-output/       committed transcripts of the green runs
docs/                INTERFACE.md, ARCHITECTURE.md, RUNBOOK.md, BUGLOG.md,
                     HANDOVER.md, SUBMISSION.md, evidence/
tools/               terminal-to-png.py (capture → screenshot), build-submission-docx.py
AGENTS.md            repository contract for the next maintainer (mirrored to CLAUDE.md)
LICENSE              Apache-2.0
```

## Security / Privacy Model

Three credentials, three jobs, no shared blast radius:

| Identity | Credential | Authority |
|---|---|---|
| tenant | private key | registers the contract, creates the maps, seeds the policy |
| user (data owner) | private key | signs one scoped delegation grant |
| agent | opaque `t3n_key_…` bearer token | calls exactly six functions on exactly one contract, with egress to exactly two hosts |

The agent's credential is not a private key and never touches the session
handshake: it goes to the invoke endpoint in an `X-T3N-Api-Key` header, and the
contract's private material is generated inside the enclave and never leaves it.

**PII.** ExpenseGuard minimises direct PII exposure rather than eliminating PII
as a category: the contract receives an `employee_ref` and resolves approved
contact placeholders (`{{profile.*}}`) at the host/dispatch layer, so names and
email addresses are never stored in contract memory. That is a narrower and
exact claim — the outbound webhook request does carry the resolved contact
details, because a human approver has to be reachable.

**Audit ledger.** Records are keyed `seq:<6-digit>`, written once, never
rewritten, and read back through `get-audit`. It is an **append-only,
sequence-numbered ledger inside the TEE** — not an independently verifiable
cryptographic structure: there is no per-record signature, Merkle path or
receipt that a third party could check against the enclave without trusting the
operator's attestation. `docs/HANDOVER.md` lists a verifiable receipt as the
first production follow-up.

**Egress.** Each call names the identity it acts *for* (`pii_did`), and the
enclave resolves the contract's outbound-host allowlist from that identity's
grant. A call that names nobody is treated as a self call and is denied
(`BUG-14`).

## Current Implementation

Implemented and verified in this repository: T3N TEE deployment of a WASM/WIT
contract; scoped agent delegation over six functions and two hosts; deterministic
policy engine with stable rule ids; live FX conversion with a cached fallback;
duplicate detection over a configurable window; approval routing through the
host's placeholder-aware HTTP egress; an append-only audit ledger with read-back;
PII minimisation via `employee_ref` and dispatch-time placeholder resolution; a
21-check preflight and an 8-step end-to-end demo with committed transcripts.

## Production Follow-ups

Not implemented here, and not claimed: cryptographically verifiable audit
receipts (a per-decision digest a third party can verify independently of the
operator); atomic sequence allocation for concurrent writers; attestation
verification in the client rather than bare reachability; signed and versioned
policy documents; per-function egress scoping instead of one host list per
delegation row; multi-tenant policy management; external webhook secret rotation;
operational monitoring and alerting.

## Known Limitations

Each of these is true of the current code and is the honest answer to "what would
break in production?":

- **One policy per tenant.** The policy map holds a single `current` key, so a
  tenant runs one rulebook at a time; per-department policy sets need a keying
  change.
- **No atomic compare-and-set on the ledger.** Sequence numbers are derived by
  reading the highest key and incrementing, so two concurrent writers could pick
  the same number. Claims arrive serially per team in an approval workflow, which
  is why this is acceptable here and not in a high-throughput ledger.
- **Approval delivery depends on the configured webhook.** If the secrets row is
  missing or the approver endpoint is down, `request-approval` records
  `status: "failed"` with the HTTP code rather than retrying. The function is
  covered by 15 unit tests but is not exercised end to end in the committed demo,
  so its first live call against a real endpoint is genuinely untested.
- **FX depends on an external provider.** `open.er-api.com` is reachable from the
  enclave; when it is not, a non-expired cache entry is used and otherwise the
  verdict is forced to `needs_approval` with `fx_unavailable`.
- **The ledger is append-only but not independently cryptographically
  verifiable** (see the Security/Privacy Model section).
- **The agent holds a bearer token, not a key pair.** It cannot prove possession
  of a key to a third party; authority comes from the delegation document and the
  host's resolution of it.

## Bug / Developer-Experience Findings

Development surfaced 15 documentation, developer-experience and integration
defects in the T3N ADK, reproduced with file:line, verbatim errors and proposed
fixes in [`docs/BUGLOG.md`](docs/BUGLOG.md). They are categorised there by class
(documentation, developer experience, integration) — no vulnerability was found
in the platform while building this. The two that dominated the build were an
agent call that omits its acting identity being resolved as a self call and
denied with an empty allowlist (`BUG-14`), and the SDK's declared delegation row
(`function: string`) being rejected by the node, which accepts `functions: [...]`
(`BUG-15`).

## Handover

Nothing in the running system is bound to our accounts, so either path works —
we keep running it, or Terminal 3 takes it over. The transferable surface is a
wasm artifact, one registration call, four KV maps, one delegation document and
one secrets row. [`docs/HANDOVER.md`](docs/HANDOVER.md) is the full checklist:
provisioning order, credentials, preflight, the two traps worth reading before
the first call, and what we would do before running this for real.
[`docs/RUNBOOK.md`](docs/RUNBOOK.md) re-derives every operational step from the
repository alone.

## License

Apache-2.0 — see [`LICENSE`](LICENSE).
