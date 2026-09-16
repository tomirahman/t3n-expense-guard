/**
 * Preflight checks for the harness.
 *
 * `doctor` never touches a key and never authenticates: it inspects the
 * environment, the installed SDK, the repository layout and the built artefact,
 * and prints one PASS / WARN / FAIL line per check. WARN means "the step that
 * needs this will fail, but the rest of the harness is fine"; FAIL means the
 * harness cannot work at all.
 *
 * Exit code is 1 if any check FAILs, 0 otherwise, so it is usable as a gate:
 *
 *   npm run doctor && npm run register
 */
import { readFile, stat } from "node:fs/promises";
import path from "node:path";

import {
  CLIENT_ROOT,
  INTERFACE_DOC,
  WASM_ARTIFACT,
  WIT_WORLD,
  allowedHosts,
  approvalWebhookUrl,
  contractTail,
  contractVersion,
  delegationScopes,
  fxBaseCurrency,
  targetEnvironment,
  IDENTITY_ENV_VAR,
  IDENTITY_ROLE,
  requireEnv,
  type Identity,
} from "./lib/env.js";
import { CONTRACT_FUNCTIONS, MAP_TAIL, canonicalName } from "./lib/interface.js";
import { readRegistration } from "./lib/artifacts.js";

type CheckStatus = "pass" | "warn" | "fail";

interface CheckResult {
  readonly name: string;
  readonly status: CheckStatus;
  readonly detail: string;
}

const results: CheckResult[] = [];

function record(name: string, status: CheckStatus, detail: string): void {
  results.push({ name, status, detail });
}

/**
 * Run one check, converting an unexpected throw into a FAIL rather than a
 * crash — a doctor that dies on a broken configuration is useless exactly when
 * it is needed.
 */
async function check(name: string, run: () => Promise<string> | string): Promise<void> {
  try {
    record(name, "pass", await run());
  } catch (error) {
    record(name, "fail", error instanceof Error ? error.message : String(error));
  }
}

/** Run a check that reports WARN instead of FAIL when it throws. */
async function softCheck(name: string, run: () => Promise<string> | string): Promise<void> {
  try {
    record(name, "pass", await run());
  } catch (error) {
    record(name, "warn", error instanceof Error ? error.message : String(error));
  }
}

// ---------------------------------------------------------------------------
// Checks
// ---------------------------------------------------------------------------

const MIN_NODE_MAJOR = 20;

/** The SDK symbols this harness calls. A missing one breaks the build, not just a step. */
const REQUIRED_SDK_EXPORTS = [
  "T3nClient",
  "TenantClient",
  "setEnvironment",
  "getNodeUrl",
  "loadWasmComponent",
  "fetchTrustedManifest",
  "eth_get_address",
  "metamask_sign",
  "createEthAuthInput",
  "getContractVersion",
  "fanOutGrant",
] as const;

async function readJsonObject(file: string): Promise<Record<string, unknown>> {
  const raw = await readFile(file, "utf8");
  const parsed: unknown = JSON.parse(raw);
  if (typeof parsed !== "object" || parsed === null || Array.isArray(parsed)) {
    throw new Error(`${file} is not a JSON object`);
  }
  return { ...parsed };
}

/** Read a nested object without ever falling back to `any`. */
function nestedObject(source: Record<string, unknown>, key: string): Record<string, unknown> {
  const value = source[key];
  if (typeof value !== "object" || value === null || Array.isArray(value)) {
    throw new Error(`expected "${key}" to be a JSON object`);
  }
  return { ...value };
}

await check("node version", () => {
  const major = Number.parseInt(process.versions.node.split(".")[0] ?? "0", 10);
  if (major < MIN_NODE_MAJOR) {
    throw new Error(`node ${process.versions.node} is too old — ${MIN_NODE_MAJOR}+ required`);
  }
  return `node ${process.versions.node} (>= ${MIN_NODE_MAJOR} required)`;
});

await check("package layout", async () => {
  const pkg = await readJsonObject(path.join(CLIENT_ROOT, "package.json"));
  if (pkg["type"] !== "module") throw new Error('client/package.json must declare "type": "module"');
  const deps = nestedObject(pkg, "dependencies");
  const pinned = deps["@terminal3/t3n-sdk"];
  if (typeof pinned !== "string") throw new Error("client/package.json does not pin @terminal3/t3n-sdk");
  return `ESM ("type": "module"), @terminal3/t3n-sdk pinned to ${pinned}`;
});

await softCheck("sdk version", async () => {
  const pkg = await readJsonObject(path.join(CLIENT_ROOT, "package.json"));
  const deps = nestedObject(pkg, "dependencies");
  const pinned = deps["@terminal3/t3n-sdk"];

  const installedPath = path.join(CLIENT_ROOT, "node_modules", "@terminal3", "t3n-sdk", "package.json");
  let installed: string;
  try {
    const sdk = await readJsonObject(installedPath);
    installed = typeof sdk["version"] === "string" ? sdk["version"] : "unknown";
  } catch {
    throw new Error("node_modules/@terminal3/t3n-sdk is missing — run `npm install`");
  }
  if (installed !== pinned) {
    throw new Error(`installed ${installed} does not match the pin ${String(pinned)} — run \`npm install\``);
  }
  return `@terminal3/t3n-sdk ${installed} (matches the pin)`;
});

await check("sdk api surface", async () => {
  const sdk: Record<string, unknown> = { ...(await import("@terminal3/t3n-sdk")) };
  const missing = REQUIRED_SDK_EXPORTS.filter((symbol) => !(symbol in sdk));
  if (missing.length > 0) {
    throw new Error(`the installed SDK does not export: ${missing.join(", ")}`);
  }
  return `${REQUIRED_SDK_EXPORTS.length} required exports present`;
});

await check("repository layout", async () => {
  const required = [
    { file: WIT_WORLD, label: "contract/wit/world.wit" },
    { file: INTERFACE_DOC, label: "docs/INTERFACE.md" },
  ];
  const missing: string[] = [];
  for (const entry of required) {
    try {
      await readFile(entry.file, "utf8");
    } catch {
      missing.push(entry.label);
    }
  }
  if (missing.length > 0) {
    throw new Error(`missing ${missing.join(", ")} — is client/ inside the t3n-agent repository?`);
  }
  return "contract/wit/world.wit and docs/INTERFACE.md present";
});

await check("wasm artifact", async () => {
  let bytes: Buffer;
  try {
    bytes = await readFile(WASM_ARTIFACT);
  } catch {
    throw new Error(
      `${path.relative(CLIENT_ROOT, WASM_ARTIFACT)} not found — build it with ` +
        "`cargo build --release --target wasm32-wasip2` in contract/",
    );
  }
  if (bytes.byteLength < 8 || bytes.subarray(0, 4).toString("hex") !== "0061736d") {
    throw new Error(`${path.relative(CLIENT_ROOT, WASM_ARTIFACT)} is not a WebAssembly module`);
  }
  const { sha256Hex } = await import("./lib/artifacts.js");
  return `${(bytes.byteLength / 1024).toFixed(1)} KiB, sha256 ${sha256Hex(bytes).slice(0, 16)}…`;
});

// --- identities ------------------------------------------------------------

for (const identity of Object.keys(IDENTITY_ENV_VAR) as Identity[]) {
  await check(`env ${IDENTITY_ENV_VAR[identity]} (${identity})`, () => {
    requireEnv(IDENTITY_ENV_VAR[identity]);
    return `set — ${IDENTITY_ROLE[identity]}`;
  });
}

await check("env T3N_ENV", () => `target cluster: ${targetEnvironment()}`);

await check("env CONTRACT_TAIL", () => {
  const tail = contractTail();
  return `registered as ${canonicalName("<tid>", tail)}`;
});

await check("env CONTRACT_VERSION", () => contractVersion());

await check("env registry vs INTERFACE.md", async () => {
  const doc = await readFile(INTERFACE_DOC, "utf8");
  const missing = CONTRACT_FUNCTIONS.filter((fn) => !doc.includes(fn));
  if (missing.length > 0) throw new Error(`INTERFACE.md no longer documents: ${missing.join(", ")}`);
  return `${CONTRACT_FUNCTIONS.length} functions and ${Object.keys(MAP_TAIL).length} map tails match INTERFACE.md`;
});

await check("env FX_BASE_CURRENCY", () => fxBaseCurrency());

await softCheck("env APPROVAL_WEBHOOK_URL", () => {
  const url = approvalWebhookUrl();
  return `${url} — grant host allowlist will be [${allowedHosts().join(", ")}]`;
});

await check("delegation scopes", () => {
  const scopes = delegationScopes();
  return scopes.length === 0
    ? "empty (least privilege: the contract reads no org-data scopes)"
    : `${scopes.length} org-data scope(s) delegated`;
});

// --- artifacts and reachability -------------------------------------------

await softCheck("registration record", async () => {
  const record = await readRegistration();
  // The record also pins the exact bytes that were registered, so a rebuilt
  // component that was never re-registered shows up here instead of as a
  // confusing mismatch at invoke time.
  const built = await stat(WASM_ARTIFACT);
  if (built.size !== record.wasm.bytes) {
    throw new Error(
      `${WASM_ARTIFACT} is ${built.size} bytes but ${record.name} was registered from ` +
        `${record.wasm.bytes} — rebuild and re-run \`npm run register\` if the contract changed`,
    );
  }
  return `${record.name} @ ${record.version} → contract_id ${record.contract_id}`;
});

await softCheck("node reachability", async () => {
  const { getNodeUrl, setEnvironment } = await import("@terminal3/t3n-sdk");
  const environment = targetEnvironment();
  setEnvironment(environment);
  const nodeUrl = getNodeUrl();
  const response = await fetch(`${nodeUrl}/status`, { signal: AbortSignal.timeout(5000) });
  if (!response.ok) throw new Error(`${nodeUrl}/status answered HTTP ${response.status}`);
  return `${nodeUrl}/status answered HTTP ${response.status}`;
});

await softCheck("fx host reachability", async () => {
  const url = `https://open.er-api.com/v6/latest/${fxBaseCurrency()}`;
  const response = await fetch(url, { signal: AbortSignal.timeout(8000) });
  if (!response.ok) throw new Error(`${url} answered HTTP ${response.status}`);
  return `${url} answered HTTP ${response.status}`;
});

// ---------------------------------------------------------------------------
// Report
// ---------------------------------------------------------------------------

const SYMBOL: Readonly<Record<CheckStatus, string>> = { pass: "PASS", warn: "WARN", fail: "FAIL" };
const width = results.reduce((max, result) => Math.max(max, result.name.length), 0);

console.log(`z-expense-guard client — doctor (${results.length} checks)\n`);
for (const result of results) {
  console.log(`${SYMBOL[result.status]}  ${result.name.padEnd(width)}  ${result.detail}`);
}

const failed = results.filter((result) => result.status === "fail");
const warned = results.filter((result) => result.status === "warn");
console.log(
  `\n${results.length - failed.length - warned.length} passed, ${warned.length} warned, ${failed.length} failed`,
);

if (failed.length > 0) {
  console.log(`\nNot ready. First failure: ${failed[0]?.name}. See README.md for the setup steps.`);
  process.exitCode = 1;
}
