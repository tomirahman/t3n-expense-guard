/**
 * Centralised configuration access.
 *
 * One module owns every environment variable, every path and every default, so
 * no script has to know how the harness is configured. Values are read lazily:
 * importing this module never throws, which is what lets `doctor` report on a
 * broken configuration instead of dying inside it.
 *
 * Secrets are read here and nowhere else, are never logged, and are never
 * written to disk — see README.md ("Handling of credentials").
 */
import "dotenv/config";
import path from "node:path";
import { fileURLToPath } from "node:url";

import type { Environment, WireScope } from "@terminal3/t3n-sdk";

import { CONTRACT_TAIL_DEFAULT, CONTRACT_VERSION_DEFAULT, FX_HOST } from "./interface.js";

/** Raised for a missing or malformed environment variable. */
export class ConfigError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "ConfigError";
  }
}

// ---------------------------------------------------------------------------
// Paths
// ---------------------------------------------------------------------------

/**
 * This module is `client/src/lib/` when run through tsx and `client/dist/lib/`
 * after `npm run build` — the same depth either way, so the derived roots below
 * hold in both.
 */
const MODULE_DIR = path.dirname(fileURLToPath(import.meta.url));

/** Repository root (`.../t3n-agent`), shared with the Rust half of the build. */
export const REPO_ROOT = path.resolve(MODULE_DIR, "..", "..", "..");

/** This package's root (`.../t3n-agent/client`). */
export const CLIENT_ROOT = path.join(REPO_ROOT, "client");

/** Local, git-ignored facts recorded by earlier steps (never secrets). */
export const ARTIFACTS_DIR = path.join(CLIENT_ROOT, "artifacts");

/** Demo transcripts, one JSON file per run. */
export const DEMO_OUTPUT_DIR = path.join(CLIENT_ROOT, "demo-output");

/** The built component registered in step 01. Read-only; owned by `contract/`. */
export const WASM_ARTIFACT = path.join(
  REPO_ROOT,
  "contract",
  "target",
  "wasm32-wasip2",
  "release",
  "z_expense_guard.wasm",
);

/** WIT world, used by `doctor` as a layout probe. */
export const WIT_WORLD = path.join(REPO_ROOT, "contract", "wit", "world.wit");

/** Normative interface contract, used by `doctor` as a layout probe. */
export const INTERFACE_DOC = path.join(REPO_ROOT, "docs", "INTERFACE.md");

// ---------------------------------------------------------------------------
// Raw access
// ---------------------------------------------------------------------------

/** Trimmed value of `name`, or `undefined` when unset or empty. */
export function readEnv(name: string): string | undefined {
  const raw = process.env[name];
  if (raw === undefined) return undefined;
  const trimmed = raw.trim();
  return trimmed.length === 0 ? undefined : trimmed;
}

/**
 * Value of `name`, or a {@link ConfigError} naming what to do about it.
 *
 * Also rejects an unfilled `.env.example` placeholder (`<...>`), the most
 * common way to get a confusing authentication failure.
 */
export function requireEnv(name: string): string {
  const value = readEnv(name);
  if (value === undefined) {
    throw new ConfigError(`${name} is not set — copy .env.example to .env and fill it in`);
  }
  if (value.startsWith("<") && value.endsWith(">")) {
    throw new ConfigError(`${name} still holds the .env.example placeholder ${value}`);
  }
  return value;
}

// ---------------------------------------------------------------------------
// Identities
// ---------------------------------------------------------------------------

/**
 * The three distinct sessions this harness uses. They are separate identities
 * with separate keys: the tenant owns the maps and registers the contract, the
 * user delegates, and the agent is the caller of record.
 */
export const IDENTITY_ENV_VAR = {
  tenant: "T3N_API_KEY",
  agent: "AGENT_KEY",
  user: "USER_KEY",
} as const;

export type Identity = keyof typeof IDENTITY_ENV_VAR;

export const IDENTITY_ROLE: Readonly<Record<Identity, string>> = {
  tenant: "control plane: creates maps, registers the contract, seeds secrets",
  agent: "caller of record: invokes the contract's six functions",
  user: "data owner: signs the delegation grant that authorises egress",
};

/** Raw private key for `identity`. Never log the result. */
export function identityKey(identity: Identity): string {
  return requireEnv(IDENTITY_ENV_VAR[identity]);
}

// ---------------------------------------------------------------------------
// Target cluster and contract
// ---------------------------------------------------------------------------

/**
 * Cluster to talk to. `testnet` is the documented default for the public SDK
 * artifact; `production` must be opted into explicitly.
 */
export function targetEnvironment(): Environment {
  const raw = readEnv("T3N_ENV") ?? "testnet";
  if (raw === "testnet" || raw === "production" || raw === "sandbox") return raw;
  throw new ConfigError(
    `T3N_ENV must be one of "testnet" | "production" | "sandbox" — got ${JSON.stringify(raw)}`,
  );
}

/** Registration tail; the canonical name becomes `z:<tid>:<tail>`. */
export function contractTail(): string {
  return readEnv("CONTRACT_TAIL") ?? CONTRACT_TAIL_DEFAULT;
}

/** Version to register. Must be plain `MAJOR.MINOR.PATCH`; the node parses SemVer. */
export function contractVersion(): string {
  const raw = readEnv("CONTRACT_VERSION") ?? CONTRACT_VERSION_DEFAULT;
  if (!/^\d+\.\d+\.\d+$/.test(raw)) {
    throw new ConfigError(
      `CONTRACT_VERSION must be a plain MAJOR.MINOR.PATCH semver (got ${JSON.stringify(raw)})`,
    );
  }
  return raw;
}

/** Base currency for FX normalisation, and the policy's `base_currency`. */
export function fxBaseCurrency(): string {
  return (readEnv("FX_BASE_CURRENCY") ?? "USD").toUpperCase();
}

// ---------------------------------------------------------------------------
// Egress
// ---------------------------------------------------------------------------

/** Approval webhook the contract POSTs to from `request-approval`. */
export function approvalWebhookUrl(): string {
  return requireEnv("APPROVAL_WEBHOOK_URL");
}

/** Hostname of {@link approvalWebhookUrl}, as it must appear in a grant. */
export function approvalWebhookHost(): string {
  const raw = approvalWebhookUrl();
  let parsed: URL;
  try {
    parsed = new URL(raw);
  } catch {
    throw new ConfigError(`APPROVAL_WEBHOOK_URL must be an absolute URL — got ${JSON.stringify(raw)}`);
  }
  return parsed.hostname;
}

/**
 * Every host the contract may dial, as the delegation grant's `allowed_hosts`.
 *
 * Egress is authorised per call from the *calling user's* grant, so a host
 * missing here is not a warning: the call fails with `host/http.egress_denied`.
 */
export function allowedHosts(): string[] {
  return [...new Set([FX_HOST, approvalWebhookHost()])];
}

// ---------------------------------------------------------------------------
// Delegation
// ---------------------------------------------------------------------------

/** Structural check for a {@link WireScope}, used for env and artifact parsing. */
export function isWireScope(value: unknown): value is WireScope {
  if (typeof value !== "object" || value === null) return false;
  const candidate: Record<string, unknown> = { ...value };
  const access = candidate["access"];
  return (
    typeof candidate["path"] === "string" &&
    Array.isArray(access) &&
    access.every((verb) => verb === "read" || verb === "write" || verb === "delete")
  );
}

/**
 * Org-data scopes carried by the delegation grant.
 *
 * Empty by default: z-expense-guard keeps its state in tenant-scoped KV maps
 * and reads no org-data scopes, so the least-privilege grant is one that confers
 * none. Extend via `DELEGATION_SCOPES` only when the contract grows a scope read.
 */
export function delegationScopes(): WireScope[] {
  const raw = readEnv("DELEGATION_SCOPES");
  if (raw === undefined) return [];

  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch {
    throw new ConfigError("DELEGATION_SCOPES must be a JSON array of WireScope objects");
  }
  if (!Array.isArray(parsed) || !parsed.every(isWireScope)) {
    throw new ConfigError(
      'DELEGATION_SCOPES must be a JSON array of {"path": string, "access": ["read"|"write"|"delete"]}',
    );
  }
  return parsed;
}
