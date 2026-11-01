import { describe, expect, it } from "vitest";
import { CACHE_CONTROL, ETAG, cacheKey, conditional, ifNoneMatch, notModified } from "../src/lib/cache";

describe("cache headers", () => {
  it("uses the expected Cache-Control values", () => {
    expect(CACHE_CONTROL.archive).toBe("public, max-age=31536000, immutable");
    expect(CACHE_CONTROL.version).toBe("public, max-age=31536000, immutable");
    expect(CACHE_CONTROL.package).toBe("public, max-age=60, stale-while-revalidate=600");
    expect(CACHE_CONTROL.search).toBe("public, max-age=60");
    expect(CACHE_CONTROL.docs).toBe("public, max-age=86400, stale-while-revalidate=604800");
    expect(CACHE_CONTROL.docsRedirect).toBe("public, max-age=60");
    expect(CACHE_CONTROL.page).toBe("public, max-age=60");
    expect(CACHE_CONTROL.pagePrivate).toBe("private, max-age=60");
  });

  it("builds the expected ETags", () => {
    expect(ETAG.archive("ab12")).toBe('"ab12"');
    expect(ETAG.version("ab12", false)).toBe('"ab12-0"');
    expect(ETAG.version("ab12", true)).toBe('"ab12-1"');
    expect(ETAG.package("cable", 7)).toBe('"cable-7"');
    expect(ETAG.docs("0.1.0+abc", "ab12", "ffee")).toBe('"0.1.0+abc-ab12-ffee"');
  });

  const req = (inm?: string) => new Request("https://fluxpkg.dev/x", inm ? { headers: { "If-None-Match": inm } } : {});

  it("matches If-None-Match lists, weak tags and *", () => {
    expect(ifNoneMatch(req('"a"'), '"a"')).toBe(true);
    expect(ifNoneMatch(req('"b", "a"'), '"a"')).toBe(true);
    expect(ifNoneMatch(req('W/"a"'), '"a"')).toBe(true);
    expect(ifNoneMatch(req("*"), '"a"')).toBe(true);
    expect(ifNoneMatch(req('"b"'), '"a"')).toBe(false);
    expect(ifNoneMatch(req(), '"a"')).toBe(false);
    expect(ifNoneMatch(req('"a"'), null)).toBe(false);
  });

  it("turns a matching 200 into a 304 with the validators", () => {
    const full = new Response("body", { headers: { ETag: '"a"', "Cache-Control": CACHE_CONTROL.package, "X-Fpkg-Sha256": "a" } });
    const res = conditional(req('"a"'), full);
    expect(res.status).toBe(304);
    expect(res.headers.get("ETag")).toBe('"a"');
    expect(res.headers.get("Cache-Control")).toBe(CACHE_CONTROL.package);
    expect(res.headers.get("X-Fpkg-Sha256")).toBe("a");
    expect(conditional(req('"b"'), new Response("x", { headers: { ETag: '"a"' } })).status).toBe(200);
    expect(notModified('"z"', CACHE_CONTROL.archive).status).toBe(304);
  });

  it("keys the cache on the URL alone (plus a variant)", () => {
    const k = cacheKey(new Request("https://docs.fluxpkg.dev/a/1.0.0/", { headers: { Authorization: "x", "If-None-Match": '"q"' } }), "0.1.0+abc");
    expect(k.url).toBe("https://docs.fluxpkg.dev/a/1.0.0/?__v=0.1.0%2Babc");
    expect(k.headers.get("If-None-Match")).toBeNull();
    expect(k.method).toBe("GET");
  });
});
