/**
 * Session helper — the one place a raw key becomes an authenticated client.
 *
 * Every step of the harness runs as one of three identities ({@link Identity}),
 * and all three are built identically: load the session WASM component, pin the
 * cluster's attested trust anchor, sign the challenge with the key, and read the
 * DID back off the authenticated session. Nothing tenant- or agent-specific is
 * configured here, because there is nothing to configure — the node resolves the
 * identity from the session itself.
 *
 * The private key is passed to the signer handler and nothing else. It is never
 * logged, never returned from this module, and never written to disk.
 */
import {
  T3nClient,
  createEthAuthInput,
  eth_get_address,
  fetchTrustedManifest,
  getNodeUrl,
  loadWasmComponent,
  metamask_sign,
  setEnvironment,
  type Environment,
  type GuestToHostHandlers,
} from "@terminal3/t3n-sdk";

import { identityKey, targetEnvironment, type Identity } from "./env.js";
import { tenantIdFromDid } from "./interface.js";

/** Raised when a session cannot be established; wraps the SDK's own error. */
export class SessionError extends Error {
  constructor(message: string, cause: unknown) {
    super(message, { cause });
    this.name = "SessionError";
  }
}

/** An authenticated identity, plus everything derived from it. */
export interface Session {
  /** Which of the three roles this session is. */
  readonly identity: Identity;
  /** Checksum-less lowercase `0x…` Ethereum address the key derives to. */
  readonly address: string;
  /** `did:t3n:<40-hex>` as returned by the node. */
  readonly did: string;
  /** The `<tid>` in `z:<tid>:…` — the DID suffix. */
  readonly tenantId: string;
  /** Cluster this session is bound to. */
  readonly environment: Environment;
  /** Node URL the client is talking to. */
  readonly nodeUrl: string;
  /** The authenticated client, ready for `execute` / `executeAndDecode`. */
  readonly client: T3nClient;
}

/**
 * Build and authenticate a session for `identity`.
 *
 * @throws {SessionError} when the handshake or the authentication fails — most
 *   commonly an unreachable node, an empty credit balance, or a key that is not
 *   the one the DID was claimed with.
 */
export async function connect(identity: Identity): Promise<Session> {
  const privateKey = identityKey(identity);
  const environment = targetEnvironment();

  // Global on purpose: `getNodeUrl()` and the default transport read this, so
  // it must be set before any client is constructed.
  setEnvironment(environment);
  const nodeUrl = getNodeUrl();

  let client: T3nClient;
  try {
    // The trust anchor is required at construction — the client throws without
    // it. It is fetched from the environment's published manifest and verified
    // against the node's DKG attestation during the handshake.
    const trustAnchor = await fetchTrustedManifest(environment);
    const wasmComponent = await loadWasmComponent();
    const address = eth_get_address(privateKey);
    const handlers: GuestToHostHandlers = {
      EthSign: metamask_sign(address, undefined, privateKey),
    };

    client = new T3nClient({ trustAnchor, wasmComponent, handlers });
    await client.handshake();
  } catch (cause) {
    throw new SessionError(
      `handshake with ${nodeUrl} failed for identity "${identity}" (${environment})`,
      cause,
    );
  }

  let did: string;
  try {
    const auth = await client.authenticate(createEthAuthInput(eth_get_address(privateKey)));
    did = auth.value;
  } catch (cause) {
    throw new SessionError(
      `authentication failed for identity "${identity}": the node rejected the EthSign challenge. ` +
        `Check that ${identity} has a funded, claimed key — an agent's balance is separate from its tenant's.`,
      cause,
    );
  }

  return {
    identity,
    address: eth_get_address(privateKey),
    did,
    tenantId: tenantIdFromDid(did),
    environment,
    nodeUrl,
    client,
  };
}

/** One-line, secret-free description of a session, for script headers. */
export function describe(session: Session): string {
  return `${session.identity.padEnd(6)} did=${session.did} node=${session.nodeUrl}`;
}
