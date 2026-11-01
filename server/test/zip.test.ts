import { describe, expect, it } from "vitest";
import { LIMITS, ZipError, crc32, pathProblem, validateArchive } from "../src/lib/zip";
import { packageZip, zip, type Member } from "./zipwriter";

const spec = { name: "pkg/packagespec.lua", data: "Package:describe(function(s)\n  s.name = 'P'\n  s.version = '1.0.0'\nend)\n" };

function problems(buf: Uint8Array, limits = LIMITS): string[] {
  try {
    validateArchive(buf, "pkg", limits);
    return [];
  } catch (e) {
    if (e instanceof ZipError) return e.details;
    throw e;
  }
}

function withMember(m: Member): Uint8Array {
  return zip([spec, m]);
}

describe("zip validation", () => {
  it("accepts a good archive and returns the spec", () => {
    const v = validateArchive(packageZip("pkg"), "pkg");
    expect(v.entries.map((e) => e.name)).toEqual(["pkg/packagespec.lua", "pkg/lib/sh_pkg.lua"]);
    expect(v.spec).toContain("s.version = '1.0.0'");
  });

  it("computes the zip CRC-32", () => {
    expect(crc32(0, new TextEncoder().encode("123456789"))).toBe(0xcbf43926);
  });

  it.each([
    ["pkg/../evil.lua", /`\.\.` component/],
    ["pkg/./a.lua", /`\.` component/],
    ["/pkg/abs.lua", /absolute path/],
    ["C:/pkg/x.lua", /drive letter/],
    ["pkg/C:/x.lua", /drive letter/],
    ["pkg\\win.lua", /backslash/],
    ["pkg//double.lua", /empty component/],
    ["other/file.lua", /outside `pkg\/`/],
    ["pkg", /outside `pkg\/`/],
    [`pkg/${"a".repeat(256)}.lua`, /longer than 255 bytes/],
    ["pkg/ctl\u0001.lua", /control character/],
  ])("refuses the path %j", (name, re) => {
    expect(problems(withMember({ name, data: "x" })).join("\n")).toMatch(re);
  });

  it("accepts a 255-byte component and UTF-8 names", () => {
    expect(pathProblem("pkg", `pkg/${"é".repeat(127)}a`)).toBeNull();
    expect(problems(withMember({ name: "pkg/ünïcödé.lua", data: "x" }))).toEqual([]);
  });

  it("refuses a missing packagespec", () => {
    expect(problems(zip([{ name: "pkg/lib/a.lua", data: "x" }])).join()).toMatch(/no `pkg\/packagespec.lua`/);
    expect(problems(zip([{ name: "pkg/sub/packagespec.lua", data: "x" }])).join()).toMatch(/no `pkg\/packagespec.lua`/);
  });

  it("refuses symlinks, directories and other non-regular entries", () => {
    expect(problems(withMember({ name: "pkg/link", data: "/etc/passwd", attrs: (0o120777 << 16) >>> 0 })).join()).toMatch(/symbolic link/);
    expect(problems(withMember({ name: "pkg/lib/", data: "", attrs: ((0o040755 << 16) | 0x10) >>> 0 })).join()).toMatch(/directory/);
    expect(problems(withMember({ name: "pkg/dir", data: "", madeBy: 0, attrs: 0x10 })).join()).toMatch(/directory/);
    expect(problems(withMember({ name: "pkg/fifo", data: "", attrs: (0o010644 << 16) >>> 0 })).join()).toMatch(/not a regular file/);
  });

  it("reads the Unix mode whatever host made the entry, and refuses unknown hosts", () => {
    const link = (0o120777 << 16) >>> 0;
    expect(problems(withMember({ name: "pkg/l1", data: "x", madeBy: 0, attrs: link })).join()).toMatch(/symbolic link/);
    expect(problems(withMember({ name: "pkg/l2", data: "x", madeBy: 19, attrs: link })).join()).toMatch(/symbolic link/);
    expect(problems(withMember({ name: "pkg/ok.lua", data: "x", madeBy: 19 }))).toEqual([]);
    expect(problems(withMember({ name: "pkg/dos.lua", data: "x", madeBy: 0, attrs: 0x20 }))).toEqual([]);
    expect(problems(withMember({ name: "pkg/nt.lua", data: "x", madeBy: 10, attrs: 0 })).join()).toMatch(/host system 10/);
  });

  it("refuses an archive over 64 MiB before parsing it", () => {
    expect(problems(new Uint8Array(LIMITS.compressed + 1)).join()).toMatch(/larger than 67108864 bytes/);
  });

  it("refuses too many members and too many uncompressed bytes", () => {
    const many = zip([spec, ...Array.from({ length: 10 }, (_, i) => ({ name: `pkg/f${i}.lua`, data: "x" }))]);
    expect(problems(many, { ...LIMITS, members: 10 }).join()).toMatch(/11 members, more than 10/);
    const big = zip([spec, { name: "pkg/big.lua", data: "abcdefghijklmnopqrstuvwxyz0123456789".repeat(100) }]);
    expect(problems(big, { ...LIMITS, uncompressed: 1000 }).join()).toMatch(/inflates to \d+ bytes, more than 1000/);
  });

  it("refuses the real member limit", () => {
    const members = [spec, ...Array.from({ length: LIMITS.members }, (_, i) => ({ name: `pkg/f${i}`, data: "" }))];
    expect(problems(zip(members)).join()).toMatch(/10001 members, more than 10000/);
  });

  it("refuses a compression ratio bomb", () => {
    const zeros = new Uint8Array(1024 * 1024);
    expect(problems(withMember({ name: "pkg/bomb.lua", data: zeros })).join()).toMatch(/compression ratio of \d+:1, the limit is under 100:1/);
  });

  it("stops a member that inflates beyond its declared size while inflating", () => {
    // The headers claim 2000 bytes, which keeps the declared ratio low.
    const zeros = new Uint8Array(4 * 1024 * 1024);
    expect(problems(withMember({ name: "pkg/liar.lua", data: zeros, claimSize: 2000 })).join()).toMatch(/inflates beyond its declared size of 2000 bytes/);
  });

  it("refuses a CRC mismatch, duplicates, case clashes and file/directory clashes", () => {
    const good = withMember({ name: "pkg/a.lua", data: "hello", method: 0 });
    const bad = good.slice();
    const at = new TextDecoder("latin1").decode(bad).indexOf("hello");
    bad[at] = "j".charCodeAt(0);
    expect(problems(bad).join()).toMatch(/CRC-32 mismatch/);
    expect(problems(zip([spec, { name: "pkg/a.lua", data: "1" }, { name: "pkg/a.lua", data: "2" }])).join()).toMatch(/more than once/);
    // Names that differ only in case are distinct members.
    expect(problems(zip([spec, { name: "pkg/A.lua", data: "1" }, { name: "pkg/a.lua", data: "2" }]))).toEqual([]);
    expect(problems(zip([spec, { name: "pkg/x", data: "1" }, { name: "pkg/x/y.lua", data: "2" }])).join()).toMatch(/both a file and a directory/);
  });

  it("refuses encrypted members and unknown methods", () => {
    expect(problems(withMember({ name: "pkg/e.lua", data: "x", flags: 0x801 })).join()).toMatch(/encrypted/);
    expect(problems(withMember({ name: "pkg/m.lua", data: "x", method: 12 })).join()).toMatch(/compression method 12/);
  });

  it("refuses overlapping members, hidden bytes and local/central disagreement", () => {
    const overlap = zip([spec, { name: "pkg/a.lua", data: "a" }, { name: "pkg/b.lua", data: "b" }], { offsetOf: { 2: 1 } });
    expect(problems(overlap).join()).toMatch(/does not start where the previous one ends/);
    const prefixed = zip([spec], { prefix: new Uint8Array([1, 2, 3, 4]) });
    expect(problems(prefixed).join()).toMatch(/does not start where the previous one ends/);
    expect(problems(withMember({ name: "pkg/a.lua", localName: "pkg/b.lua", data: "x" })).join()).toMatch(/different name in its local header/);
  });

  it("refuses something that is not a zip", () => {
    expect(problems(new TextEncoder().encode("PK but not really")).join()).toMatch(/not a zip archive/);
    expect(problems(zip([])).join()).toMatch(/empty/);
  });
});
