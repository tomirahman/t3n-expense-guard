# Architecture and threat model

## Trust boundaries

| boundary | crosses it | notes |
|---|---|---|
| employee → agent | claim JSON with an opaque `employee_ref` | no name, no email, no card data |
| agent → enclave | authenticated session (agent DID) | the agent is a separate identity with its own key and credits |
| enclave → KV maps | host-mediated, prefix-enforced | maps are `z:<tid>:*`; the contract can only touch its tenant's namespace |
| enclave → internet | `http` / `http-with-placeholders` | each call is authorised from the calling user's grant, per call |
| enclave → audit ledger | host-mediated | append-only by convention, sequence-numbered |

The operator of the node is outside the trust boundary for policy contents and
claim data: both live in tenant-scoped KV and inside enclave memory.

## Data flow for one claim

1. The agent calls `check-expense` with
   `{expense_id, employee_ref, amount, currency, category, vendor, incurred_on}`.
2. The contract loads the policy from `z:<tid>:policy` (`current`). If nothing
   is stored it uses the built-in default policy and announces that in the
   response, so no caller is ever silently evaluated against default rules.
3. FX conversion: the cache `fx:<FROM>:<TO>` in `z:<tid>:fx` is consulted first.
   On a miss or expiry (`fx_cache_ttl_hours`) the contract calls
   `https://open.er-api.com/v6/latest/<FROM>` — a keyless, PII-free endpoint —
   and caches the rate with the cluster timestamp.
4. Duplicate detection reads `dup:<employee_ref>:<amount+currency+vendor hash>`
   and compares the recorded cluster timestamp against `duplicate_window_days`.
5. Rules are evaluated, the worst verdict wins (`rejected` > `needs_approval` >
   `compliant`), and a record is appended to `z:<tid>:audit` under the next
   `seq:<000001>` key, plus a pointer at `exp:<expense_id>`.
6. The response returns the verdict, the rule ids that fired, the FX rate and
   the ledger sequence number.

## Why PII never enters the contract

`request-approval` builds the outbound approval body with marker strings:

```json
{ "text": "Expense EXP-1042 flagged for approval",
  "employee": "{{profile.first_name}} {{profile.last_name}}",
  "contact":  "{{profile.verified_contacts.email.value}}" }
```

The contract composes the request; the host resolves the markers from the
calling user's profile inside the enclave at dispatch time. Consequences:

- WASM linear memory holds only the marker strings, so a memory-disclosure bug
  in our own code cannot leak an employee's name.
- Our logs and audit records store `employee_ref`, an opaque handle, only.
- If the profile lacks a field, the host returns `PlaceholderUnknown(field)`.
  The contract retries once without the optional markers and records
  `profile_placeholder_fallback` in the audit entry instead of guessing a value.

## Capability scoping

The contract's capability set *is* its WIT import list — there is no separate
manifest. It imports `logging`, `kv-store`, `http`,
`http-with-placeholders` and `tenant-context`; removing an import is the only
way to remove a capability.

Egress is authorised per call, from the delegation grant of **the identity the call
acts for**. The accepted row shape carries a list of functions plus the hosts that
row may reach:

```json
{ "grantee": "did:t3n:<agent>", "contract_id": "z:<tid>:expense-guard",
  "functions": ["check-expense", "request-approval"], "scopes": ["…"],
  "version_req": "0.1.0",
  "allowed_hosts": ["open.er-api.com", "postman-echo.com"] }
```

Per-function granularity is reachable by posting one such row per function, with a
single-element `functions` list — useful when different functions need different
hosts. What the SDK declares for that case (`BoundGrant.function: string`) is
rejected by the node, so the loop has to be written by hand (BUG-15).

The trap this design sets is on the *call* side, and it is BUG-14: an agent
`invoke` without `pii_did` is a **self** call, so the acting identity is the agent,
whose own document grants nothing — the enclave then denies the outbound call with
`egress_denied … not in the resolved allowlist []` while the grant above sits
correctly on the delegator's document. `src/invoke.ts` therefore refuses to run
without `PII_DID` and prints that reasoning instead.

`member-delegation-update` **replaces** the whole document, so a naive
single-grant write silently drops every other grant — including rows it does not
own, such as the profile-scope grant on `tee:user/contracts`. `src/grant.ts`
therefore reads the document first, keeps every row that is not the one it is
about to write, and posts the merged document back.

## Ledger design

Records are keyed `seq:<6-digit>` and written once; nothing rewrites a sequence
row. `exp:<expense_id>` points at the latest record for a claim so a single claim
can be traced across `check-expense` and `request-approval`. Sequence numbers are
derived by reading the highest existing key and incrementing.

What the ledger does **not** provide: any integrity proof a third party could
check without trusting the operator. There is no per-record signature, no Merkle
path and no receipt binding an expense, the policy version and the verdict into
something verifiable offline; the records are plain JSON written inside the
enclave. The audit record is therefore described everywhere as an *append-only,
sequence-numbered ledger* and never as cryptographically tamper-evident. A signed
decision receipt is listed as the first production follow-up in
`docs/HANDOVER.md`.

Limitations, stated honestly: KV has no atomic compare-and-set exposed through
the host interface, so two concurrent writers could in principle pick the same
sequence number. That is acceptable for an approval workflow (claims arrive
serially per team) but would need a different design for high-throughput
ledgers; see BUGLOG.md.

## Failure modes

| failure | behaviour |
|---|---|
| FX egress denied or upstream down | falls back to a non-expired cache entry; otherwise `fx_unavailable` forces `needs_approval` and `base_amount` is `null` |
| policy map missing | built-in default policy is used and flagged in the response |
| caller lacks a grant | host denies the outbound call (`host/http.egress_denied`); the verdict is still computed and recorded |
| audit write fails | the function returns `Err("kv: ...")` — a verdict that cannot be recorded is not reported as success |
| approver webhook non-2xx | `status: "failed"` with the HTTP code, still recorded in the ledger |
