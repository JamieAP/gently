export const MAX_REQUEST_BYTES = 1024 * 1024;

export class ClientError extends Error {
  constructor(public readonly status: number, message: string) { super(message); }
}

export function jsonResponse(data: unknown, status = 200): Response {
  return new Response(JSON.stringify(data), {
    status,
    headers: {
      "Content-Type": "application/json",
      "Cache-Control": "no-store",
      "X-Content-Type-Options": "nosniff",
    },
  });
}

/** Bound bytes while reading; Content-Length alone cannot bound streamed bodies. */
export async function readJson(request: Request): Promise<unknown> {
  const contentLength = request.headers.get("Content-Length");
  if (contentLength && Number(contentLength) > MAX_REQUEST_BYTES) {
    throw new ClientError(413, "Request too large");
  }
  if (!request.body) throw new ClientError(400, "Invalid JSON");
  const reader = request.body.getReader();
  const chunks: Uint8Array[] = [];
  let total = 0;
  try {
    while (true) {
      const { done, value } = await reader.read();
      if (done) break;
      total += value.byteLength;
      if (total > MAX_REQUEST_BYTES) {
        await reader.cancel();
        throw new ClientError(413, "Request too large");
      }
      chunks.push(value);
    }
    const bytes = new Uint8Array(total);
    let offset = 0;
    for (const chunk of chunks) { bytes.set(chunk, offset); offset += chunk.byteLength; }
    return JSON.parse(new TextDecoder("utf-8", { fatal: true, ignoreBOM: false }).decode(bytes));
  } catch (error) {
    if (error instanceof ClientError) throw error;
    throw new ClientError(400, "Invalid JSON");
  } finally {
    reader.releaseLock();
  }
}
