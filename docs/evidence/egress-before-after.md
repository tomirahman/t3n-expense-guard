# Evidence — the egress grant, before and after `pii_did`

Two runs of the same code against the same registered contract
(`z:f817f498…:expense-guard` v0.1.0), with one variable changed: whether the invoke
request named the identity it acts for.

## Without `pii_did` — the failure we chased for hours

The delegation document on the delegator's account granted the agent six functions
and both hosts, and `member-delegation-get` returned it verbatim. The contract's
outbound FX lookup still failed, and the enclave log carried the only real clue:

```
fx: live lookup failed: host/http.egress_denied: egress host(s)
    [open.er-api.com] not in the resolved allowlist []
```

`discoverCheckDelegation` agreed that nothing was authorised, and said nothing more:

```
{"authorised":false,"disclosed":false,"satisfied":[],"missing":[]}
```

## With `pii_did` = the delegator — same document, same contract

```
{"expense_id":"EXP-PII-PROBE","verdict":"compliant","base_currency":"USD",
 "base_amount":48.46,"fx_rate":1.153788,"fx_source":"open.er-api.com",
 "rule_hits":[],"ledger_seq":10}
```

and the same check:

```
{"authorised":true,"disclosed":true,
 "satisfied":[{"grant":"member_delegation",
               "contract":"z:f817f498…:expense-guard",
               "functions":["health","set-policy","get-policy","check-expense",
                            "get-audit"],"scopes":[]}],
 "missing":[]}
```

`fx_source: open.er-api.com` is the part that matters: the rate was fetched by the
enclave over its own HTTP egress, not supplied by the caller.

## The row shape is not the variable

Both row layouts were written to the same document and called afterwards:

| row written | invoke without `pii_did` | invoke with `pii_did` |
|---|---|---|
| one row, `functions: [six names]` | `egress_denied`, allowlist `[]` | host resolves, `fx_source` set |
| six rows, `functions: [one name]` each | `egress_denied`, allowlist `[]` | host resolves, `fx_source` set |

The resolver honours both shapes; only the acting identity decides the allowlist.
`BUGLOG.md` BUG-14 has the diagnosis, and BUG-15 the SDK/node vocabulary mismatch
that makes the per-function form hard to write in the first place.

Transcripts of the green runs are in `client/demo-output/`; `demo.png` and
`doctor.png` in this directory are renders of real captures (`script -q`), not
mock-ups.
