/**
 * Local records passed between steps.
 *
 * Step 01 learns the contract's canonical name and numeric id; step 02 needs
 * both to build map ACLs and step 03 needs the name to scope the grant. Rather
 * than re-deriving them (the numeric id cannot be derived at all), each step
 * writes what it learned to a small JSON file under `client/artifacts/` and the
 * next step reads it back.
 *
 * Two rules hold for everything in this module:
 *
 *   - **No secrets.** Keys, signatures and session tokens never reach these
 *     files. DIDs, names and ids are public identifiers.
 *   - **Fail loudly.** A missing or stale file raises a {@link ConfigError}
 *     naming the command to run, instead of letting a later step fail somewhere
 *     far away from the cause.
 */
import { createHash } from "node:crypto";
import { access, mkdir, readFile, writeFile } from "node:fs/promises";
import path from "node:path";

import type { WireScope } from "@terminal3/t3n-sdk";

import { ARTIFACTS_DIR, ConfigError, DEMO_OUTPUT_DIR, isWireScope } from "./env.js";

/** Identity of the WASM actually registered, so a stale build is visible. */
export interface WasmFingerprint {
  /** Path relative to the repository root. */
  file: string;
  bytes: number;
  sha256: string;
}

/** Recorded by step 01. */
export interface RegistrationRecord {
  recorded_at: string;
  environment: string;
  tenant_did: string;
  tenant_id: string;
  /** Registration tail, e.g. `expense-guard`. */
  tail: string;
  /** Canonical name, `z:<tid>:<tail>`. This is the `contract_id` on the wire. */
  name: string;
  /**
   * Stable monotonic numeric id assigned at registration. Map ACLs
   * (`WriterSet` / `ReaderSet`) take these numbers, not names — see BUGLOG.md.
   */
  contract_id: number;
  version: string;
  wasm: WasmFingerprint;
}

/** Recorded by step 03. */
export interface DelegationRecord {
  recorded_at: string;
  environment: string;
  contract_name: string;
  contract_version: string;
  /** The agent DID the grant authorises. */
  grantee: string;
  /** The user DID that signed it. */
  delegator: string;
  /** Which write path was used, and why — see README.md. */
  path_used: string;
  functions: string[];
  allowed_hosts: string[];
  scopes: WireScope[];
  /** Rows the read-merge-write reported as preserved from the previous policy. */
  preserved_rows: string[];
}

export const REGISTRATION_FILE = path.join(ARTIFACTS_DIR, "registration.json");
export const DELEGATION_FILE = path.join(ARTIFACTS_DIR, "delegation.json");

const RUN_REGISTER_HINT = "run `npm run register`";
const RUN_DELEGATE_HINT = "run `npm run delegate`";

/** SHA-256 of a byte buffer, lowercase hex. Used to fingerprint the WASM. */
export function sha256Hex(bytes: Uint8Array): string {
  return createHash("sha256").update(bytes).digest("hex");
}

/** Filesystem-safe UTC timestamp, e.g. `2026-09-16T12-34-56-789Z`. */
export function timestampSlug(date: Date): string {
  return date.toISOString().replaceAll(":", "-").replace(".", "-");
}

// ---------------------------------------------------------------------------
// JSON plumbing
// ---------------------------------------------------------------------------

function isObject(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

/**
 * Raised for a file that exists but cannot be trusted: a hand-edited or
 * older-layout artifact must be regenerated, never partially read.
 */
function staleFileError(where: string, hint: string): ConfigError {
  return new ConfigError(`${where} is unreadable or from an older layout — delete it and ${hint}`);
}

function requireString(source: Record<string, unknown>, key: string, where: string): string {
  const value = source[key];
  if (typeof value !== "string" || value.length === 0) {
    throw new ConfigError(`${where} is missing a non-empty string "${key}"`);
  }
  return value;
}

function requireNumber(source: Record<string, unknown>, key: string, where: string): number {
  const value = source[key];
  if (typeof value !== "number" || !Number.isFinite(value)) {
    throw new ConfigError(`${where} is missing a number "${key}"`);
  }
  return value;
}

function requireStringArray(source: Record<string, unknown>, key: string, where: string): string[] {
  const value = source[key];
  if (!Array.isArray(value) || !value.every((item) => typeof item === "string")) {
    throw new ConfigError(`${where} is missing a string array "${key}"`);
  }
  return value;
}

function requireWireScopes(source: Record<string, unknown>, key: string, where: string): WireScope[] {
  const value = source[key];
  if (!Array.isArray(value) || !value.every(isWireScope)) {
    throw new ConfigError(`${where} is missing a WireScope array "${key}"`);
  }
  return value;
}

async function writeJsonFile(file: string, value: unknown): Promise<void> {
  await mkdir(path.dirname(file), { recursive: true });
  await writeFile(file, `${JSON.stringify(value, null, 2)}\n`, "utf8");
}

async function readJsonFile(file: string, hint: string): Promise<unknown> {
  let raw: string;
  try {
    raw = await readFile(file, "utf8");
  } catch {
    throw new ConfigError(`${file} does not exist — ${hint}`);
  }
  try {
    return JSON.parse(raw);
  } catch {
    throw new ConfigError(`${file} is not valid JSON — delete it and ${hint}`);
  }
}

async function fileExists(file: string): Promise<boolean> {
  try {
    await access(file);
    return true;
  } catch {
    return false;
  }
}

// ---------------------------------------------------------------------------
// Registration record (step 01 → steps 02, 03, 04)
// ---------------------------------------------------------------------------

/** Write the registration record for later steps. */
export async function writeRegistration(record: RegistrationRecord): Promise<void> {
  await writeJsonFile(REGISTRATION_FILE, record);
}

/** Read the registration record, or fail with the command that creates it. */
export async function readRegistration(): Promise<RegistrationRecord> {
  const where = REGISTRATION_FILE;
  const parsed = await readJsonFile(where, RUN_REGISTER_HINT);
  if (!isObject(parsed) || !isObject(parsed["wasm"])) {
    throw staleFileError(where, RUN_REGISTER_HINT);
  }

  const tenantId = requireString(parsed, "tenant_id", where);
  const tail = requireString(parsed, "tail", where);
  const name = requireString(parsed, "name", where);

  // The naming fields must agree, or the map ACLs and the grant would target
  // different things while both looking plausible.
  const expected = `z:${tenantId}:${tail}`;
  if (name !== expected) {
    throw new ConfigError(
      `${where} is inconsistent: name ${name} is not the canonical ${expected} — delete it and ${RUN_REGISTER_HINT}`,
    );
  }

  return {
    recorded_at: requireString(parsed, "recorded_at", where),
    environment: requireString(parsed, "environment", where),
    tenant_did: requireString(parsed, "tenant_did", where),
    tenant_id: tenantId,
    tail,
    name,
    contract_id: requireNumber(parsed, "contract_id", where),
    version: requireString(parsed, "version", where),
    wasm: {
      file: requireString(parsed["wasm"], "file", where),
      bytes: requireNumber(parsed["wasm"], "bytes", where),
      sha256: requireString(parsed["wasm"], "sha256", where),
    },
  };
}

// ---------------------------------------------------------------------------
// Delegation record (step 03 → step 04)
// ---------------------------------------------------------------------------

/** Write the delegation record as evidence of the authority a run had. */
export async function writeDelegation(record: DelegationRecord): Promise<void> {
  await writeJsonFile(DELEGATION_FILE, record);
}

/** Read the delegation record, or `null` when step 03 has not run yet. */
export async function readDelegation(): Promise<DelegationRecord | null> {
  const where = DELEGATION_FILE;
  if (!(await fileExists(where))) return null;

  const parsed = await readJsonFile(where, RUN_DELEGATE_HINT);
  if (!isObject(parsed)) throw staleFileError(where, RUN_DELEGATE_HINT);

  return {
    recorded_at: requireString(parsed, "recorded_at", where),
    environment: requireString(parsed, "environment", where),
    contract_name: requireString(parsed, "contract_name", where),
    contract_version: requireString(parsed, "contract_version", where),
    grantee: requireString(parsed, "grantee", where),
    delegator: requireString(parsed, "delegator", where),
    path_used: requireString(parsed, "path_used", where),
    functions: requireStringArray(parsed, "functions", where),
    allowed_hosts: requireStringArray(parsed, "allowed_hosts", where),
    scopes: requireWireScopes(parsed, "scopes", where),
    preserved_rows: requireStringArray(parsed, "preserved_rows", where),
  };
}

// ---------------------------------------------------------------------------
// Guards and paths
// ---------------------------------------------------------------------------

/**
 * Guard against a record left behind by a different identity.
 *
 * The recorded names embed the tenant `<tid>`, so switching `T3N_API_KEY`
 * changes every name; catching that here turns a confusing "map not found" or
 * "contract not registered" into one clear instruction.
 */
export function assertSameTenant(
  recordedTenantId: string,
  sessionTenantId: string,
  artifactName: string,
): void {
  if (recordedTenantId !== sessionTenantId) {
    throw new ConfigError(
      `${artifactName} was recorded for tenant ${recordedTenantId} but this session resolves to ` +
        `${sessionTenantId} — re-run \`npm run register\`, then \`npm run maps\` and \`npm run delegate\``,
    );
  }
}

/** Directory for demo transcripts, created on demand. */
export async function ensureDemoOutputDir(): Promise<string> {
  await mkdir(DEMO_OUTPUT_DIR, { recursive: true });
  return DEMO_OUTPUT_DIR;
}
