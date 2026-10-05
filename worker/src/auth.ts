/** Runtime admission only. Raw decryption keys never belong in Worker bindings. */
export type Capability = "ingest" | "read";
export interface Principal {
  tenant_id: string;
  device_id: string;
  capabilities: Capability[];
}
interface Host extends Principal { token: string }

export const ID_PATTERN = /^[A-Za-z0-9_-]{1,64}$/;

// Compare the complete bearer rather than returning at the first differing byte.
function tokenMatches(actual: string, expected: string): boolean {
  if (actual.length !== expected.length) return false;
  let difference = 0;
  for (let i = 0; i < actual.length; i++) {
    difference |= actual.charCodeAt(i) ^ expected.charCodeAt(i);
  }
  return difference === 0;
}

function configuredHosts(configuration: string): Host[] | null {
  try {
    const value: unknown = JSON.parse(configuration);
    if (!Array.isArray(value) || value.length === 0) return null;
    const tokens = new Set<string>();
    for (const entry of value) {
      if (!entry || typeof entry !== "object" || Array.isArray(entry)) return null;
      if (Object.keys(entry).some(key => !["token", "tenant_id", "device_id", "capabilities"].includes(key))) return null;
      if (typeof entry.token !== "string" || !entry.token || entry.token.length > 4096 || tokens.has(entry.token)) return null;
      if (typeof entry.tenant_id !== "string" || !ID_PATTERN.test(entry.tenant_id)) return null;
      if (typeof entry.device_id !== "string" || !ID_PATTERN.test(entry.device_id)) return null;
      if (!Array.isArray(entry.capabilities) || entry.capabilities.length === 0 ||
          entry.capabilities.some((capability: unknown) => capability !== "ingest" && capability !== "read")) return null;
      tokens.add(entry.token);
    }
    return value as Host[];
  } catch {
    return null;
  }
}

export function authenticate(request: Request, configuration: string): Principal | null {
  const authorization = request.headers.get("Authorization");
  if (!authorization?.startsWith("Bearer ")) return null;
  const token = authorization.slice("Bearer ".length);
  const hosts = configuredHosts(configuration);
  if (!hosts) return null;
  let matchingHost: Host | undefined;
  for (const host of hosts) {
    if (tokenMatches(token, host.token)) matchingHost = host;
  }
  if (!matchingHost) return null;
  return {
    tenant_id: matchingHost.tenant_id,
    device_id: matchingHost.device_id,
    capabilities: matchingHost.capabilities,
  };
}
