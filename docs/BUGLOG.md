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

_(Remaining live-run defects are added below as the deployment run surfaces them —
see `docs/RUNBOOK.md`.)_

## How we'd prioritise the fixes

1. **BUG-09** — blocks the documented first command.
2. **BUG-01** — blocks the documented first paste.
3. **BUG-03** — costs the platform its differentiator: builders cannot discover
   `scan` or `set-claims-digest`, so nobody ships offline-verifiable receipts.
4. **BUG-02** — silent path to a non-loadable component.
5. **BUG-04, BUG-05** — tooling friction with no workaround.
6. **BUG-06, BUG-07, BUG-08** — vocabulary and naming drift.
