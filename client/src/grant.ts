/**
 * Step 03 — the data owner authorises the agent (`member-delegation-update`).
 *
 * The document written here is `{ grants: GrantRow[], discover_dids: string[] }`
 * on the delegator's own identity: one row naming the agent as `grantee`, the
 * contract's canonical name, all six exported functions, and the two hosts the
 * contract is allowed to dial.
 *
 * What the node accepts — verified, not inferred
 * ----------------------------------------------
 * The write vocabulary is `functions: [...]` (PLURAL) plus `allowed_hosts`, per
 * row, exactly as the official walkthrough shows. The SDK's `BoundGrant` type
 * declares `function: string` (singular) and calls that "one function per
 * grant"; writing that shape is REJECTED by the deployed node:
 *
 *   Invalid delegation document: unknown field `function`, expected one of
 *   `grantee`, `contract_id`, `version_req`, `functions`, `scopes`,
 *   `read_scopes`, `allowed_hosts`, `window`
 *
 * Recorded as BUG-15 in docs/BUGLOG.md. Both shapes — one row carrying several
 * functions, and six single-function rows — are accepted and both resolve, so
 * the row shape is not what gates anything (see the section below).
 *
 * What actually gates the contract's outbound calls
 * -------------------------------------------------
 * `allowed_hosts` here is the authority; the identity it is read for is chosen
 * PER CALL. An agent invocation that omits `pii_did` is treated as a self call,
 * and the enclave then resolves the allowlist for the agent's own DID — which
 * holds no grant — so the contract's HTTP call fails with
 *
 *   host/http.egress_denied: egress host(s) [open.er-api.com] not in the
 *   resolved allowlist []
 *
 * while this document sits on the node, valid and correctly shaped. Calls must
 * therefore pass `pii_did=<delegator did>`; see src/invoke.ts and BUG-14.
 *
 * Usage:
 *   set -a && . ./.env && set +a && npm run delegate
 */
import {
  getContractVersion,
  getNodeUrl,
  setEnvironment,
} from "@terminal3/t3n-sdk";

import { writeDelegation } from "./lib/artifacts.js";
import {
  allowedHosts,
  delegationScopes,
  readEnv,
  requireEnv,
  targetEnvironment,
} from "./lib/env.js";
import {
  CONTRACT_TAIL_DEFAULT,
  CONTRACT_VERSION_DEFAULT,
  CONTRACT_FUNCTIONS,
  canonicalName,
  tenantIdFromDid,
} from "./lib/interface.js";
import { connect } from "./lib/session.js";

/**
 * A row as the deployed node accepts it. Deliberately NOT the SDK's `BoundGrant`:
 * that type's `function` (singular) is rejected on write (BUG-15), so importing
 * it here would only invite the shape the node refuses.
 */
interface GrantRow {
  grantee: string;
  contract_id: string;
  version_req?: string;
  functions: string[];
  scopes: unknown[];
  allowed_hosts?: string[];
  window?: { valid_from_secs?: number; valid_until_secs?: number };
}

/** The document as `member-delegation-get` returns it. */
interface DelegationDocument {
  grants: GrantRow[];
  discover_dids: string[];
}

const ENVIRONMENT = targetEnvironment();
setEnvironment(ENVIRONMENT);

const NODE_URL = getNodeUrl();
const TENANT_DID = requireEnv("TENANT_DID");
const AGENT_DID = requireEnv("AGENT_DID");
const CONTRACT_NAME = canonicalName(
  tenantIdFromDid(TENANT_DID),
  readEnv("CONTRACT_TAIL") ?? CONTRACT_TAIL_DEFAULT,
);
const VERSION_FALLBACK = readEnv("CONTRACT_VERSION") ?? CONTRACT_VERSION_DEFAULT;

function section(title: string): void {
  console.log("");
  console.log(`== ${title}`);
}

function describeError(error: unknown): string {
  if (error instanceof Error) return `${error.name}: ${error.message}`;
  return String(error);
}

function rowLabel(row: GrantRow): string {
  const functions = Array.isArray(row.functions) ? row.functions.join(",") : "(none)";
  return `${row.grantee} ${row.contract_id} functions=[${functions}]`;
}

/** True when `row` authorises the same (grantee, contract) pair we are about to write. */
function sameTarget(row: GrantRow, grantee: string, contract: string): boolean {
  return (
    String(row.grantee ?? "").toLowerCase() === grantee.toLowerCase() &&
    String(row.contract_id ?? "") === contract
  );
}

/** Tolerates a document that omits either field, rather than throwing on read. */
function parseDocument(raw: unknown): DelegationDocument {
  const candidate = (raw ?? {}) as { grants?: unknown; discover_dids?: unknown };
  return {
    grants: Array.isArray(candidate.grants) ? (candidate.grants as GrantRow[]) : [],
    discover_dids: Array.isArray(candidate.discover_dids)
      ? (candidate.discover_dids as string[])
      : [],
  };
}

// ---------------------------------------------------------------------------
// 0. Preconditions
// ---------------------------------------------------------------------------
section("0. Preconditions");
for (const name of ["TENANT_API_KEY", "TENANT_DID", "AGENT_DID", "APPROVAL_WEBHOOK_URL"]) {
  const value = readEnv(name);
  console.log(`  ${name.padEnd(22)} ${value === undefined ? "MISSING" : "(present, value withheld)"}`);
  if (value === undefined) process.exit(2);
}
console.log(`  environment            ${ENVIRONMENT}`);
console.log(`  node                   ${NODE_URL}`);
console.log(`  contract               ${CONTRACT_NAME}`);
console.log(`  delegator (signs)      ${TENANT_DID}`);
console.log(`  grantee (agent)        ${AGENT_DID}`);

// ---------------------------------------------------------------------------
// 1. Session — the delegator's own identity
// ---------------------------------------------------------------------------
section("1. Authenticate the delegator (handshake + EthSign challenge)");
const delegator = await connect("tenant");
console.log(`  did                    ${delegator.did}`);
console.log(`  address                ${delegator.address}`);
if (delegator.did.toLowerCase() !== TENANT_DID.toLowerCase()) {
  console.error(`  FAIL: session DID ${delegator.did} does not match TENANT_DID ${TENANT_DID}`);
  process.exit(1);
}

// ---------------------------------------------------------------------------
// 2. Contract version — pinned in the row, resolved from the node when it can be
// ---------------------------------------------------------------------------
section("2. Resolve the contract version for `version_req`");
let versionReq: string;
let versionSource: string;
try {
  versionReq = await getContractVersion(NODE_URL, CONTRACT_NAME);
  versionSource = "getContractVersion(node, canonical name)";
} catch (error) {
  versionReq = VERSION_FALLBACK;
  versionSource = `fallback ${VERSION_FALLBACK} — the node resolved no version: ${describeError(error)}`;
}
console.log(`  version_req            ${versionReq}`);
console.log(`  source                 ${versionSource}`);

// ---------------------------------------------------------------------------
// 3. The row to write
// ---------------------------------------------------------------------------
section("3. Plan the row");
const scopes = delegationScopes();
const hosts = allowedHosts();
const row: GrantRow = {
  grantee: AGENT_DID,
  contract_id: CONTRACT_NAME,
  version_req: versionReq,
  functions: [...CONTRACT_FUNCTIONS],
  scopes,
  allowed_hosts: hosts,
};
console.log(`  ${rowLabel(row)}`);
console.log(`  allowed_hosts          [${hosts.join(", ")}]`);
console.log(`  scopes                 ${scopes.length === 0 ? "[] — the contract reads no org-data scopes" : JSON.stringify(scopes)}`);
console.log("  one row carries all six functions: the node accepts both this and six");
console.log("  single-function rows, and resolves the allowlist identically either way.");

// ---------------------------------------------------------------------------
// 4. Read the current document, then merge
// ---------------------------------------------------------------------------
section("4. Read the current document (member-delegation-get)");
const before = parseDocument(await delegator.client.getMemberDelegation());
console.log(`  rows now               ${before.grants.length}`);
console.log(`  discover_dids          ${before.discover_dids.length}`);
for (const existing of before.grants) {
  const marks = sameTarget(existing, AGENT_DID, CONTRACT_NAME) ? "   <- replaced by this run" : "";
  console.log(`    ${rowLabel(existing)}${marks}`);
}

// Unrelated rows (a different grantee, or a different contract) are carried
// forward untouched: the write replaces the document, so dropping them here
// would silently revoke somebody else's authority.
const preserved = before.grants.filter(
  (existing) => !sameTarget(existing, AGENT_DID, CONTRACT_NAME),
);
console.log(`  preserved              ${preserved.length} row(s)`);

// ---------------------------------------------------------------------------
// 5. Write
// ---------------------------------------------------------------------------
section("5. Write (member-delegation-update)");
const next: DelegationDocument = {
  grants: [...preserved, row],
  discover_dids: before.discover_dids,
};
try {
  await delegator.client.memberDelegationUpdate(
    next as unknown as Parameters<typeof delegator.client.memberDelegationUpdate>[0],
  );
} catch (error) {
  console.error(`  FAIL: the node rejected the write — ${describeError(error)}`);
  console.error("  The accepted vocabulary is `functions` (plural); see BUG-15 in docs/BUGLOG.md.");
  process.exit(1);
}
console.log(`  OK — ${next.grants.length} row(s) submitted`);

// ---------------------------------------------------------------------------
// 6. Read back and assert
// ---------------------------------------------------------------------------
section("6. Read back and assert");
const after = parseDocument(await delegator.client.getMemberDelegation());
const stored = after.grants.find((candidate) => sameTarget(candidate, AGENT_DID, CONTRACT_NAME));
let failures = 0;
const check = (label: string, ok: boolean, detail: string): void => {
  console.log(`  ${ok ? "PASS" : "FAIL"}  ${label}: ${detail}`);
  if (!ok) failures += 1;
};
check("row present", stored !== undefined, stored === undefined ? "no row for the agent" : "1 row");
if (stored !== undefined) {
  const storedFunctions = Array.isArray(stored.functions) ? stored.functions : [];
  const storedHosts = Array.isArray(stored.allowed_hosts) ? stored.allowed_hosts : [];
  check(
    "functions",
    storedFunctions.length === CONTRACT_FUNCTIONS.length,
    `[${storedFunctions.join(", ")}]`,
  );
  check(
    "allowed_hosts",
    hosts.every((host) => storedHosts.includes(host)),
    `[${storedHosts.join(", ")}]`,
  );
  check(
    "version_req",
    stored.version_req === versionReq,
    String(stored.version_req ?? "(absent)"),
  );
  check("rows preserved", after.grants.length === next.grants.length, `${after.grants.length} row(s) total`);
}
console.log("");
console.log("  Read-back proves the document is stored as written. It does NOT prove the");
console.log("  enclave will honour it: resolution is per call and keyed on the identity the");
console.log("  call acts for, so the same document authorises egress only when the caller");
console.log("  passes pii_did=<delegator>. src/doctor.ts asserts that with a real call.");

// ---------------------------------------------------------------------------
// 7. Artifact
// ---------------------------------------------------------------------------
section("7. Record the grant as an artifact");
await writeDelegation({
  recorded_at: new Date().toISOString(),
  environment: ENVIRONMENT,
  contract_name: CONTRACT_NAME,
  contract_version: versionReq,
  grantee: AGENT_DID,
  delegator: delegator.did,
  path_used: "memberDelegationUpdate(read-merge-write, one rows-per-grantee shape)",
  functions: [...CONTRACT_FUNCTIONS],
  allowed_hosts: hosts,
  scopes,
  preserved_rows: preserved.map((existing) => rowLabel(existing)),
});
console.log("  artifact               client/artifacts/delegation.json");

section("Verdict");
if (failures > 0) {
  console.log(`  FAIL — ${failures} assertion(s) did not hold.`);
  process.exit(1);
}
console.log("  PASS — the agent is authorised on all six functions, with both hosts, by the");
console.log(`  identity ${delegator.did}.`);
