# Runbook — deploying and demoing ExpenseGuard

Everything needed to take this repo from a clone to a live demo on T3N testnet.
Fifteen minutes end to end, most of it `npm install`.

## 0. Prerequisites

| tool | version we used | note |
| --- | --- | --- |
| Node.js | 22.23.1 | ESM; the SDK needs >= 20 |
| Rust / cargo | 1.98.0 | `rustup target add wasm32-wasip2` |
| `wasm-tools` | any recent | only for inspecting the component's ABI |

## 1. The three identities

The platform meters and authorises three separate credentials, and this project
uses all three for what they are for:

| role | credential | authority | how to get one |
| --- | --- | --- | --- |
| **tenant** | private key (`0x…`) | registers the contract, creates the KV maps, seeds the policy and the secrets row | the campaign claim page |
| **user** (data owner) | private key (`0x…`) | signs the member-delegation grant the agent runs under | a second key; one account can hold several |
| **agent** | opaque token (`t3n_key_<id>.<secret>`) | calls six functions on one contract, with egress to two hosts | `t3n agent create`, then `t3n agent fund` |

Two things we learned the hard way, both worth knowing before you start:

- **The claim page does not mint a second identity.** Visiting it again with the
  same account returns credentials for the *same* DID — we proved this by
  authenticating two different private keys and getting an identical DID back.
  A DID is per account, not per key. If you want a distinct agent identity, mint
  it through the CLI (below) rather than by re-claiming.
- **An agent needs its own balance.** An agent DID starts at zero and its calls
  are metered separately from the tenant's. Provision it, then fund it:

  ```bash
  t3n org create --name "ExpenseGuard" --env testnet --json                     # → organisationDid
  t3n agent create --org "$ORG_DID" --name expense-guard --env testnet --json  # → agentDid + token (shown once)
  t3n agent fund --agent "$AGENT_DID" --amount 2000000000 --note "initial funding" --env testnet
  ```

  Afterwards, `t3n contract get <canonical-name> --env testnet` reports the
  registered version — note that it prints the canonical *name* under the label
  `contract_id`, which is exactly the confusion `BUG-11` describes.

  Do **not** pass `--initial-credits` to `agent create`: it draws on the
  organisation's pool, which is empty, and fails with an error that names no
  source (`BUG-12`). `agent create` is not idempotent either — it registers a DID
  each time, and the agent's bearer token is printed exactly once.

`t3n agent create` prints the bearer token once; capture it straight into `.env`
without echoing it to a terminal log, and never commit it.

## 2. Configure

```bash
cd client
cp .env.example .env && chmod 600 .env
```

```dotenv
T3N_ENVIRONMENT=testnet
TENANT_API_KEY=0x...            # tenant private key
TENANT_DID=did:t3n:...          # read back from a session, never constructed
PII_DID=did:t3n:...             # the identity the grant belongs to — see below
AGENT_INVOKE_KEY=t3n_key_...    # bearer token, printed once at agent create
AGENT_KEY_ID=t3n_key_...
AGENT_DID=did:t3n:...
ORG_DID=did:t3n:...
CONTRACT_TAIL=expense-guard
CONTRACT_VERSION=0.1.0
FX_BASE_CURRENCY=USD
APPROVAL_WEBHOOK_URL=https://postman-echo.com/post
```

`USER_API_KEY` (a distinct data-owner key) and `AGENT_API_KEY` (an Ethereum key for
the agent) are both optional and both unset in the deployment above: a DID is per
account, so a second key for the same account returns the same DID and adds no
isolation, and an org agent's only credential is its bearer token. `PII_DID` is not
optional — see step 4.

`npm run doctor` then checks 21 things — node and SDK versions, the SDK's
exported surface, the built wasm and its fingerprint, the contract tail and
version against `docs/INTERFACE.md`, both reachable hosts, and the credentials.
One of them is the delegated-call identity: `PII_DID` missing is a setup that
authenticates cleanly and then fails inside the enclave (BUG-14), so it is worth
catching before the first call.
Its registration-record check is the one to watch: it compares the wasm on disk
with the one that was registered, so it catches "you rebuilt and forgot to
re-register".

## 3. Build and test the contract

```bash
cd contract
cargo test                                        # 115 native tests + 1 doctest
cargo build --target wasm32-wasip2 --release      # → 255,734-byte component
wasm-tools validate target/wasm32-wasip2/release/z_expense_guard.wasm
```

Business logic is tested natively, so the suite runs in under a second without a
cluster. `cargo test` works here because our `.cargo/config.toml` keeps the
default target on the host triple and puts the component target behind the `wasm`
alias; the vendored sample pins `wasm32-wasip2` as the default instead, which
makes its own `cargo test` try to run a `.wasm` binary (`BUG-09`).

## 4. Run the flow

Each step depends on the one before it, and each authenticates as a different
identity — run them in order:

```bash
cd client
npm run register    # tenant: register the wasm, create 4 maps, seed the policy
npm run delegate    # data owner: grant the agent 6 functions on 1 contract, 2 hosts
npm run demo        # agent: the expense flow; writes a transcript
```

**`register` is not idempotent.** It allocates a new numeric `contract_id` on
every call, and re-registering the same version is refused outright
("not higher than current"). That numeric id is what the KV map ACLs take, and
there is no API to read it back afterwards (`BUG-11`), so `register` persists it
to `client/artifacts/registration.json` and the later steps read it from there.

`delegate` reads the delegation document first, keeps every row it does not own
(the profile-scope grant on `tee:user/contracts` included), merges in its own row
and posts the result — `member-delegation-update` replaces the whole document, so a
blind write drops the rest. `allowed_hosts` is derived from `APPROVAL_WEBHOOK_URL`
plus the FX host, and the step refuses to write a grant that does not cover what
the contract will actually call.

`demo` refuses to start without `PII_DID`, for the reason step 2 lists: an agent
invoke that names no identity is a self call, the agent holds no grant of its own,
and every outbound call is denied with an empty allowlist while the grant sits
correctly on the delegator's document (`BUG-14`).

## 5. Read the results

- **Verdicts** come back as JSON from `check-expense`: `verdict`, `rules[]`,
  `base_amount`, `fx_rate`, `fx_source`, `ledger_seq`. `rules` carries stable ids
  (`over_category_limit`, `possible_duplicate`, `fx_unavailable`), never prose.
- **Ledger** — `get-audit` returns the enclave-side records in host sequence
  order; `get-audit` with `{"employee_ref": "EMP-7"}` filters to one employee.
- **Approval round trip** — `request-approval` posts to the echo endpoint in the
  secrets map and returns the body the endpoint echoed back, so the
  `{{profile.*}}` substitution is verifiable without a third-party dashboard.
- **Transcript** — `npm run demo` writes `client/demo-output/<timestamp>-invoke.json`
  containing every call, its raw response and its classification. That file, not
  a screenshot, is the primary evidence; `tools/terminal-to-png.py` renders a
  real terminal capture into an image for the write-up:

  ```bash
  script -q -c "npm run demo" /tmp/demo.txt
  python3 tools/terminal-to-png.py --input /tmp/demo.txt --output docs/evidence/demo.png \
      --title "npm run demo — agent session against z:<tid>:expense-guard"
  ```

## Failure modes we actually hit

**`<NAME> is not set` from doctor.** `.env` is missing or still holds the
placeholder. Doctor stops at the first failure and names it.

**`contract version is not higher than`** on re-register. Bump
`CONTRACT_VERSION`, or skip registration and reuse the recorded `contract_id`.

**`fx_unavailable` in a verdict, or `host/http.egress_denied` from the host.**
The enclave could not dial `open.er-api.com`. Two causes, and the second is the
one that costs an afternoon:

1. the grant's `allowed_hosts` do not cover the host; or
2. the call named no `pii_did`, so the acting identity was the agent, which holds
   no grant of its own. The grant on the delegator's document is correct and reads
   back intact, and the resolved allowlist is still empty. That is `BUG-14`, and it
   is why `doctor` and `demo` both refuse to run without `PII_DID`.

Either way the contract is right to refuse rather than invent a rate: the verdict
carries `fx_unavailable`, `base_amount` is `null`, and the rule id says why.

**`AccessDenied` on a KV read or write.** The map's ACL does not include the
contract's current numeric id. If you re-registered without updating the ACLs,
that is `BUG-11`.

**`InsufficientCreditError` as the agent.** The agent DID's balance is separate
from the tenant's. `t3n agent fund` it; do not reuse the tenant key.

**Component rejected at registration.** The WIT import set drifted from the host
ABI. Compare with `wasm-tools component wit` against a known-good component.

## Re-running and tearing down

Nothing is mutated in place except the tenant's namespace, so a clean re-run is
either (a) register the same tail with a higher `CONTRACT_VERSION` and re-run
`delegate` + `demo`, which is what we did while iterating, or (b) start with a
fresh org and agent (`t3n org create` / `t3n agent create`) and point `.env` at
them. Maps are scoped to the tenant DID; delete the four `z:<tid>:*` maps when
you are done with them.
