import { flatten } from "./otlp.js";
import { insertSpans, traces, trace, spans, stats } from "./d1.js";
import type { Env } from "./d1.js";

export type { Env };

// Constant-time-ish bearer token comparison: equal lengths + XOR accumulation.
// Not cryptographically strict, but prevents trivial timing oracle via early exit.
function tokenMatches(actual: string, expected: string): boolean {
  if (actual.length !== expected.length) return false;
  let diff = 0;
  for (let i = 0; i < actual.length; i++) {
    diff |= actual.charCodeAt(i) ^ expected.charCodeAt(i);
  }
  return diff === 0;
}

function unauthorized(): Response {
  return new Response(JSON.stringify({ error: "Unauthorized" }), {
    status: 401,
    headers: { "Content-Type": "application/json" },
  });
}

function notFound(): Response {
  return new Response(JSON.stringify({ error: "Not found" }), {
    status: 404,
    headers: { "Content-Type": "application/json" },
  });
}

function jsonOk(data: unknown): Response {
  return new Response(JSON.stringify(data), {
    status: 200,
    headers: { "Content-Type": "application/json" },
  });
}

function checkAuth(req: Request, env: Env): boolean {
  const authHeader = req.headers.get("Authorization");
  if (!authHeader) return false;
  const prefix = "Bearer ";
  if (!authHeader.startsWith(prefix)) return false;
  const token = authHeader.slice(prefix.length);
  return tokenMatches(token, env.GENTLY_TOKEN);
}

export default {
  async fetch(req: Request, env: Env): Promise<Response> {
    try {
      if (!checkAuth(req, env)) return unauthorized();

      const url = new URL(req.url);
      const pathname = url.pathname;

      if (req.method === "POST" && pathname === "/v1/traces") {
        const body = await req.json();
        const rows = flatten(body as Parameters<typeof flatten>[0]);
        await insertSpans(env, rows);
        // Log the negotiated wire protocol (HTTP/3 when QUIC is used) so the
        // transport can be confirmed end-to-end via `wrangler tail`. Also echoed
        // in the response for direct probes.
        const httpProtocol = req.cf?.httpProtocol ?? "unknown";
        console.log(JSON.stringify({ ev: "ingest", httpProtocol, spans: rows.length }));
        return jsonOk({ partialSuccess: {}, httpProtocol });
      }

      // Lightweight probe: returns the negotiated protocol for this request.
      if (req.method === "GET" && pathname === "/v1/whoami") {
        return jsonOk({ httpProtocol: req.cf?.httpProtocol ?? "unknown" });
      }

      if (req.method === "GET" && pathname === "/v1/query") {
        const op = url.searchParams.get("op");

        if (op === "traces") {
          const result = await traces(env, {
            limit: url.searchParams.get("limit"),
            since: url.searchParams.get("since"),
            harness: url.searchParams.get("harness"),
          });
          return jsonOk(result);
        }

        if (op === "trace") {
          const trace_id = url.searchParams.get("trace_id") ?? "";
          const result = await trace(env, trace_id);
          return jsonOk(result);
        }

        if (op === "spans") {
          const result = await spans(env, {
            trace_id: url.searchParams.get("trace_id"),
            tool_name: url.searchParams.get("tool_name"),
            status: url.searchParams.get("status"),
            since: url.searchParams.get("since"),
            limit: url.searchParams.get("limit"),
          });
          return jsonOk(result);
        }

        if (op === "stats") {
          const result = await stats(env);
          return jsonOk(result);
        }

        return notFound();
      }

      return notFound();
    } catch (_err) {
      return new Response(JSON.stringify({ error: "Internal server error" }), {
        status: 500,
        headers: { "Content-Type": "application/json" },
      });
    }
  },
};
