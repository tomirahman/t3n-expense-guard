# Handover — running ExpenseGuard after the challenge

The submission asks one question we would rather answer with a checklist than a
paragraph: do we keep running this, or does Terminal 3 take it over? Either
works, because nothing in the deployment is bound to our accounts.

## What the running system actually is

| piece | where it lives | how a new operator gets it |
| --- | --- | --- |
| contract | T3N testnet, `z:<tid>:expense-guard` v0.1.0, wasm 255,734 B | the wasm artifact in this repo plus one `tenant.contracts.register` call (`npm run register`) |
| four KV maps | `z:<tid>:{policy,audit,fx,secrets}`, ACL'd to the contract's numeric id | recreated by `npm run register` |
| policy | `z:<tid>:policy` | seeded by `npm run register` (idempotent), reseeded by `npm run seed` |
| delegation grant | the data owner's delegation document, not ours | one `npm run delegate` run, idempotent by design |
| secrets row | `z:<tid>:secrets` (webhook URL) | `npm run seed` |
| harness | `client/` in this repo | `npm ci` plus the values listed in `client/.env.example` |
| credits | tenant balance (deploys, map writes) and agent balance (calls) | `t3n agent fund --agent <did> --amount <n>` |

There is no hosted service, no private registry and no external config store. The
contract is a wasm artifact; the client is a TypeScript project; the credentials
are three T3N keys.

## Option A — we keep running it

The tenant org, the agent and the registration stay ours. Operating cost is
metered per call through the host's `token` capability, so a production budget is
a multiplication rather than a platform fee. What that requires from us is a
funded agent balance and a rotation cadence for three keys — nothing else here
decays: `RUNBOOK.md` re-derives every operational step from the repository alone.

## Option B — hand it to Terminal 3

Order matters; this is the sequence that works on a clean tenant.

1. **Provision.** A tenant with test credits, an org, and an agent
   (`t3n org create` → `t3n agent create` → `t3n agent fund`). Do not pass
   `--initial-credits` to `agent create` on a fresh org — it debits an empty
   balance and fails (`BUG-12`).
2. **Credentials.** Three keys — tenant, agent bearer token, data owner —
   written to `client/.env` from `client/.env.example`. The agent token is
   printed exactly once at creation; if it is lost, the agent has to be recreated.
3. **Environment.** Node 22+, Rust 1.98+ with the `wasm32-wasip2` target.
4. **Preflight.** `npm run doctor` — 21 checks across env, SDK, wasm, egress
   hosts and the acting identity. It names every missing value rather than
   failing at the first one.
5. **Deploy.** `npm run register` (contract, maps, policy, secrets row), then
   `npm run delegate` (the grant), then `npm run demo` (8 checks, writes a
   transcript). Re-running any of the three is safe.
6. **Verify.** `npm run demo` green is the acceptance test; the transcript lands
   in `client/demo-output/`.

A second operator can be sandboxed against the same repo: register a new tail
(`expense-guard-handover`), point `TENANT_API_KEY` at the new tenant, and the two
instances share nothing but code.

## Two traps worth reading before the first call

- **`PII_DID` must name the delegator.** Egress hosts are resolved per call from
  the identity the call acts *for*. A call that names nobody is a self call, the
  agent holds no grant of its own, and the enclave denies the contract's outbound
  HTTP with an empty allowlist while the grant sits intact on the delegator's
  document. Full write-up in `BUGLOG.md` (`BUG-14`).
- **Credit reserves are not prices.** A metered call reserves an order of
  magnitude more than it charges, so a "funded" agent can fail on `available=0`.
  `t3n token balance` will not read an agent's balance — it takes a private key
  only (`BUG-13`) — so watch the 403 body.

## What we would do next in production

1. **Verify attestation, not just reachability.** Nothing in the client checks
   the enclave quote today; a compliance deployment should refuse to run against
   an unexpected measurement.
2. **Version and sign the policy.** `set-policy` is a single writer today. In
   production the policy should be a signed document whose hash lands in the same
   audit record as the decision it produced.
3. **Per-function egress scoping.** One delegation row currently carries one
   `allowed_hosts` list for all six functions, so a function that dials nothing
   inherits the webhook host (`BUG-15`). The single-element row form fixes it
   once the SDK type can express it.
4. **Move the webhook secret out of the contract's KV row.** The secrets row is
   private to the contract, which is the right boundary for a demo and the wrong
   one for a rotation policy.
5. **Upstream first:** `BUG-09`, `BUG-01`, `BUG-14`, `BUG-02`, `BUG-11` — in that
   order, per the prioritisation at the end of `BUGLOG.md`. Each one blocks or
   mis-directs a first-time builder.
