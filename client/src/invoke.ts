/**
 * Step 04 — drive `z:<tid>:expense-guard` as the organisation-owned agent.
 *
 * This agent was created with `t3n agent create --org …`, so T3N minted its
 * secp256k1 key *inside* the TEE and handed back only an opaque bearer token
 * (`t3n_key_<key-id>.<secret>`). There is no ethers-style private key, so the
 * session flow (handshake + EthSign authenticate) does not apply. The agent
 * authenticates STATELESSLY: one HTTPS request carrying the bearer token in the
 * `X-T3N-Api-Key` header, issued through the SDK's `invoke()` helper.
 *
 * Run:
 *   cd client && set -a && . ./.env && set +a && ./node_modules/.bin/tsx src/invoke.ts
 *
 * The bearer token is read from the environment, never printed, never written
 * to the transcript file, and never inserted into an error message. The SDK's
 * `InvokeError` guarantees the same for its own messages; the one place the raw
 * node body is shown (see `rawNodeError`) passes through `redact()` first. Only
 * the non-secret key-id half is ever displayed.
 *
 * Scenario — every function name and JSON key below comes from
 * docs/INTERFACE.md, which is normative for this contract:
 *   0   health                        liveness, no KV, no egress
 *   0b  (read-only diagnostics)       is the contract registered? is the agent granted?
 *   1   get/set-policy                make the demo policy deterministic
 *   2   check-expense  EXP-1042       small EUR claim → expect `compliant`
 *                                     (the contract performs its outbound HTTP
 *                                     FX lookup and returns a decision)
 *   3   check-expense  EXP-1042 again same claim resubmitted → expect
 *                                     `possible_duplicate`
 *   4   check-expense  EXP-1044       large claim → expect
 *                                     `over_approval_threshold` escalation
 *   5   check-expense  EXP-1045       disallowed category → expect `rejected`
 *   6   get-audit      {}             read the ledger back, newest first
 *
 * A failing step never aborts the run: the platform's verbatim error is printed
 * and the next step proceeds, so one blocked path still leaves the rest of the
 * contract observable.
 */
import { mkdir, writeFile } from "node:fs/promises";
import path from "node:path";

import {
  InvokeError,
  discoverCheckDelegation,
  getEnvironmentName,
  getNodeUrl,
  invoke,
  setEnvironment,
  type Environment,
  type InvokeRequest,
} from "@terminal3/t3n-sdk";

import { DEMO_OUTPUT_DIR, readEnv } from "./lib/env.js";
import {
  CONTRACT_FUNCTIONS,
  CONTRACT_TAIL_DEFAULT,
  CONTRACT_VERSION_DEFAULT,
  canonicalName,
  demoPolicy,
  tenantIdFromDid,
  type AuditResult,
  type CheckExpenseInput,
  type CheckExpenseResult,
  type HealthResult,
  type Policy,
  type SetPolicyResult,
} from "./lib/interface.js";

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

const ENVIRONMENT = (readEnv("T3N_ENVIRONMENT") ?? readEnv("T3N_ENV") ?? "testnet") as Environment;
const TENANT_DID = readEnv("TENANT_DID") ?? "";
const AGENT_DID = readEnv("AGENT_DID") ?? "";
/** Safe to display: the key *identifier*, never the secret half. */
const AGENT_KEY_ID = readEnv("AGENT_KEY_ID") ?? "(unset)";
/** NEVER printed unless passed through {@link redact}. */
const AGENT_INVOKE_KEY = readEnv("AGENT_INVOKE_KEY") ?? "";
/**
 * Optional delegated-call target. `invoke()`'s `pii_did` is "the member the
 * agent wants to act for" — set it to act on behalf of the data owner that
 * signed the grant, omit it for a self call. The data owner's DID is produced
 * by the grant flow (a different step), so this is read from the environment
 * when present rather than invented here.
 */
const PII_DID = readEnv("PII_DID") ?? readEnv("DELEGATOR_DID") ?? readEnv("USER_DID") ?? undefined;

/**
 * Run-scoped claim ids.
 *
 * `check-expense` remembers `exp:<expense_id>` so it can flag a resubmission, so
 * a fixed id would make the SECOND run of this demo report step 2's claim as a
 * duplicate. Ids stay stable inside one run — step 3 must resubmit step 2's
 * claim — and differ between runs. Pin one with DEMO_RUN_ID to reproduce a run.
 */
const RUN_STAMP = (readEnv("DEMO_RUN_ID") ?? String(Date.now())).slice(-6);
const claimId = (base: string): string => `EXP-${RUN_STAMP}-${base}`;

/**
 * The duplicate rule keys on employee/amount/currency/vendor inside a 30-day
 * window, and the entries a previous run of this demo wrote are still inside it.
 * A small run-scoped drift on the amounts keeps each run's first claim clean
 * while step 3's resubmission (the same object) still trips the rule. Override
 * DEMO_RUN_ID to reproduce a run.
 */
const RUN_DRIFT = (Number(RUN_STAMP) % 40) / 100;

if (AGENT_INVOKE_KEY === "") {
  console.error("AGENT_INVOKE_KEY is not set — source client/.env before running.");
  process.exit(2);
}
if (TENANT_DID === "") {
  console.error("TENANT_DID is not set — source client/.env before running.");
  process.exit(2);
}
if (PII_DID === undefined) {
  console.error(
    "PII_DID is not set, and omitting it silently changes which identity's grant the\n" +
      "enclave resolves egress against: without it the call is a SELF call, the acting\n" +
      "identity is the agent, the agent holds no grant, and every outbound call fails with\n" +
      "  host/http.egress_denied: egress host(s) [...] not in the resolved allowlist []\n" +
      "even though the delegation document is correct and reads back clean. Set PII_DID to\n" +
      "the delegator's DID (the identity that signed the grant) — see docs/BUGLOG.md BUG-14.",
  );
  process.exit(2);
}

setEnvironment(ENVIRONMENT);
const BASE_URL = getNodeUrl();
const CONTRACT_NAME = canonicalName(
  tenantIdFromDid(TENANT_DID),
  readEnv("CONTRACT_TAIL") ?? CONTRACT_TAIL_DEFAULT,
);
const VERSION = readEnv("CONTRACT_VERSION") ?? CONTRACT_VERSION_DEFAULT;

// ---------------------------------------------------------------------------
// Secret hygiene
// ---------------------------------------------------------------------------

/** Every secret-shaped env value, so a node body can never echo one back. */
const SECRETS = [
  ...new Set(
    Object.entries(process.env)
      .filter(([name, value]) => /_KEY$|_SECRET$/.test(name) && typeof value === "string" && value.length >= 20)
      .map(([, value]) => value as string),
  ),
];

/** Strip credentials from text before it can reach a terminal or a file. */
function redact(text: string): string {
  let out = text;
  for (const secret of SECRETS) out = out.split(secret).join("[REDACTED]");
  return out
    .replace(/t3n_key_[\w.-]+/g, "t3n_key_[REDACTED]")
    .replace(/0x[0-9a-fA-F]{64}/g, "0x[REDACTED]");
}

const line = (char = "─") => console.log(char.repeat(74));
const header = (text: string) => {
  console.log();
  line();
  console.log(text);
  line();
};

// ---------------------------------------------------------------------------
// Reusable caller
// ---------------------------------------------------------------------------

type Outcome<T> = { ok: true; value: T } | { ok: false; error: string; nodeError?: string | undefined };

function wireRequest(fn: string, input: unknown): InvokeRequest {
  const request: InvokeRequest = {
    contract_id: CONTRACT_NAME,
    contract_version: VERSION,
    function_name: fn,
    input,
  };
  // Delegated call: name the member whose grant authorises this action.
  if (PII_DID !== undefined) request.pii_did = PII_DID;
  return request;
}

/**
 * The SDK's `InvokeError` is deliberately opaque ("the message is always one of
 * a small set of fixed, generic strings … it NEVER contains the raw api key,
 * the response body text"). That is the right default, but it makes a failure
 * impossible to diagnose. So on a non-2xx we re-issue the identical request by
 * hand, once, purely to read the node's own error JSON — redacted before it is
 * printed or persisted.
 */
async function rawNodeError(request: InvokeRequest): Promise<string | undefined> {
  try {
    const response = await fetch(`${BASE_URL}/api/invoke`, {
      method: "POST",
      headers: { "content-type": "application/json", "X-T3N-Api-Key": AGENT_INVOKE_KEY },
      body: JSON.stringify(request),
    });
    const body = await response.text();
    return redact(`HTTP ${response.status} ${body.slice(0, 800)}`);
  } catch {
    return undefined;
  }
}

/**
 * Submit exactly one contract call and print the decoded result.
 *
 * A platform rejection (`not registered`, `host/http.egress_denied`,
 * `InsufficientCreditError`, an unknown function, a missing grant) must never
 * be translated into a polite "failed" that hides what the node actually said.
 */
async function call<T>(fn: string, input: unknown, label: string): Promise<Outcome<T>> {
  const request = wireRequest(fn, input);
  console.log(`\n▸ ${label}`);
  console.log(`  → ${fn} ${JSON.stringify(input)}`);
  try {
    const value = await invoke<T>({ baseUrl: BASE_URL, apiKey: AGENT_INVOKE_KEY, request });
    console.log(`  ✓ ${JSON.stringify(value)}`);
    return { ok: true, value };
  } catch (error) {
    const name = error instanceof InvokeError ? "InvokeError" : ((error as Error)?.name ?? "Error");
    const status = error instanceof InvokeError && error.status !== undefined ? ` (HTTP ${error.status})` : "";
    const detail = error instanceof Error ? error.message : String(error);
    console.log(`  ✗ ${name}${status}: ${detail}`);
    const nodeError = await rawNodeError(request);
    if (nodeError !== undefined) console.log(`    node says: ${nodeError}`);
    console.log("    (continuing to the next step)");
    return { ok: false, error: `${name}${status}: ${detail}`, nodeError };
  }
}

/** One scenario step's result, for the pass/fail table and the transcript. */
interface Step {
  step: string;
  description: string;
  expected: string;
  actual: string;
  status: "PASS" | "MISMATCH" | "ERROR" | "BLOCKED";
  /** Verbatim error text as the platform produced it, when the step failed. */
  error?: string | undefined;
}

const steps: Step[] = [];
const record = (s: Step) => {
  steps.push(s);
  console.log(`  ⇒ expected ${s.expected} — observed ${s.actual} — ${s.status}`);
};

/**
 * Classify a failure honestly. Two of these are *dependencies*, not defects in
 * this script, and they are reported as such rather than papered over.
 */
function classify(text: string): Step["status"] {
  if (/not registered|not_found|unknown contract|no such contract/i.test(text)) return "BLOCKED";
  if (/egress_denied|not authoris|not authoriz|delegation|forbidden|unauthoris|unauthoriz/i.test(text)) {
    return "BLOCKED";
  }
  if (/InsufficientCredit|credit balance|quota exceeded/i.test(text)) return "BLOCKED";
  return "ERROR";
}

// ---------------------------------------------------------------------------
// Run
// ---------------------------------------------------------------------------

async function main(): Promise<void> {
  header("ExpenseGuard — org-agent driver (stateless bearer-token invocation)");
  console.log(`environment    : ${ENVIRONMENT} (SDK env=${getEnvironmentName()})`);
  console.log(`node baseUrl   : ${BASE_URL}`);
  console.log(`contract       : ${CONTRACT_NAME}  version ${VERSION}`);
  console.log(`call mode      : ${PII_DID === undefined ? "self call (no pii_did)" : `delegated call, pii_did=${PII_DID}`}`);
  console.log(`agent DID      : ${AGENT_DID}`);
  console.log(`agent key-id   : ${AGENT_KEY_ID}   (secret half withheld — never printed)`);
  console.log(`tenant DID     : ${TENANT_DID}`);

  // -- 0. health ------------------------------------------------------------
  header("STEP 0 — health: is the contract registered and reachable?");
  {
    const out = await call<HealthResult>("health", {}, "liveness probe (no KV, no egress)");
    if (out.ok) {
      record({
        step: "0 health",
        description: "liveness",
        expected: "ok=true",
        actual: JSON.stringify(out.value),
        status: out.value?.ok === true ? "PASS" : "MISMATCH",
      });
    } else {
      record({
        step: "0 health",
        description: "liveness",
        expected: "ok=true",
        actual: out.nodeError ?? out.error,
        status: classify(out.nodeError ?? out.error),
        error: out.nodeError ?? out.error,
      });
    }
  }

  // -- 0b. read-only authorisation diagnostics ------------------------------
  header("STEP 0b — (read-only) does the data owner's grant authorise this agent?");
  console.log(
    "  discover.check-delegation is a signed read on the agent's own key: it names the\n" +
      "  grant edges that are satisfied and the ones still missing. No contract call.",
  );
  {
    const functions = CONTRACT_FUNCTIONS.filter((f) => f !== "request-approval");
    try {
      const check = await discoverCheckDelegation(
        { baseUrl: BASE_URL, apiKey: AGENT_INVOKE_KEY },
        { contract: CONTRACT_NAME, pii_did: PII_DID ?? AGENT_DID, functions: [...functions], scopes: [] },
      );
      console.log(`  ✓ ${JSON.stringify(check)}`);
      record({
        step: "0b delegation",
        description: "grant present for the 5 called functions",
        expected: "authorised=true",
        actual: `authorised=${check?.authorised} satisfied=${check?.satisfied?.length ?? 0} missing=${JSON.stringify(check?.missing ?? [])}`,
        status: check?.authorised === true ? "PASS" : "MISMATCH",
      });
    } catch (error) {
      const detail = error instanceof Error ? `${error.name}: ${error.message}` : String(error);
      console.log(`  ✗ ${detail}`);
      record({
        step: "0b delegation",
        description: "grant check",
        expected: "authorised=true",
        actual: detail,
        status: "ERROR",
        error: detail,
      });
    }
  }

  // -- 1. policy ------------------------------------------------------------
  header("STEP 1 — policy: read it, install the INTERFACE.md policy if unset");
  {
    const before = await call<Policy>("get-policy", {}, "read the installed policy");
    if (!before.ok) {
      record({
        step: "1 get-policy",
        description: "policy read",
        expected: "Policy JSON",
        actual: before.nodeError ?? before.error,
        status: classify(before.nodeError ?? before.error),
        error: before.nodeError ?? before.error,
      });
    } else {
      const categories = Object.keys(before.value?.categories ?? {}).length;
      if (categories > 0) {
        console.log(`  policy already installed: ${JSON.stringify(before.value)}`);
        record({
          step: "1 get-policy",
          description: "policy present",
          expected: "categories>=1",
          actual: `categories=${categories}`,
          status: "PASS",
        });
      } else {
        console.log(
          "\n  policy is at its built-in default (no category table) — installing the\n" +
            "  INTERFACE.md policy so the category paths below are deterministic.",
        );
        const set = await call<SetPolicyResult>("set-policy", demoPolicy("USD"), "install demo policy");
        record({
          step: "1 set-policy",
          description: "install INTERFACE.md policy",
          expected: "stored=true, categories=3",
          actual: set.ok ? JSON.stringify(set.value) : (set.nodeError ?? set.error),
          status: set.ok ? (set.value?.stored === true ? "PASS" : "MISMATCH") : classify(set.error),
          error: set.ok ? undefined : (set.nodeError ?? set.error),
        });
        if (set.ok) {
          const after = await call<Policy>("get-policy", {}, "read the policy back");
          if (after.ok) console.log(`  policy now: ${JSON.stringify(after.value)}`);
        }
      }
    }
  }

  // -- 2. (a) claim that should be approved ---------------------------------
  header("STEP 2 — (a) a first claim that should be APPROVED end-to-end");
  const approvedClaim: CheckExpenseInput = {
    expense_id: claimId("1042"),
    employee_ref: "emp_7f3a",
    amount: 96.5 + RUN_DRIFT,
    currency: "EUR",
    category: "software",
    vendor: "Amazon Web Services",
    incurred_on: "2026-09-02",
  };
  {
    const out = await call<CheckExpenseResult>(
      "check-expense",
      approvedClaim,
      "claim EXP-1042 (96.50 EUR, software) — under every limit",
    );
    if (out.ok) {
      record({
        step: "2 check-expense EXP-1042",
        description: "first claim — FX conversion + decision",
        expected: "compliant",
        actual: `verdict=${out.value?.verdict} base_amount=${out.value?.base_amount} ${out.value?.base_currency} fx_rate=${out.value?.fx_rate} fx_source=${out.value?.fx_source} rules=${JSON.stringify(out.value?.rule_hits)} ledger_seq=${out.value?.ledger_seq}`,
        status: out.value?.verdict === "compliant" ? "PASS" : "MISMATCH",
      });
    } else {
      record({
        step: "2 check-expense EXP-1042",
        description: "first claim",
        expected: "compliant",
        actual: out.nodeError ?? out.error,
        status: classify(out.error),
        error: out.nodeError ?? out.error,
      });
    }
  }

  // -- 3. (b) the same claim resubmitted → duplicate -------------------------
  header("STEP 3 — (b) the SAME claim resubmitted: duplicate detection");
  console.log(
    "  identical expense_id, employee_ref, amount, currency and vendor.\n" +
      "  INTERFACE.md §rule ids + ledger.rs::find_duplicate key on those four fields\n" +
      "  (employee_ref + amount + currency + vendor) inside duplicate_window_days.",
  );
  {
    const out = await call<CheckExpenseResult>("check-expense", approvedClaim, "claim EXP-1042 resubmitted verbatim");
    if (out.ok) {
      const hits = (out.value?.rule_hits ?? []) as string[];
      record({
        step: "3 duplicate EXP-1042",
        description: "resubmission caught",
        expected: "rule_hits contains possible_duplicate",
        actual: `verdict=${out.value?.verdict} rules=${JSON.stringify(hits)} ledger_seq=${out.value?.ledger_seq}`,
        status: hits.includes("possible_duplicate") ? "PASS" : "MISMATCH",
      });
    } else {
      record({
        step: "3 duplicate EXP-1042",
        description: "resubmission",
        expected: "possible_duplicate",
        actual: out.nodeError ?? out.error,
        status: classify(out.error),
        error: out.nodeError ?? out.error,
      });
    }
  }

  // -- 4. (c) policy threshold escalation -----------------------------------
  header("STEP 4 — (c) a claim escalated by the policy threshold");
  const bigClaim: CheckExpenseInput = {
    expense_id: claimId("1044"),
    employee_ref: "emp_7f3a",
    amount: 1200.0 + RUN_DRIFT,
    currency: "EUR",
    category: "travel",
    vendor: "Lufthansa",
    incurred_on: "2026-09-10",
  };
  {
    const out = await call<CheckExpenseResult>(
      "check-expense",
      bigClaim,
      "claim EXP-1044 (1200.00 EUR travel — above the 500 USD approval threshold, below the 1500 travel limit)",
    );
    if (out.ok) {
      const hits = (out.value?.rule_hits ?? []) as string[];
      const escalated = hits.includes("over_approval_threshold") || hits.includes("over_category_limit");
      record({
        step: "4 threshold EXP-1044",
        description: "threshold escalation",
        expected: "needs_approval + over_approval_threshold",
        actual: `verdict=${out.value?.verdict} base_amount=${out.value?.base_amount} rules=${JSON.stringify(hits)}`,
        status: out.value?.verdict === "needs_approval" && escalated ? "PASS" : "MISMATCH",
      });
    } else {
      record({
        step: "4 threshold EXP-1044",
        description: "threshold escalation",
        expected: "needs_approval",
        actual: out.nodeError ?? out.error,
        status: classify(out.error),
        error: out.nodeError ?? out.error,
      });
    }
  }

  // -- 5. disallowed category → rejected ------------------------------------
  header("STEP 5 — (c′) a claim the policy REJECTS outright");
  const badClaim: CheckExpenseInput = {
    expense_id: claimId("1045"),
    employee_ref: "emp_91bb",
    amount: 40.0 + RUN_DRIFT,
    currency: "USD",
    category: "entertainment",
    vendor: "Skyline Lounge",
    incurred_on: "2026-09-11",
  };
  {
    const out = await call<CheckExpenseResult>(
      "check-expense",
      badClaim,
      "claim EXP-1045 (40.00 USD entertainment — allowed:false)",
    );
    if (out.ok) {
      const hits = (out.value?.rule_hits ?? []) as string[];
      record({
        step: "5 rejected EXP-1045",
        description: "disallowed category",
        expected: "rejected + disallowed_category",
        actual: `verdict=${out.value?.verdict} rules=${JSON.stringify(hits)}`,
        status: out.value?.verdict === "rejected" ? "PASS" : "MISMATCH",
      });
    } else {
      record({
        step: "5 rejected EXP-1045",
        description: "disallowed category",
        expected: "rejected",
        actual: out.nodeError ?? out.error,
        status: classify(out.error),
        error: out.nodeError ?? out.error,
      });
    }
  }

  // -- 6. (d) read the ledger back ------------------------------------------
  header("STEP 6 — (d) the audit ledger, newest first");
  {
    const out = await call<AuditResult>("get-audit", { limit: 20 }, "read the append-only ledger");
    if (out.ok) {
      const entries = out.value?.entries ?? [];
      console.log();
      console.log("  seq   expense_id   verdict         base_amount  category       vendor                   rule_hits");
      for (const e of entries) {
        console.log(
          `  ${String(e.seq).padEnd(5)} ${String(e.expense_id).padEnd(12)} ${String(e.verdict).padEnd(15)} ` +
            `${String(e.base_amount ?? "null").padEnd(12)} ${String(e.category).padEnd(14)} ` +
            `${String(e.vendor).padEnd(24)} ${JSON.stringify(e.rule_hits)}`,
        );
      }
      record({
        step: "6 get-audit",
        description: "ledger read-back",
        expected: "count>=4 (one record per check-expense above)",
        actual: `count=${out.value?.count}`,
        status: (out.value?.count ?? 0) >= 4 ? "PASS" : "MISMATCH",
      });
    } else {
      record({
        step: "6 get-audit",
        description: "ledger read-back",
        expected: "count>=4",
        actual: out.nodeError ?? out.error,
        status: classify(out.error),
        error: out.nodeError ?? out.error,
      });
    }
  }

  // -- summary --------------------------------------------------------------
  header("RESULT");
  for (const s of steps) console.log(`  ${s.status.padEnd(9)} ${s.step.padEnd(28)} ${s.description}`);
  const counts = steps.reduce<Record<string, number>>((acc, s) => {
    acc[s.status] = (acc[s.status] ?? 0) + 1;
    return acc;
  }, {});
  console.log(`\n  ${JSON.stringify(counts)}`);

  const failed = steps.filter((s) => s.status === "BLOCKED" || s.status === "ERROR");
  if (failed.length > 0) {
    console.log("\n  Verbatim platform errors:");
    for (const s of failed) console.log(`    ${s.step}:\n      ${s.error}`);
  }

  // -- transcript -----------------------------------------------------------
  await mkdir(DEMO_OUTPUT_DIR, { recursive: true });
  const stamp = new Date().toISOString().replace(/[:.]/g, "-");
  const file = path.join(DEMO_OUTPUT_DIR, `${stamp}-invoke.json`);
  await writeFile(
    file,
    `${JSON.stringify(
      {
        ran_at: new Date().toISOString(),
        environment: ENVIRONMENT,
        node_base_url: BASE_URL,
        contract: CONTRACT_NAME,
        contract_version: VERSION,
        call_mode: PII_DID === undefined ? "self" : "delegated",
        agent_did: AGENT_DID,
        agent_key_id: AGENT_KEY_ID,
        tenant_did: TENANT_DID,
        note: "the bearer token is deliberately absent: never logged, never persisted",
        steps,
      },
      null,
      2,
    )}\n`,
    "utf8",
  );
  console.log(`\n  transcript: ${file}`);
}

main().then(
  () => process.exit(0),
  (error: unknown) => {
    console.error("\nfatal:", error instanceof Error ? error.stack : String(error));
    process.exit(1);
  },
);
