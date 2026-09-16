/**
 * Frozen interface mirror — the TypeScript counterpart of `docs/INTERFACE.md`.
 *
 * Every name in this module is part of the contract between the Rust component
 * in `../contract` and this harness: function names, KV map names, JSON keys,
 * verdict strings and rule ids. `docs/INTERFACE.md` is normative. If a name
 * here ever disagrees with it, INTERFACE.md is right and this file is the bug.
 *
 * Nothing here may be renamed without updating INTERFACE.md first.
 */

/** WIT world name, and the tail the contract is registered under. */
export const CONTRACT_TAIL_DEFAULT = "expense-guard";

/** Version pinned by INTERFACE.md; overridable via CONTRACT_VERSION. */
export const CONTRACT_VERSION_DEFAULT = "0.1.0";

/** The six exported WIT functions, in interface order. */
export const CONTRACT_FUNCTIONS = [
  "health",
  "set-policy",
  "get-policy",
  "check-expense",
  "request-approval",
  "get-audit",
] as const;

export type ContractFunction = (typeof CONTRACT_FUNCTIONS)[number];

/**
 * KV map tails. Full names are `z:<tid>:<tail>`, built at runtime.
 *
 * Key layout, for reference — the harness never writes these directly, the
 * contract does through its `kv-store` import:
 *   policy  — `current`
 *   audit   — `seq:<000001>` (immutable record) and `exp:<expense_id>` (pointer)
 *   fx      — `<FROM>:<TO>`
 *   secrets — `approval_webhook_url` (below)
 */
export const MAP_TAIL = {
  policy: "policy",
  audit: "audit",
  fx: "fx",
  secrets: "secrets",
} as const;

export type MapTail = (typeof MAP_TAIL)[keyof typeof MAP_TAIL];

/** Key holding the approval webhook URL inside `z:<tid>:secrets`. */
export const SECRET_WEBHOOK_URL_KEY = "approval_webhook_url";

/** Host the contract dials for FX rates: keyless, PII-free. */
export const FX_HOST = "open.er-api.com";

/** Verdict strings, worst-last. */
export const VERDICTS = ["compliant", "needs_approval", "rejected"] as const;

export type Verdict = (typeof VERDICTS)[number];

/**
 * Worst rule wins: `rejected` > `needs_approval` > `compliant`. Kept as an
 * explicit table so scripts never re-derive the ordering by comparison order.
 */
export const VERDICT_RANK: Readonly<Record<Verdict, number>> = {
  compliant: 0,
  needs_approval: 1,
  rejected: 2,
};

/** Stable rule ids; never localised prose. */
export const RULE_IDS = [
  "disallowed_category",
  "over_category_limit",
  "over_approval_threshold",
  "possible_duplicate",
  "fx_unavailable",
  "unknown_category",
] as const;

export type RuleId = (typeof RULE_IDS)[number];

/** Stable error-string prefixes returned by the contract's `Err(String)`. */
export const ERROR_PREFIXES = ["policy:", "kv:", "fx:", "approval:", "input:"] as const;

// ---------------------------------------------------------------------------
// Wire types
// ---------------------------------------------------------------------------

export interface CategoryPolicy {
  /** Limit in `base_currency`. `0` disables the limit check for the category. */
  limit: number;
  allowed: boolean;
}

export interface Policy {
  base_currency: string;
  approval_threshold: number;
  default_category_limit: number;
  duplicate_window_days: number;
  fx_cache_ttl_hours: number;
  categories: Record<string, CategoryPolicy>;
}

/** `health` — liveness / self-description probe. No KV or egress access. */
export interface HealthResult {
  ok: boolean;
  contract: string;
  version: string;
  tenant_did: string;
  cluster_ts: number;
}

/** `set-policy` */
export interface SetPolicyResult {
  stored: boolean;
  base_currency: string;
  categories: number;
}

/** `check-expense` input. `employee_ref` is an opaque pseudonymous handle. */
export interface CheckExpenseInput {
  expense_id: string;
  employee_ref: string;
  amount: number;
  currency: string;
  category: string;
  vendor: string;
  incurred_on: string;
}

/** `check-expense` output. `base_amount` is null only on `fx_unavailable`. */
export interface CheckExpenseResult {
  expense_id: string;
  verdict: Verdict;
  base_currency: string;
  base_amount: number | null;
  fx_rate: number | null;
  fx_source: string;
  rule_hits: RuleId[];
  ledger_seq: number;
  evaluated_at: number;
}

/** `request-approval` input. */
export interface RequestApprovalInput {
  expense_id: string;
  /** Resolve `{{profile.*}}` markers host-side from the calling user's profile. */
  use_profile: boolean;
  note?: string;
}

/** `request-approval` output. */
export interface RequestApprovalResult {
  expense_id: string;
  status: string;
  http_code: number;
  approval_ref: string;
  ledger_seq: number;
}

/** One immutable audit-ledger record. */
export interface AuditRecord {
  seq: number;
  expense_id: string;
  employee_ref: string;
  verdict: Verdict;
  base_amount: number | null;
  category: string;
  vendor: string;
  rule_hits: RuleId[];
  recorded_at: number;
  contract_version: string;
}

/** `get-audit` input; every field is optional. */
export interface GetAuditInput {
  expense_id?: string;
  employee_ref?: string;
  limit?: number;
}

/** `get-audit` output — newest first. */
export interface AuditResult {
  count: number;
  entries: AuditRecord[];
}

// ---------------------------------------------------------------------------
// Naming helpers
// ---------------------------------------------------------------------------

/** `z:<tid>:<tail>` — the only spelling of a contract or map name we emit. */
export function canonicalName(tenantId: string, tail: string): string {
  return `z:${tenantId}:${tail}`;
}

/**
 * `did:t3n:<tid>` → `<tid>`. The contract host hands the component raw tenant
 * DID bytes, which it hex-encodes exactly once; the DID suffix is that same
 * hex string, so this is the single translation between the two.
 */
export function tenantIdFromDid(did: string): string {
  const prefix = "did:t3n:";
  return did.startsWith(prefix) ? did.slice(prefix.length) : did;
}

/**
 * Policy mirroring the INTERFACE.md example, retargeted at `baseCurrency`.
 * `approval_threshold` and per-category `limit` are in `base_currency`.
 */
export function demoPolicy(baseCurrency: string): Policy {
  return {
    base_currency: baseCurrency,
    approval_threshold: 500,
    default_category_limit: 250,
    duplicate_window_days: 30,
    fx_cache_ttl_hours: 6,
    categories: {
      travel: { limit: 1500, allowed: true },
      software: { limit: 600, allowed: true },
      entertainment: { limit: 0, allowed: false },
    },
  };
}
