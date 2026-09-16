# ExpenseGuard client — the control plane

The Node side of ExpenseGuard. It authenticates the three identities, registers
the TEE contract, provisions its KV maps, hands the agent a scoped delegation,
and then drives the contract as that agent.

The contract itself is in `../contract/`; the frozen interface it implements is
in `../docs/INTERFACE.md`. Read that first if you are changing anything here.

## The three identities

Every step is performed by a different credential, on purpose — the agent never
holds the tenant's key, and the tenant never holds the agent's:

| Identity | Credential | Used by | Authority |
| --- | --- | --- | --- |
| `tenant` | private key (`TENANT_API_KEY`) | `register`, `delegate` | registers the contract, creates the maps, seeds the policy and secrets |
| `user` (data owner) | private key (`USER_API_KEY`) | `delegate` | signs the member-delegation grant the agent runs under |
| `agent` | opaque bearer token (`AGENT_INVOKE_KEY`) | `demo` | calls six functions on one contract, with egress to two hosts |

Two of these are *session* identities: the SDK opens a WASM session, authenticates
and signs with the private key (`src/lib/session.ts`). The third is not — the
agent's credential is an opaque `t3n_key_…` token that the SDK relays verbatim in
an `X-T3N-Api-Key` header (`invoke()`), with no handshake and no key material on
disk. That asymmetry is the platform's, not ours, and it is why the agent cannot
reuse the session helper.

## Setup

```bash
cd client
npm install
cp .env.example .env && chmod 600 .env   # then fill in the three credentials
npm run doctor                            # 19 checks: env, sdk, wasm artifact, reachability
```

`doctor` is the gate. It verifies the SDK version and its exported surface, that
the wasm matches the registered fingerprint, that the contract tail and version
agree with `INTERFACE.md`, and that both the cluster and the FX host answer.

## Run order

```bash
npm run register    # 1. register the wasm, create the four maps, seed the policy
npm run delegate    # 2. the data owner grants the agent six functions on one contract
npm run demo        # 3. the agent runs the whole expense flow; writes a transcript
```

Run them in order — each one depends on the previous. `register` is **not**
idempotent: it allocates a new numeric `contract_id`, so a re-registration
orphans every map ACL that referenced the old one (this is `BUG-11` in
`../docs/BUGLOG.md`, and the reason `register` persists
`artifacts/registration.json` for the later steps to read).

### What each step proves

- **`register`** → the WASM component loads in an enclave, the four namespaced
  maps exist, and the policy is in the enclave's KV store.
- **`delegate`** → the grant is scoped to one contract, six functions and two
  hosts; the script prints the resolved grant and rejects a webhook host that
  disagrees with the secrets row.
- **`demo`** → the agent authenticates with its bearer token, the contract
  evaluates five crafted expenses, an approval request goes out with placeholders
  resolved inside the enclave, and the audit trail is read back. The transcript
  lands in `demo-output/<timestamp>-invoke.json`.

## Expected output

`npm run demo` ends with a per-step table; the transcript is the same data:

```
[PASS]     0 health                    {"ok":true,...}
           0b delegation              authorised=true satisfied=…
[PASS]     1 get-policy                categories=3
[PASS]     2 check-expense EXP-1042    verdict=needs_approval base_amount=… USD fx_rate=…
```

Steps are classified `PASS` / `MISMATCH` against the verdicts the policy should
produce — a `MISMATCH` is information, not a crash: it means the contract
answered differently from the expectation encoded in the demo.

## Troubleshooting

- **`TENANT_API_KEY is not set`** — `.env` is missing or still holds placeholders;
  `doctor` reports this first and stops.
- **`contract version is not higher than`** on re-register — the tail already has
  that version. Bump `CONTRACT_VERSION`, or use the recorded `contract_id` from
  `artifacts/registration.json` instead of re-registering.
- **`fx_unavailable` in a verdict** — the enclave's HTTP egress for
  `open.er-api.com` is not granted, or the host is down. The check is in the
  grant's `allowed_hosts`; `npm run delegate` prints what it wrote.
- **`AccessDenied` from inside the contract** — the map ACL does not include the
  contract's current numeric id. See `BUG-11`: re-registering changes that id.
- **`MISMATCH 0b delegation`** — the grant is missing or scoped to the wrong
  contract, so the agent can call but nothing is authorised. Re-run `delegate`.

## Layout

```
src/doctor.ts    preflight: environment, sdk, wasm, reachability
src/deploy.ts    register + create maps + seed policy  (npm run register)
src/grant.ts     member-delegation grant for the agent (npm run delegate)
src/invoke.ts    the agent session and the demo        (npm run demo)
src/lib/         env, session, interface constants, artifact records
```
