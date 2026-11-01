// JSON responses and the error envelope `{ "error": { "code", "message", "details"? } }`.

export const API_HEADER = "X-Fpkg-Api";
export const API_VERSION = "1";

/** An error that becomes the API's error envelope. */
export class ApiError extends Error {
  constructor(
    public readonly status: number,
    public readonly code: string,
    message: string,
    public readonly details?: string[],
    public readonly headers?: Record<string, string>,
  ) {
    super(message);
  }
}

export const notFound = (message: string) => new ApiError(404, "not_found", message);
export const unauthorized = (message = "this endpoint needs an API token: `Authorization: Bearer fpk_...`") =>
  new ApiError(401, "unauthorized", message, undefined, { "WWW-Authenticate": 'Bearer realm="fluxpkg.dev"' });
export const forbidden = (message: string) => new ApiError(403, "forbidden", message);
export const badRequest = (message: string) => new ApiError(400, "bad_request", message);
export const invalid = (message: string, details: string[]) => new ApiError(422, "validation_failed", message, details);

export function errorBody(code: string, message: string, details?: string[]): unknown {
  return { error: details && details.length > 0 ? { code, message, details } : { code, message } };
}

export function jsonResponse(body: unknown, status = 200, headers: Record<string, string> = {}): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "Content-Type": "application/json; charset=utf-8", "Cache-Control": "no-store", ...headers },
  });
}

export function errorResponse(e: ApiError): Response {
  return jsonResponse(errorBody(e.code, e.message, e.details), e.status, e.headers ?? {});
}

/** A copy of `res` with mutable headers and `X-Fpkg-Api: 1`. */
export function withApiHeader(res: Response): Response {
  const out = new Response(res.body, res);
  out.headers.set(API_HEADER, API_VERSION);
  return out;
}

/** `2026-11-01T18:04:11Z`: UTC, seconds. */
export function now(date = new Date()): string {
  return date.toISOString().replace(/\.\d{3}Z$/, "Z");
}

export function today(date = new Date()): string {
  return date.toISOString().slice(0, 10);
}
