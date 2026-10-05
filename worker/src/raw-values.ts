import type { Env } from "./d1.js";
import type { Principal } from "./auth.js";
import { ID_PATTERN } from "./auth.js";
import { ClientError } from "./http.js";

export const RAW_REF_PATTERN = /^[0-9a-f]{32}$/;
export const MAX_CIPHERTEXT_BYTES = 512 * 1024;
const AGE_MAGIC = "age-encryption.org/v1\n";

export interface RawContext {
  tenant_id: string;
  device_id: string;
  key_epoch: number;
  raw_ref: string;
  session_id: string;
  harness: string;
  event: string;
}
export interface RawObject {
  version: 1;
  context: RawContext;
  ciphertext_b64: string;
}

function exactObject(value: unknown, keys: string[]): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value) &&
    Object.keys(value).length === keys.length && Object.keys(value).every(key => keys.includes(key));
}

/** Structural transport guard only; enrolled clients authenticate and decrypt age. */
export function validateRawObject(value: unknown, principal: Principal): RawObject {
  if (!exactObject(value, ["version", "context", "ciphertext_b64"]) || value.version !== 1 ||
      !exactObject(value.context, ["tenant_id", "device_id", "key_epoch", "raw_ref", "session_id", "harness", "event"])) {
    throw new ClientError(400, "Invalid encrypted raw object");
  }
  const context = value.context;
  for (const field of ["tenant_id", "device_id", "raw_ref", "session_id", "harness", "event"]) {
    const text = context[field];
    if (typeof text !== "string" || text.length === 0 || new TextEncoder().encode(text).length > 256 || /\p{Cc}/u.test(text) ||
        Array.from(text).some(character => character.length === 1 && character.charCodeAt(0) >= 0xd800 && character.charCodeAt(0) <= 0xdfff)) {
      throw new ClientError(400, "Invalid encrypted raw context");
    }
  }
  if (!ID_PATTERN.test(context.tenant_id as string) || !ID_PATTERN.test(context.device_id as string) ||
      !RAW_REF_PATTERN.test(context.raw_ref as string) ||
      typeof context.key_epoch !== "number" || !Number.isSafeInteger(context.key_epoch) || context.key_epoch <= 0) {
    throw new ClientError(400, "Invalid encrypted raw context");
  }
  if (context.tenant_id !== principal.tenant_id || context.device_id !== principal.device_id) {
    throw new ClientError(403, "Forbidden");
  }
  const encoded = value.ciphertext_b64;
  if (typeof encoded !== "string" || encoded.length === 0) throw new ClientError(400, "Invalid ciphertext");
  if (encoded.length > Math.ceil(MAX_CIPHERTEXT_BYTES / 3) * 4) throw new ClientError(413, "Ciphertext too large");
  let ciphertext: string;
  try {
    ciphertext = atob(encoded);
    if (btoa(ciphertext) !== encoded) throw new Error("Noncanonical base64");
  } catch {
    throw new ClientError(400, "Invalid ciphertext");
  }
  if (ciphertext.length > MAX_CIPHERTEXT_BYTES) throw new ClientError(413, "Ciphertext too large");
  validateAgeStructure(ciphertext);
  // Normalize property order so differently ordered JSON retries remain identical.
  return {
    version: 1,
    context: {
      tenant_id: context.tenant_id as string,
      device_id: context.device_id as string,
      key_epoch: context.key_epoch,
      raw_ref: context.raw_ref as string,
      session_id: context.session_id as string,
      harness: context.harness as string,
      event: context.event as string,
    },
    ciphertext_b64: encoded,
  };
}

// Parse the public framing from https://c2sp.org/age@v1.1.0. This verifies
// syntax and lengths only: a reader must authenticate the MAC and payload.
function validateAgeStructure(ciphertext: string): void {
  const invalid = () => new ClientError(400, "Invalid ciphertext format");
  if (!ciphertext.startsWith(AGE_MAGIC)) throw invalid();
  let offset = AGE_MAGIC.length;
  let stanzas = 0;
  const line = (): string => {
    const end = ciphertext.indexOf("\n", offset);
    if (end < 0) throw invalid();
    const result = ciphertext.slice(offset, end);
    offset = end + 1;
    return result;
  };
  const unpadded = (encoded: string, expectedLength?: number): string => {
    if (!/^[A-Za-z0-9+/]*$/.test(encoded) || encoded.length % 4 === 1) throw invalid();
    let decoded: string;
    try {
      decoded = atob(encoded + "=".repeat((4 - encoded.length % 4) % 4));
    } catch { throw invalid(); }
    if (btoa(decoded).replace(/=+$/, "") !== encoded ||
        (expectedLength !== undefined && decoded.length !== expectedLength)) throw invalid();
    return decoded;
  };
  while (true) {
    const argumentsLine = line();
    if (argumentsLine.startsWith("--- ")) {
      if (stanzas === 0 || argumentsLine.length !== 47) throw invalid();
      unpadded(argumentsLine.slice(4), 32);
      // A 16-byte nonce and at least one 16-byte authentication tag follow.
      if (ciphertext.length - offset < 32) throw invalid();
      return;
    }
    if (!/^-> [!-~]+(?: [!-~]+)*$/.test(argumentsLine)) throw invalid();
    const args = argumentsLine.slice(3).split(" ");
    if (args[0] === "scrypt") throw invalid();
    stanzas += 1;
    let encodedBody = "";
    while (true) {
      const bodyLine = line();
      if (bodyLine.length > 64 || !/^[A-Za-z0-9+/]*$/.test(bodyLine)) throw invalid();
      encodedBody += bodyLine;
      if (bodyLine.length < 64) break;
    }
    const body = unpadded(encodedBody);
    if (args[0] === "X25519") {
      if (args.length !== 2 || body.length !== 32) throw invalid();
      unpadded(args[1], 32);
    } else if (args[0] === "p256tag") {
      if (args.length !== 3 || body.length !== 32) throw invalid();
      unpadded(args[1], 4);
      unpadded(args[2], 65);
    }
  }
}

export async function insertRawObject(env: Env, object: RawObject): Promise<void> {
  const envelope = JSON.stringify(object);
  await env.DB.prepare(
    `INSERT INTO raw_values (tenant_id, raw_ref, device_id, key_epoch, envelope_json, created_unix_nano)
     VALUES (?, ?, ?, ?, ?, ?) ON CONFLICT(tenant_id, raw_ref) DO NOTHING`,
  ).bind(object.context.tenant_id, object.context.raw_ref, object.context.device_id,
    object.context.key_epoch, envelope, String(Date.now() * 1_000_000)).run();
  const stored = await env.DB.prepare("SELECT envelope_json FROM raw_values WHERE tenant_id = ? AND raw_ref = ?")
    .bind(object.context.tenant_id, object.context.raw_ref).first<{ envelope_json: string }>();
  if (stored?.envelope_json !== envelope) throw new ClientError(409, "Raw reference already exists");
}

export async function rawObject(env: Env, tenantId: string, rawRef: string): Promise<RawObject | null> {
  if (!RAW_REF_PATTERN.test(rawRef)) throw new ClientError(400, "Invalid raw reference");
  const stored = await env.DB.prepare("SELECT envelope_json FROM raw_values WHERE tenant_id = ? AND raw_ref = ?")
    .bind(tenantId, rawRef).first<{ envelope_json: string }>();
  return stored ? JSON.parse(stored.envelope_json) as RawObject : null;
}
