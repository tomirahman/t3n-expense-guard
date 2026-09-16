# Runbook — deploying and demoing ExpenseGuard

Everything needed to take this repo from clone to a live, verifiable demo on the
T3N testnet. Ten to fifteen minutes end to end.

## 0. What you need

Three T3N identities, because the platform meters them separately:

| role | what it is | why it needs its own key |
|---|---|---|
| tenant | owns the contract, the KV maps and the policy | registration is billed to the tenant |
| agent | the identity that actually calls the contract | an agent DID's balance starts at zero and is separate from the tenant's — reusing the tenant key fails metered calls with `InsufficientCreditError` |
| user | stands in for the employee submitting a claim | the contract signs and resolves `{{profile.*}}` for the *calling* user |

Get all three from the claim page (self-serve, no approval; revisiting mints a
fresh key + credits each time). If a claim site asks for a campaign code, use the
one your challenge or event gave you.

Put them in `client/.env`:

```dotenv
T3N_ENVIRONMENT=testnet
TENANT_API_KEY=...
AGENT_API_KEY=...
USER_API_KEY=...
```

`client/.env` is git-ignored. Keys are shown once by the claim page — store them
before closing the tab.

## 1. Build the contract

```bash
cd contract
cargo build --target wasm32-wasip2 --release
ls -l target/wasm32-wasip2/release/z_expense_guard.wasm
```

Business-logic tests run natively, without a cluster:

```bash
cd contract
cargo test --target x86_64-unknown-linux-gnu
```

Use the explicit `--target`, not a bare `cargo test`: `.cargo/config.toml` pins
`wasm32-wasip2` so the contract links against the host ABI by default, and a bare
`cargo test` then tries to execute a `.wasm` test binary. See `docs/BUGLOG.md`
BUG-09.

## 2. Install the harness

```bash
cd client
npm ci
cp .env.example .env   # then fill in the three keys
npm run typecheck
```

## 3. Run the whole flow

```bash
cd client
npm run demo
```

The demo is idempotent per step and prints exactly what it did:

1. connects the tenant session and prints the tenant DID;
2. registers the contract from `contract/target/wasm32-wasip2/release/` and
   prints `contract_id`;
3. creates the three KV maps and grants the contract read/write ACL on each;
4. connects the user session and records their DID;
5. grants the user access to the contract functions (including
   `check-expense`) and the agent its scope;
6. seeds the approval webhook URL into the secrets map;
7. submits four claims as the user: one compliant, one over the category limit,
   one disallowed category, one duplicate of the first;
8. reads the audit trail back and prints the ledger;
9. prints the recomputed decision digest for the first claim so a third party
   can check it against the receipt.

Individual steps are also runnable on their own:

```bash
npm run register      # 2 only
npm run grants        # 4-5 only
npm run demo:claims   # 7-9 only
```

## 4. Read the results

- **Verdicts** come back as JSON from `check-expense`.
- **Ledger**: `get-audit` with `{ "employee_ref": "EMP-7" }` returns the entries
  in host sequence order.
- **Policy**: `get-policy` returns the active policy document.
- **Approval traffic**: the demo posts to the echo endpoint configured in the
  secrets map and stores the response code in the ledger entry, so the approval
  round trip is auditable without a third-party dashboard.

## Troubleshooting

**`InsufficientCreditError` on any agent call.** The agent identity has no
credits. Claim a fresh key for the agent and update `AGENT_API_KEY`. Do not reuse
the tenant key.

**The contract registers but every call fails to instantiate.** The cluster's
linker world does not provide an interface the wasm imports. Check that the
`host:interfaces/...@X.Y.Z` imports in `contract/wit/world.wit` match the
versions under `contract/wit/deps/`. See `docs/BUGLOG.md` BUG-02.

**Egress to the FX or approval host is denied.** Outbound HTTP is gated by the
contract's allow-list. Add the host to the grant that the demo configures, and
keep the hosts in `client/src/config.ts` and the grant in sync.

**`PlaceholderUnknown` on the approval post.** The calling user's profile is
missing a field the contract asked for. Either have the user fill that profile
field, or run the claim with approval posting disabled (`use_profile: false`) —
the contract will then post the non-PII payload only.

**`cargo test` fails with "could not execute process ... wasm".** Pass
`--target x86_64-unknown-linux-gnu` (see step 1).

## Tearing down

Maps and contracts are scoped to the tenant DID. Re-registering the contract
creates a new `contract_id` and the old ACL entries go stale — the demo writes
the current id to `client/.state.json` so re-runs reuse it instead of orphaning
grants. See `docs/BUGLOG.md` (contract-id retrieval) for the platform-side gap.
