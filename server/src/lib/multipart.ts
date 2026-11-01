// multipart/form-data (RFC 7578) over an in-memory body. Parts are views, not copies,
// so a 64 MiB archive is resident once in the 128 MiB Worker; `formData()` would copy it.

export interface Part {
  name: string;
  filename: string | null;
  contentType: string | null;
  data: Uint8Array;
}

export class MultipartError extends Error {}

/** The boundary of a `multipart/form-data` Content-Type, or null. */
export function boundaryOf(contentType: string | null | undefined): string | null {
  if (!contentType || !/^multipart\/form-data\s*;/i.test(contentType)) return null;
  const m = /;\s*boundary=(?:"([^"]+)"|([^;\s]+))/i.exec(contentType);
  const b = m ? (m[1] ?? m[2])! : null;
  return b && b.length <= 70 ? b : null;
}

function indexOf(hay: Uint8Array, needle: Uint8Array, from: number): number {
  const first = needle[0]!;
  const last = hay.length - needle.length;
  outer: for (let i = hay.indexOf(first, from); i !== -1 && i <= last; i = hay.indexOf(first, i + 1)) {
    for (let j = 1; j < needle.length; j++) if (hay[i + j] !== needle[j]) continue outer;
    return i;
  }
  return -1;
}

function param(header: string, key: string): string | null {
  const m = new RegExp(`;\\s*${key}="((?:[^"\\\\]|\\\\.)*)"`, "i").exec(header) ?? new RegExp(`;\\s*${key}=([^;\\s]+)`, "i").exec(header);
  return m ? m[1]!.replace(/\\(.)/g, "$1") : null;
}

/** Splits a multipart body into its parts. */
export function parseMultipart(body: Uint8Array, boundary: string): Part[] {
  const enc = new TextEncoder();
  const dec = new TextDecoder();
  const delimiter = enc.encode(`--${boundary}`);
  const crlfDelimiter = enc.encode(`\r\n--${boundary}`);
  let at = indexOf(body, delimiter, 0);
  if (at !== 0 && (at === -1 || body[at - 1] !== 0x0a)) throw new MultipartError("the body does not start with the boundary");
  const parts: Part[] = [];
  at += delimiter.length;
  for (;;) {
    // After a delimiter: `--` ends the body, CRLF starts a part.
    if (body[at] === 0x2d && body[at + 1] === 0x2d) return parts;
    while (body[at] === 0x20 || body[at] === 0x09) at++;
    if (body[at] !== 0x0d || body[at + 1] !== 0x0a) throw new MultipartError("malformed boundary line");
    at += 2;
    const headerEnd = indexOf(body, enc.encode("\r\n\r\n"), at);
    if (headerEnd === -1 || headerEnd - at > 16 * 1024) throw new MultipartError("malformed part headers");
    const headers = new Map<string, string>();
    for (const line of dec.decode(body.subarray(at, headerEnd)).split("\r\n")) {
      const colon = line.indexOf(":");
      if (colon > 0) headers.set(line.slice(0, colon).trim().toLowerCase(), line.slice(colon + 1).trim());
    }
    const start = headerEnd + 4;
    const end = indexOf(body, crlfDelimiter, start);
    if (end === -1) throw new MultipartError("a part is not terminated by the boundary");
    const disposition = headers.get("content-disposition") ?? "";
    if (!/^form-data\b/i.test(disposition)) throw new MultipartError("a part is not form-data");
    const name = param(disposition, "name");
    if (name === null) throw new MultipartError("a part has no name");
    parts.push({ name, filename: param(disposition, "filename"), contentType: headers.get("content-type") ?? null, data: body.subarray(start, end) });
    at = end + crlfDelimiter.length;
  }
}
