# Evidence — decision samples, quoted from a committed run

The submission asks for a screenshot of an actual compliance decision. This file
is the text form of that capture: the lines below are copied verbatim from the
`actual` field of a committed demo transcript, which is the raw output of
`npm run demo` against the registered testnet contract. Nothing here was typed by
hand from memory; the source file and step are named for each block.

Source: `client/demo-output/2026-09-16T07-29-04-824Z-invoke.json`
(run of `npm run demo`, 8 of 8 steps PASS, contract
`z:f817f49837375d99b44cbf8907becc154f2a5fc9:expense-guard` v0.1.0).

## Step 4 — `check-expense`, above the approval threshold

```
verdict=needs_approval base_amount=1384.56 rules=["over_approval_threshold"]
```

A EUR claim equivalent to 1384.56 USD is over the policy's `approval_threshold`,
so the verdict is `needs_approval` and the rule id names the reason. In the
contract's JSON response this field is `rule_hits` (see `docs/INTERFACE.md`); the
transcript prints it as `rules=`.

## Step 5 — `check-expense` on a disallowed category

```
verdict=rejected rules=["disallowed_category"]
```

## Step 3 — `check-expense`, duplicate resubmission

```
verdict=needs_approval rules=["possible_duplicate"] ledger_seq=33
```

## Step 2 — `check-expense`, compliant claim with FX conversion

```
verdict=compliant base_amount=111.35 USD fx_rate=1.153788 fx_source=cache rules=[] ledger_seq=32
```

`fx_rate 1.153788` is the EUR→USD rate the enclave fetched over its own egress;
`fx_source=cache` means the value came from `z:<tid>:fx` on this call because an
earlier call in the same run had already fetched it live. The live-fetch path,
including the egress denial that precedes it when the acting identity is missing,
is captured in `docs/evidence/egress-before-after.md`.

## Why this is a text capture and not a new screenshot

A fresh terminal screenshot of these four lines would require another metered
agent session, and the two screenshots that do exist (`doctor.png`, `demo.png`)
already come from real captures of the running system. Quoting the committed
transcript keeps the evidence checkable — anyone can re-open the JSON file, or
re-run `npm run demo` and compare.
