import { flatten } from "./otlp.js";
import { insertSpans, traces, trace, tracePage, spans, stats } from "./d1.js";
import type { Env } from "./d1.js";
import { authenticate, ID_PATTERN } from "./auth.js";
import { ClientError, jsonResponse, readJson } from "./http.js";
import { insertRawObject, rawObject, validateRawObject } from "./raw-values.js";

export type { Env };

export default {
  async fetch(req: Request, env: Env): Promise<Response> {
    try {
      const principal = authenticate(req, env.GENTLY_HOSTS);
      if (!principal) return jsonResponse({ error: "Unauthorized" }, 401);

      const url = new URL(req.url);
      const pathname = url.pathname;
      const requestedTenant = url.searchParams.get("tenant_id");
      if (!requestedTenant || !ID_PATTERN.test(requestedTenant) || url.searchParams.getAll("tenant_id").length !== 1) {
        throw new ClientError(400, "A valid tenant_id is required");
      }
      if (requestedTenant !== principal.tenant_id) throw new ClientError(403, "Forbidden");
      const tenantId = principal.tenant_id;
      const requireCapability = (capability: "ingest" | "read") => {
        if (!principal.capabilities.includes(capability)) throw new ClientError(403, "Forbidden");
      };

      if (req.method === "POST" && pathname === "/v1/raw-values") {
        requireCapability("ingest");
        const object = validateRawObject(await readJson(req), principal);
        await insertRawObject(env, object);
        return jsonResponse({ raw_ref: object.context.raw_ref });
      }
      if (req.method === "GET" && pathname.startsWith("/v1/raw-values/")) {
        requireCapability("read");
        const object = await rawObject(env, tenantId, pathname.slice("/v1/raw-values/".length));
        return object ? jsonResponse(object) : jsonResponse({ error: "Not found" }, 404);
      }

      if (req.method === "POST" && pathname === "/v1/traces") {
        requireCapability("ingest");
        const body = await readJson(req);
        const rows = flatten(body as Parameters<typeof flatten>[0], principal);
        await insertSpans(env, tenantId, principal.device_id, rows);
        // Log the negotiated wire protocol (HTTP/3 when QUIC is used) so the
        // transport can be confirmed end-to-end via `wrangler tail`. Also echoed
        // in the response for direct probes.
        const httpProtocol = req.cf?.httpProtocol ?? "unknown";
        console.log(JSON.stringify({ ev: "ingest", httpProtocol, spans: rows.length }));
        return jsonResponse({ partialSuccess: {}, httpProtocol });
      }

      // Lightweight probe: returns the negotiated protocol for this request.
      if (req.method === "GET" && pathname === "/v1/whoami") {
        return jsonResponse({ httpProtocol: req.cf?.httpProtocol ?? "unknown" });
      }

      if (req.method === "GET" && pathname === "/v1/query") {
        requireCapability("read");
        const op = url.searchParams.get("op");

        if (op === "traces") {
          const result = await traces(env, tenantId, {
            limit: url.searchParams.get("limit"),
            harness: url.searchParams.get("harness"),
            session_id: url.searchParams.get("session_id"),
            since: url.searchParams.get("since"),
            until: url.searchParams.get("until"),
            order: url.searchParams.get("order"),
          });
          return jsonResponse(result);
        }

        if (op === "trace") {
          const trace_id = url.searchParams.get("trace_id") ?? "";
          const page = url.searchParams.get("page");
          if (page !== null && page !== "1") throw new ClientError(400, "Invalid page mode");
          const result = page === "1"
            ? await tracePage(env, tenantId, trace_id, url.searchParams.get("cursor"), url.searchParams.get("limit"))
            : await trace(env, tenantId, trace_id);
          return jsonResponse(result);
        }

        if (op === "spans") {
          const result = await spans(env, tenantId, {
            trace_id: url.searchParams.get("trace_id"),
            session_id: url.searchParams.get("session_id"),
            harness: url.searchParams.get("harness"),
            tool_name: url.searchParams.get("tool_name"),
            name: url.searchParams.get("name"),
            status: url.searchParams.get("status"),
            kind: url.searchParams.get("kind"),
            since: url.searchParams.get("since"),
            until: url.searchParams.get("until"),
            limit: url.searchParams.get("limit"),
            order: url.searchParams.get("order"),
          });
          return jsonResponse(result);
        }

        if (op === "stats") {
          const result = await stats(env, tenantId);
          return jsonResponse(result);
        }

        return jsonResponse({ error: "Not found" }, 404);
      }

      return jsonResponse({ error: "Not found" }, 404);
    } catch (error) {
      if (error instanceof ClientError) return jsonResponse({ error: error.message }, error.status);
      return jsonResponse({ error: "Internal server error" }, 500);
    }
  },
};
