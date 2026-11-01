import { describe, expect, it } from "vitest";
import { MultipartError, boundaryOf, parseMultipart } from "../src/lib/multipart";

async function encode(form: FormData): Promise<{ body: Uint8Array; type: string }> {
  const req = new Request("https://x/", { method: "POST", body: form });
  return { body: new Uint8Array(await req.arrayBuffer()), type: req.headers.get("Content-Type")! };
}

describe("multipart/form-data", () => {
  it("parses what the runtime's FormData serialises, as views", async () => {
    const bytes = Uint8Array.from({ length: 70_000 }, (_, i) => (i * 31) & 0xff);
    const form = new FormData();
    form.append("metadata", '{"id":"x","name":"Quotes \\" and \\r\\n"}');
    form.append("archive", new File([bytes], "x-1.0.0.zip", { type: "application/zip" }));
    const { body, type } = await encode(form);
    const parts = parseMultipart(body, boundaryOf(type)!);
    expect(parts.map((p) => [p.name, p.filename, p.contentType])).toEqual([
      ["metadata", null, null],
      ["archive", "x-1.0.0.zip", "application/zip"],
    ]);
    expect(new TextDecoder().decode(parts[0]!.data)).toBe('{"id":"x","name":"Quotes \\" and \\r\\n"}');
    expect(parts[1]!.data).toEqual(bytes);
    expect(parts[1]!.data.buffer).toBe(body.buffer);
  });

  it("reads quoted boundaries and refuses malformed bodies", () => {
    expect(boundaryOf('multipart/form-data; boundary="a b"')).toBe("a b");
    expect(boundaryOf("multipart/form-data; boundary=xyz; charset=utf-8")).toBe("xyz");
    expect(boundaryOf("application/json")).toBeNull();
    expect(boundaryOf("multipart/form-data")).toBeNull();
    const enc = (s: string) => new TextEncoder().encode(s);
    const ok = '--b\r\nContent-Disposition: form-data; name="a"\r\n\r\nhello\r\n--b--\r\n';
    expect(new TextDecoder().decode(parseMultipart(enc(ok), "b")[0]!.data)).toBe("hello");
    expect(() => parseMultipart(enc('--b\r\nContent-Disposition: form-data; name="a"\r\n\r\nunterminated'), "b")).toThrow(MultipartError);
    expect(() => parseMultipart(enc("garbage"), "b")).toThrow(MultipartError);
    expect(() => parseMultipart(enc('--b\r\nContent-Disposition: attachment; name="a"\r\n\r\nx\r\n--b--'), "b")).toThrow(/not form-data/);
  });
});
