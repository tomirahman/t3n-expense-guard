# Bug log — defects found while building ExpenseGuard

Every entry below was reproduced on 2026-09-16 against the published artifacts
(docs site mirror, `Terminal-3/z-tenant-flight` at the commit we cloned, npm
registry, vendored WIT). Each entry gives the exact command or file:line, the
observed result, the impact on a first-time builder, and a suggested fix.

These are developer-experience and documentation defects — not security
vulnerabilities. We found no vulnerability in the ADK while building.

Artifacts referenced:

- `docs` = `docs.terminal3.io` mirror (49 pages, `developers/adk/**` and `t3n/**`)
- `ref` = `github.com/Terminal-3/z-tenant-flight` (the repo `write-contract.md` links as the reference)
- `abi` = `ref/wit/deps/host-interfaces-2.1.0/package.wit` (the ABI a contract links against)
- `sdk` = `@terminal3/t3n-sdk` on npm

---

## BUG-01 — the published `invoke-contract` sample does not compile

`docs/.../walkthrough/invoke-contract.md`

```
19:  fetchTrustedManifest,
25:  fetchTrustedManifest,
34:  trustAnchor: await fetchTrustedManifest("testnet"),
36:  trustAnchor,
```

The same import specifier is listed twice (lines 19 and 25), and the client
config object declares `trustAnchor` twice (lines 34 and 36). A builder who
pastes the sample as published gets a duplicate-identifier error and, on the
object literal, a duplicate-property error before ever reaching the network.

**Fix:** delete the second `fetchTrustedManifest,` import and the redundant
`trustAnchor` property. (The prose below the sample already says the anchor is
reused, so the second fetch is redundant as well.)

## BUG-02 — vendored ABI version in the docs contradicts the official reference repo

`docs/.../walkthrough/write-contract.md:42` tells builders to "vendor the
versions your target cluster provides (here, `host-interfaces-2.2.0/` and
`host-tenant-1.2.0/`)".

The official reference repo ships different versions:

```
ref/wit/deps/host-interfaces-2.1.0/
ref/wit/deps/host-tenant-1.0.0/
ref/wit/deps/host-outbox-1.0.0/
```

The only place the relationship is explained is an internal comment inside the
vendored ABI itself (`abi:1-18`): the canonical source of truth is `@2.2.0`,
while contracts stay pinned at `@2.1.0` because the host resolves a contract's
`@2.1.0` import against its `@2.1.0` surface, and bumping is done in lockstep
per-contract when opting into a new interface.

Because the host refuses a component that imports an interface its linker world
does not provide, a builder who follows the docs literally can ship a component
that will not instantiate — with no documented way to discover the linkable
version.

**Fix:** state the linkable version explicitly in the docs ("vendor
`host-interfaces-2.1.0/`; `2.2.0` is the canonical spec, opt in per-contract"),
or generate the sentence from the reference repo.

## BUG-03 — four shipped host capabilities are absent from the developer docs

Grep over the whole mirrored docs tree: `scan` → 0 hits, `claims-digest` → 0,
`get-balance` → 0, `seq-no` / `cluster-timestamp` → 0.

| capability | where it lives | what it does |
|---|---|---|
| `kv-store::scan(map, start, end, limit)` | `abi:592` | bounded half-open range scan, one-shot (no cursor) |
| `kv-store::set-claims-digest(digest)` | `abi:571` | 32-byte SHA-256 folded into the Merkle leaf "so clients can verify receipts offline" |
| `token::get-balance` | `abi:616` | read-only credit balance, exists specifically to warn before a metered flow traps on `OutOfCredit` |
| `tenant-context::seq-no`, `cluster-timestamp-secs` | `ref/wit/deps/host-tenant-1.0.0/package.wit:35,39` | host sequence number and cluster time for the current execution |

Impact is concrete, not cosmetic:

1. With `get`/`put`/`delete` as the only documented KV verbs, a contract cannot
   enumerate keys at all — an audit-trail or history read is impossible to
   implement from the docs alone. We discovered `scan` only by reading the
   vendored WIT.
2. `set-claims-digest` is the primitive that makes a decision offline-verifiable
   (the client can recompute the digest over the record and check it against the
   leaf). It is the single most differentiating capability for compliance use
   cases, and it is undocumented.
3. `get-balance` exists to make the credit trap self-serviceable, but the docs
   never mention it, so builders meet credits only as a failure.

**Fix:** publish an ABI reference page generated from the shipped `package.wit`
(the `reference.md:71` page already claims a generated reference "can't drift out
of sync" — the WIT surface currently has no such guarantee), and add one
walkthrough step using `scan` + `set-claims-digest`.

## BUG-04 — the OpenAPI spec the docs cite is not retrievable

`docs/.../reference.md:71` states the REST surface is documented via OpenAPI and
that the author verified it "directly by parsing `terminal-3-openapi.yml`
(21 paths, 24 operations, OpenAPI 3.0.3)".

Probed, all 404:

```
https://docs.terminal3.io/terminal-3-openapi.yml   404
https://docs.terminal3.io/terminal-3-openapi.yaml  404
https://docs.terminal3.io/openapi.json             404
https://terminal3.io/terminal-3-openapi.yml        404
```

**Impact:** no client codegen, no programmatic drift check, and the "can't drift"
claim is unverifiable from outside.

**Fix:** serve the spec at a stable URL and link it from `reference.md`.

## BUG-05 — quickstart pins an SDK 15 minor versions behind

```
docs/.../get-started/quickstart.md:29          npm install @terminal3/t3n-sdk@5.2.0 tsx
docs/.../support/ai-coding-assistants.md:57    npm install @terminal3/t3n-sdk@5.2.0 tsx
```

npm `latest` is `5.17.0` (138 published versions). The pinned version is what an
agent or a builder follows, so the first run uses an SDK released long before
the current docs were written; any renamed export surfaces as an unexplained
type error rather than a version warning.

**Fix:** pin `latest` (or a documented supported range) in the quickstart, and
keep the pinned value generated rather than hand-written.

## BUG-06 — two names for one delegation mechanism

`docs/.../changelog.md:68` records the rename
`agent-auth-update` / `agent-auth-get` / `agent-auth-revoke-org` →
`member-delegation-update` / `-get` / `-revoke-org`.

But the shipped ABI still exposes `interface agent-auth { update-authorisations }`
(`abi:141-157`) and describes outbound-placeholder access control as "the agent
delegation grant (`agent-auth-update`, scoped per-agent/per-contract/per-function)"
(`abi:54`). The developer docs use only the `member-delegation-*` vocabulary.

**Impact:** ambiguous which call actually gates `{{profile.*}}` egress. We had to
read the WIT to decide. A builder who assumes the rename is complete will look
for a call that no longer exists under that name, or vice versa.

**Fix:** one vocabulary across changelog, docs and WIT comments, with the legacy
name explicitly marked as removed.

## BUG-07 — the credit failure name differs between the SDK and the ABI

- SDK/docs: `InsufficientCreditError` (`invoke-contract.md:55`, `ai-coding-assistants.md:46`)
- ABI: "trap mid-execution on `OutOfCredit`" (`abi:603`)

Combined with BUG-03 (`token::get-balance` undocumented), a builder's first
credit problem appears as an unfamiliar error string with no documented way to
check a balance first.

**Fix:** one error name, and document `get-balance` next to it.

## BUG-08 — `host-outbox-1.0.0` is vendored but never used and never documented

`ref/wit/deps/host-outbox-1.0.0/package.wit` ships in the reference repo, is not
imported by `ref/wit/world.wit`, and is mentioned nowhere in the docs — while
`abi:581` refers to "reserved host-iface maps (outbox, etc.)". The concept is
real; the documentation is absent.

**Fix:** document the outbox capability or drop it from the vendored set.

## BUG-09 — `cargo test` as documented cannot run in the reference repo

`docs/.../walkthrough/test.md:9` says: "Test the business-logic guards on the
native target with `cargo test`."

Reproduced on a clean copy of `ref` (target dir excluded):

```
$ grep -A1 '\[build\]' .cargo/config.toml
[build]
target = "wasm32-wasip2"

$ cargo test
   ...
Caused by:
  could not execute process `/tmp/bare-test/target/wasm32-wasip2/debug/deps/z_tenant_flight-....wasm` (never executed)
Caused by:
  Permission denied (os error 13)
```

Because the repo pins the wasm target in `.cargo/config.toml`, a bare
`cargo test` builds a wasm test binary and tries to execute it. The documented
command fails on the documented starting repo.

**Fix:** ship the alias in the reference repo, e.g.

```toml
[alias]
test = "test --target x86_64-unknown-linux-gnu"
```

and mention it in `test.md` — with the caveat that host-calling bodies only run
under wasm.

---

## Bugs found during the live run

### BUG-10 — The SDK ships obfuscated, with no source map, while the repo is private

`@terminal3/t3n-sdk@5.17.0` is MIT-licensed (`package.json: "license": "MIT"`,
`LICENSE` present) but the published artefacts are obfuscated bundles.

```
$ find node_modules/@terminal3/t3n-sdk -name '*.map' | wc -l
0
$ du -sh node_modules/@terminal3/t3n-sdk/dist/*
2.0M    dist/cli
308K    dist/index.d.ts
1.9M    dist/index.esm.js
1.9M    dist/index.js
156K    dist/scripts
```

Every identifier in `dist/index.esm.js` is rewritten through a
`_0x...`-lookup indirection table, and no `.map` file is published, so a stack
trace is unreadable:

```
Error: No handler configured for guest-to-host request: EthSign.
    at T3nClient.handleGuestToHost (.../dist/index.esm.js:2:490764)
```

**Reproduction:** `npm i @terminal3/t3n-sdk` then trigger any SDK error; the
frame points at byte offset 490764 of a single 1.9 MB minified line.

**Why it matters here:** T3N sells verifiability — contracts run in an enclave
whose attestation you are asked to trust, and the client that talks to it is
the other half of that trust boundary. `github.com/Terminal-3/trinity` is
private (404 for anonymous requests), so the only form of the client code that
is public is obfuscated. A builder debugging an `EthSign` handler — the exact
error above — cannot read the code path that raised it. MIT's own terms assume
distribution in the preferred form for modification.

**Cheap fix, no source disclosure needed:** publish source maps
(`dist/index.esm.js.map`) as the rest of the ecosystem does. That restores
stack traces without exposing anything the obfuscator was hiding from a
casual reader — `dist/index.d.ts` (308 KB) already documents the whole public
API surface.

## BUG-11 — the numeric `contract_id` map ACLs require cannot be read back, and the SDK's example for the grant's target matches nothing

Two different identifiers point at the same contract, and the split is
documented — but only one half of it is:

- **Map ACLs take the numeric id.** `type WriterSet = "all" | { only: number[] }`
  (`dist/index.d.ts:6384`), and `register-contract.md:7` says registration
  "gives you a numeric `contract_id` that you use when creating map ACLs".
- **Grants and `delegation.check` take the canonical name.**
  `DelegationGrantRef.contract` is documented as "The target contract's
  canonical name" (`dist/index.d.ts:2892`) and the walkthrough passes
  `z:<tid>:<tail>` (`invoke-contract.md:94`).

The gap is that the numeric id is **write-only**. `ListedContract`
(`dist/index.d.ts:2846-2864`) exposes `name`, `short_name`, `kind`, `version`,
`summary`, `tags` and `owner_org_did` — and no `contract_id`. `contracts.list`
is therefore not a way to recover the id, and `register-contract.md:83` confirms
it: "there is currently no API to fetch a tail's current `contract_id` after
re-registering". The docs' mitigation is procedural — keep your own record — but
the node already hands you the id at registration, so it plainly has it.

The CLI does not close the gap, it widens it: `t3n contract get` exists, but it
labels the canonical *name* with the field name the ACLs use for the number:

```
$ t3n contract get z:f817f49837375d99b44cbf8907becc154f2a5fc9:expense-guard --env testnet
contract_id      z:f817f49837375d99b44cbf8907becc154f2a5fc9:expense-guard
current_version  0.1.0
```

(It also rejects the short tail — `t3n contract get expense-guard` → `expense-guard
is not registered`.) So the only two identifiers are still the string and the
number, the command for reading a contract prints the string under the number's
name, and the number remains write-only.

The failure is quiet and misattributed. Re-register the same tail, keep the old
ACLs, and the contract's own `kv-store` call is denied *inside the enclave* —
`AccessDenied` is the documented symptom of an ACL miss (`create-kv-maps.md:18`):

```
AccessDenied
```

Nothing in that error points at the client's bookkeeping, so the natural next
step is to debug the contract, its WIT imports and its ACL entry — all three of
which are correct. The stale id is the only wrong thing in the picture.

Separately, the one example the SDK ships for the grant's target is a form that
cannot be produced by any documented step:

```ts
// dist/index.d.ts:1215 — the delegation grant's target
/** Target contract id (canonical name, e.g. `"tee:z-payroll/contracts"`), or `"*"`. */
contract_id: string;
```

Core contracts are `tee:<name>` (`dist/index.d.ts:2847`) — the system contract in
the walkthrough is `tee:user/contracts` — and a tenant contract with tail
`payroll` is `z:<tid>:payroll`. `tee:z-payroll/contracts` is none of those, and a
builder who trusts the type comment over the walkthrough writes a grant aimed at
a contract that does not exist; `delegation.check` reports no coverage and the
denial only appears at invoke time.

**Fix:** add `contract_id` to `ListedContract` (one field, the node already has
it), and correct the JSDoc example. If the two-identifier split is deliberate,
say so in one sentence next to `WriterSet` — the type declarations are the most
authoritative thing a builder has, and right now they are the only place the
question gets answered at all.

## BUG-12 — `--initial-credits` fails on a fresh organisation with `available=0`, and names no source

Provisioning an org agent with the CLI flag that the help text advertises:

```
$ t3n org create --name "ExpenseGuard" --env testnet --json
{ "organisationDid": "did:t3n:85c188ff…", "name": "ExpenseGuard" }

$ t3n agent create --org did:t3n:85c188ff… --name "expense-guard" \
    --initial-credits 2000000000 --env testnet --json
Error: agent-credit-transfer: insufficient credit (available=0, requested=2000000000)
```

The calling identity's own balance at that moment was **20,000,000,000**
(`t3n token balance`), so the flag is not drawing on the caller — it draws on a
balance that is empty on a brand-new organisation and that no documented command
can fund. `--initial-credits` appears in the CLI help and in **none** of the 48
documented pages; the documented funding command is explicit that it does the
thing the flag appears to promise:

```
agent fund --agent <did> --amount <n> [--note <text>] [--from-org <did>]
                       fund an agent's credit balance so it can act;
                       defaults to your own balance          (write)
```

**Reproduction:** `t3n agent create --org <fresh-org> --name x --initial-credits 1`.
The call is atomic — the outer transaction is rolled back, no orphan agent is
left behind — so the damage is only the confusing error.

**Workaround, and the path we shipped on:** omit the flag, then
`t3n agent fund --agent <did> --amount 2000000000` from the caller's balance.

**Fix:** either make the flag fund from the caller's balance (matching `agent
fund`), or drop it and tell the user to call `agent fund` — and in both cases say
*whose* balance is being debited in the error message.

## BUG-13 — the CLI cannot read an agent's credit balance, only a private key's

`token balance` is the command you reach for when a metered call fails, and it
accepts a private key only:

```
$ T3N_API_KEY=t3n_key_…9b52 t3n token balance --env testnet
error: Invalid Ethereum private key (INVALID_ARGUMENT): t3n_…9b52 [redacted]
```

An agent's credential *is* a `t3n_key_…` bearer token — the platform issues no
private key for it — and agent calls are metered against the agent's own balance,
separately from the tenant's. So the one identity whose balance you can be
blocked by is the one identity the CLI will not look up. The only feedback on an
agent's balance is the 403 at call time:

```
HTTP 403 {"error":"InsufficientCredit (account=a13591f5…, required=10000000000,
available=0)","code":"forbidden","request_id":"…"}
```

`required=10000000000` here is a **reserve**, not the price of the call: our agent
was funded with 2e9, served several calls, then began failing with
`available=0`; after funding 1e10 the same call succeeded. Nothing in the docs,
the help text or the error explains that distinction, and a *read* of the audit
ledger is not obviously the most expensive thing in it.

**Fix:** let the CLI read a keyed identity's balance from its bearer token, or
accept `--agent <did>` on `token balance` for an org admin. Either one turns a
runtime 403 into a preflight check.

## BUG-14 — a call that omits `pii_did` is resolved as a self call, and the denial names neither the identity nor the grant

This is the one that cost us the most time, because we diagnosed it backwards
first: every signal pointed at the delegation document, and the document was
correct the whole time.

What the walkthrough tells a builder to do — and what we did — is to write one row
on the delegator's own delegation document naming the agent as grantee, listing the
contract's functions and the hosts the contract may dial:

```json
{ "grantee": "did:t3n:a13591f5…",
  "contract_id": "z:f817f498…:expense-guard",
  "functions": ["health", "set-policy", "get-policy", "check-expense",
                "request-approval", "get-audit"],
  "allowed_hosts": ["open.er-api.com", "postman-echo.com"] }
```

The write succeeds and `member-delegation-get` echoes the row back, `allowed_hosts`
included. The agent's first call to a contract that dials out then fails with:

```
fx: live lookup failed: host/http.egress_denied: egress host(s)
    [open.er-api.com] not in the resolved allowlist []
```

The allowlist is empty although the document granting it is stored on the node and
reads back intact. `allowed_hosts` is resolved **per call, for the identity the call
acts for**, and an `invoke` request without `pii_did` is a *self* call: the acting
identity is the agent, the agent's own delegation document grants nothing, so the
resolver has no host to read. Set `pii_did` to the delegator and the same call, the
same document and the same contract resolve the host:

```
{"expense_id":"EXP-…","verdict":"compliant","base_amount":48.46,
 "fx_rate":1.153788,"fx_source":"open.er-api.com","rule_hits":[]}
```

The SDK already ships the check that would have found this in one call —
`discoverCheckDelegation({ contract, pii_did, functions })` — and for the omitted
`pii_did` it answers:

```
{"authorised":false,"disclosed":false,"satisfied":[],"missing":[]}
```

Both `satisfied` and `missing` are empty, so the field whose name promises a
diagnosis ("reports which edge is missing for a given action", per the SDK's own
documentation) carries none. With `pii_did` set it becomes `authorised: true` with
the satisfied edge listed. An empty `missing` array beside `authorised: false` is
the least actionable answer the API could give, and it points the reader at the
grant document — which is exactly the wrong place.

**Fix:** have the denial name the acting identity and the grants that were
considered, e.g. *"no grant for the acting identity did:t3n:a13591f5…; grants for
this grantee exist on did:t3n:f817f498… — pass `pii_did` to act for it"*. Failing
that, populate `missing` in `discoverCheckDelegation`: a builder following the
walkthrough has no other way to tell a self call from a delegated one. Two related
traps belong in the same report: the node stores the document without validating its
shape, so a read-back proves only that the node remembers what it was given — it is
not a verification step, and a client that treats it as one will conclude that a
correct document is broken (as ours did); and the row shape itself is a red
herring, which we confirmed by writing both the single-row and the six-row forms and
calling the contract after each — both resolve, `pii_did` is the only variable that
changed the outcome.

## BUG-15 — the SDK's declared grant row is rejected by the node, and the type is the only machine-readable spec

`BoundGrant` in `@terminal3/t3n-sdk` declares:

```ts
/** One function per grant: a client authorising a grantee on several functions
 *  posts several rows, each carrying the full `scopes` that function needs —
 *  there is no more multi-function fan-out on the wire. */
function: string;
```

Writing exactly that row fails at the node:

```
RPC Error: Invalid delegation document: unknown field `function`, expected one of
`grantee`, `contract_id`, `version_req`, `functions`, `scopes`, `read_scopes`,
`allowed_hosts`, `window` at line 1 column 840
```

So the accepted vocabulary is `functions: string[]` — the shape the SDK's own
comment describes as retired — while the declared per-function field cannot be
serialised at all. Per-function granularity is still reachable on the wire (six
single-element rows, each carrying its own `allowed_hosts`, are accepted and
resolve), but a builder who trusts the type hits a hard error at the last step of
the walkthrough, and the error lists the fields the docs do use, which reads as
though the docs are the broken part.

**Fix:** make the declared type match the wire (`functions: string[]`), or accept
both names, and say in one place which field the resolver keys on. The least
privilege consequence is worth stating too: the documented single row carries one
`allowed_hosts` list for all six functions, so an agent granted only `check-expense`
inherits the webhook host as well — per-function egress scoping needs the
single-element form the type cannot express.

## How we'd prioritise the fixes

1. **BUG-09** — blocks the documented first command.
2. **BUG-01** — blocks the documented first paste.
3. **BUG-14** — a grant that is present, accepted and echoed back, then not
   applied because the call named no acting identity: every signal says success and
   the failure lands inside the enclave as an egress denial. Cheapest to fix,
   dearest to diagnose.
4. **BUG-02** — silent path to a non-loadable component.
5. **BUG-11** — the numeric id that map ACLs need cannot be read back, so
   re-registering a tail silently orphans every ACL that used the old id, and the
   symptom (`AccessDenied`) points the builder at the wrong layer.
6. **BUG-03** — costs the platform its differentiator: builders cannot discover
   `scan` or `set-claims-digest`, so nobody ships offline-verifiable receipts.
7. **BUG-10** — the client that talks to the enclave is published obfuscated,
   with no source maps, while the source repo is private; that is the opposite of
   the verifiability T3N sells.
8. **BUG-04, BUG-05** — tooling friction with no workaround.
9. **BUG-15** — the declared grant row is rejected by the node: the type and the
   wire disagree, and the type is the only machine-readable one.
10. **BUG-06, BUG-07, BUG-08, BUG-12, BUG-13** — vocabulary drift, one
   undocumented and mis-sourced CLI flag, and a balance the CLI cannot look up.
