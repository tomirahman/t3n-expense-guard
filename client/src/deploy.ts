/**
 * `deploy.ts` — one-shot provisioning of the z-expense-guard TEE contract.
 *
 * This is the "step 01 + step 02" of the harness collapsed into a single
 * re-runnable script. It does four things, in order, and nothing else:
 *
 *   1. authenticate as the **tenant** (the identity that owns the z-namespace),
 *   2. register the already-built WASM component under `<tail>@<version>`,
 *   3. create the four KV maps the contract reads/writes, ACL'd to the contract,
 *   4. seed the policy document at `z:<tid>:policy` key `current`.
 *
 * Map names, map tails, the policy document shape and the version live in
 * `./lib/interface.ts` — the TypeScript mirror of `docs/INTERFACE.md`. Nothing
 * here re-derives a name by hand: the contract hex-encodes the raw tenant DID
 * exactly once (`contract/src/maps.rs`), and `TenantClient.canonicalName()` is
 * the client-side spelling of that same rule.
 *
 * Credentials: `TENANT_API_KEY` is read from the environment and handed to the
 * signer handler and nothing else. It is never logged, echoed or written to
 * disk, and the only identity material this script prints is public (DIDs,
 * canonical names, numeric ids, byte counts).
 *
 * Usage:
 *   cd client && set -a && . ./.env && set +a && ./node_modules/.bin/tsx src/deploy.ts
 */

import { createHash } from "node:crypto";
import { readFile } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";

import {
  T3nClient,
  TenantClient,
  createEthAuthInput,
  eth_get_address,
  fetchTrustedManifest,
  getNodeUrl,
  loadWasmComponent,
  metamask_sign,
  setEnvironment,
  type Environment,
  type GuestToHostHandlers,
  type WriterSet,
} from "@terminal3/t3n-sdk";

import {
  CONTRACT_TAIL_DEFAULT,
  CONTRACT_VERSION_DEFAULT,
  MAP_TAIL,
  demoPolicy,
  tenantIdFromDid,
  type Policy,
} from "./lib/interface.js";
import { REGISTRATION_FILE, writeRegistration } from "./lib/artifacts.js";

// ---------------------------------------------------------------------------
// Configuration (paths + the handful of environment variables we need)
// ---------------------------------------------------------------------------

const MODULE_DIR = path.dirname(fileURLToPath(import.meta.url));

/** Repository root — `client/src/` → `t3n-agent/`. */
const REPO_ROOT = path.resolve(MODULE_DIR, "..", "..");

/** The built component registered by this script. Read-only; owned by `contract/`. */
const WASM_PATH = path.join(
  REPO_ROOT,
  "contract",
  "target",
  "wasm32-wasip2",
  "release",
  "z_expense_guard.wasm",
);

/** Tail the contract is registered under, and the KV tails it needs. */
const CONTRACT_TAIL = CONTRACT_TAIL_DEFAULT;
const CONTRACT_VERSION = CONTRACT_VERSION_DEFAULT;

/**
 * Every map goes through the same ACL: no public read, and the *only* writer
 * and reader is the contract itself. Omitting `readers` is not an option — the
 * kv-governor then denies reads to the contract with no error at creation time.
 */
const MAP_VISIBILITY = "private";

/** The four tails the Rust component actually touches (`contract/src/maps.rs`). */
const MAP_TAILS = [MAP_TAIL.policy, MAP_TAIL.audit, MAP_TAIL.fx, MAP_TAIL.secrets] as const;

/** Key inside `z:<tid>:policy` holding the active policy document. */
const POLICY_KEY = "current";

function requireEnv(name: string): string {
  const raw = process.env[name];
  const trimmed = raw?.trim() ?? "";
  if (trimmed.length === 0) {
    throw new Error(`environment variable ${name} is empty or unset — run with . ./.env loaded`);
  }
  return trimmed;
}

// ---------------------------------------------------------------------------
// Small reporting helpers — ids and sizes only, never key material
// ---------------------------------------------------------------------------

let stepNumber = 0;

function step(title: string): void {
  stepNumber += 1;
  console.log(`\n── ${String(stepNumber).padStart(2, "0")}. ${title}`);
}

function detail(label: string, value: string | number | boolean): void {
  console.log(`   ${label.padEnd(22)} ${String(value)}`);
}

function note(text: string): void {
  console.log(`   · ${text}`);
}

function errorText(error: unknown): string {
  if (error instanceof Error) {
    const cause = (error as { cause?: unknown }).cause;
    if (cause instanceof Error) return `${error.message} — ${cause.message}`;
    return error.message;
  }
  return String(error);
}

// ---------------------------------------------------------------------------
// 1. Authenticate as the tenant
// ---------------------------------------------------------------------------

interface TenantSession {
  client: T3nClient;
  tenant: TenantClient;
  did: string;
  tenantId: string;
  environment: Environment;
  nodeUrl: string;
}

async function connectTenant(privateKey: string, expectedDid: string): Promise<TenantSession> {
  const environment = (process.env["T3N_ENVIRONMENT"]?.trim() || "testnet") as Environment;
  if (environment !== "testnet" && environment !== "production" && environment !== "sandbox") {
    throw new Error(`T3N_ENVIRONMENT must be testnet | production | sandbox — got ${environment}`);
  }

  // Global on purpose: `getNodeUrl()` and the default transport read this, so it
  // has to be set before any client is constructed.
  setEnvironment(environment);
  const nodeUrl = getNodeUrl();

  const trustAnchor = await fetchTrustedManifest(environment);
  const wasmComponent = await loadWasmComponent();
  const address = eth_get_address(privateKey);
  const handlers: GuestToHostHandlers = {
    EthSign: metamask_sign(address, undefined, privateKey),
  };

  const client = new T3nClient({ trustAnchor, wasmComponent, handlers });
  await client.handshake();
  const did = (await client.authenticate(createEthAuthInput(address))).value;

  // The tenant reads its DID back off the authenticated session — it is never
  // constructed from configuration, because a wrong DID would silently target
  // another tenant's namespace.
  const tenantId = tenantIdFromDid(did);
  if (expectedDid.length > 0 && did !== expectedDid) {
    throw new Error(`authenticated as ${did} but TENANT_DID is ${expectedDid}`);
  }

  const tenant = new TenantClient({
    environment,
    baseUrl: nodeUrl,
    endpoint: nodeUrl,
    tenantDid: did,
    t3n: client,
  });

  return { client, tenant, did, tenantId, environment, nodeUrl };
}

// ---------------------------------------------------------------------------
// 2. Register the built component
// ---------------------------------------------------------------------------

interface Registration {
  contractId: number;
  name: string;
  version: string;
  wasmBytes: number;
  wasmSha256: string;
}

const VERSION_NOT_HIGHER = /not higher than/i;
const ALREADY_EXISTS = /already exists|already registered|MapAlreadyExists/i;

async function register(
  session: TenantSession,
  canonicalName: string,
): Promise<Registration> {
  const wasm = await readFile(WASM_PATH);
  const wasmBytes = wasm.byteLength;
  const wasmSha256 = createHash("sha256").update(wasm).digest("hex");

  // An inventory read first: it tells us whether this tail is already known,
  // which is the difference between "registered" and "pre-existing" in the log.
  try {
    const inventory = await session.tenant.contracts.list();
    const existing = inventory.find((name) => name === canonicalName);
    note(
      existing === undefined
        ? `tail "${CONTRACT_TAIL}" is not yet registered for this tenant`
        : `tail "${CONTRACT_TAIL}" already present in the tenant inventory`,
    );
  } catch (error) {
    note(`inventory read skipped: ${errorText(error)}`);
  }

  try {
    const result = await session.tenant.contracts.register({
      tail: CONTRACT_TAIL,
      version: CONTRACT_VERSION,
      wasm,
    });
    return {
      contractId: result.contract_id,
      name: result.name,
      version: CONTRACT_VERSION,
      wasmBytes,
      wasmSha256,
    };
  } catch (error) {
    const message = errorText(error);
    if (VERSION_NOT_HIGHER.test(message)) {
      throw new Error(
        `${message}\n   Registration is immutable per version. Bump CONTRACT_VERSION in ` +
          `src/lib/interface.ts (single source of truth) and re-run.`,
      );
    }
    throw error;
  }
}

// ---------------------------------------------------------------------------
// 3. Create the KV maps
// ---------------------------------------------------------------------------

type MapOutcome = "created" | "exists" | "converged";

async function createMaps(
  session: TenantSession,
  contractId: number,
): Promise<Map<MapTailName, { outcome: MapOutcome; name: string }>> {
  const writers: WriterSet = { only: [contractId] };
  const results = new Map<MapTailName, { outcome: MapOutcome; name: string }>();

  for (const tail of MAP_TAILS) {
    const name = session.tenant.canonicalName(tail);
    // `canonicalName()` must agree with the contract's own naming, or the
    // contract would read a map that does not exist.
    const expected = `z:${session.tenantId}:${tail}`;
    if (name !== expected) {
      throw new Error(`canonical map name ${name} does not match the contract's ${expected}`);
    }

    try {
      await session.tenant.maps.create({
        tail,
        visibility: MAP_VISIBILITY,
        writers,
        readers: writers,
      });
      results.set(tail, { outcome: "created", name });
    } catch (error) {
      const message = errorText(error);
      if (!ALREADY_EXISTS.test(message)) throw error;
      // Idempotent re-run. Re-assert the ACL through `maps.update` so a map left
      // behind by an earlier run cannot keep a stale ACL and silently starve the
      // contract of reads.
      await session.tenant.maps.update(tail, {
        visibility: MAP_VISIBILITY,
        writers,
        readers: writers,
      });
      results.set(tail, { outcome: "converged", name });
    }
    detail("map", `${tail} → ${name}`);
  }

  return results;
}

type MapTailName = (typeof MAP_TAILS)[number];

// ---------------------------------------------------------------------------
// 4. Seed the policy document
// ---------------------------------------------------------------------------

/**
 * Write `policy` to `z:<tid>:policy` key `current` through the tenant
 * management surface (`map-entry-set`), which is the only write path that does
 * not require the contract to be invoked with a delegation grant.
 */
async function seedPolicy(
  session: TenantSession,
  policy: Policy,
): Promise<{ name: string; key: string; bytes: number }> {
  const name = session.tenant.canonicalName(MAP_TAIL.policy);
  const value = JSON.stringify(policy, null, 2);

  await session.tenant.executeControl("map-entry-set", {
    map_name: name,
    key: POLICY_KEY,
    value,
  });

  return { name, key: POLICY_KEY, bytes: Buffer.byteLength(value, "utf8") };
}

/** Read the entry back off the tenant surface, so the seed is evidence, not hope. */
async function verifyPolicy(session: TenantSession): Promise<string | null> {
  try {
    return await session.tenant.maps.entryGet(MAP_TAIL.policy, POLICY_KEY);
  } catch (error) {
    note(`read-back unavailable: ${errorText(error)}`);
    return null;
  }
}

// ---------------------------------------------------------------------------
// main
// ---------------------------------------------------------------------------

async function main(): Promise<void> {
  const tenantKey = requireEnv("TENANT_API_KEY");
  const expectedDid = process.env["TENANT_DID"]?.trim() ?? "";

  console.log("z-expense-guard deployment → T3N testnet");
  console.log(`   wasm                    ${WASM_PATH}`);

  step("Authenticate as the tenant");
  const session = await connectTenant(tenantKey, expectedDid);
  detail("did", session.did);
  detail("tenant id (z:…)", session.tenantId);
  detail("environment", session.environment);
  detail("node", session.nodeUrl);

  step("Register the WASM component");
  const canonicalName = session.tenant.canonicalName(CONTRACT_TAIL);
  const registration = await register(session, canonicalName);
  detail("contract name", registration.name);
  detail("contract_id", registration.contractId);
  detail("version", registration.version);
  detail("wasm bytes", registration.wasmBytes);
  detail("wasm sha256", registration.wasmSha256);

  // Persist what only this step can know. The numeric `contract_id` is what the
  // map ACLs take, and the node cannot hand it back afterwards — `contracts.list`
  // exposes no `contract_id` field (see BUGLOG.md, BUG-11). Without this record a
  // re-run would have to re-register the tail and silently orphan the ACLs below.
  // Public identifiers only: no key material ever reaches this file.
  await writeRegistration({
    recorded_at: new Date().toISOString(),
    environment: session.environment,
    tenant_did: session.did,
    tenant_id: session.tenantId,
    tail: CONTRACT_TAIL,
    name: registration.name,
    contract_id: registration.contractId,
    version: registration.version,
    wasm: {
      file: path.relative(REPO_ROOT, WASM_PATH),
      bytes: registration.wasmBytes,
      sha256: registration.wasmSha256,
    },
  });
  detail("registration record", path.relative(REPO_ROOT, REGISTRATION_FILE));

  step("Create the contract's KV maps");
  const maps = await createMaps(session, registration.contractId);
  detail("writers / readers", `only [${registration.contractId}] (${MAP_VISIBILITY})`);

  step("Seed the policy document");
  const policy = demoPolicy("USD");
  const seeded = await seedPolicy(session, policy);
  detail("map", seeded.name);
  detail("key", seeded.key);
  detail("bytes", seeded.bytes);
  detail("categories", Object.keys(policy.categories).join(", "));
  const readBack = await verifyPolicy(session);
  if (readBack !== null) {
    const parsed = JSON.parse(readBack) as Policy;
    detail(
      "read-back",
      `${parsed.base_currency} threshold=${parsed.approval_threshold} categories=${Object.keys(parsed.categories).length}`,
    );
  }

  step("Summary");
  detail("contract_id", registration.contractId);
  detail("contract name", registration.name);
  detail("version", registration.version);
  detail("wasm sha256", registration.wasmSha256);
  for (const [tail, result] of maps) {
    detail(`map ${tail}`, `${result.name} (${result.outcome}, ${MAP_VISIBILITY}, only [${registration.contractId}])`);
  }
  detail("policy entry", `${seeded.name} → key "${seeded.key}" (${seeded.bytes} bytes)`);
  console.log("\ndeploy: OK (exit 0)");
}

await main();
