import { describe, expect, it } from "vitest";
import { canonicalize, compareVersions, isCanonical, matches, parseConstraint, parseVersion, sortKey } from "../src/lib/version";

describe("canonicalisation", () => {
  it.each([
    ["1.0", "1.0.0"],
    ["1.0.0", "1.0.0"],
    ["0.9.0", "0.9.0"],
    ["7.0.9", "7.0.9"],
    ["20211019", "20211019.0.0"],
    ["1", "1.0.0"],
    ["1.2.3-beta", "1.2.3-beta"],
    ["1.2-rc.1", "1.2.0-rc.1"],
    ["01.002.3", "1.2.3"],
    ["2.1.1", "2.1.1"],
  ])("%s -> %s", (input, canonical) => {
    expect(canonicalize(input)).toBe(canonical);
  });

  it.each(["", "v1.0", "1.0.0.0", "1..0", "1.0.", "-beta", "1.0.0-", "1.0.0+build", "1.0.0-be ta", " 1.0", "1.0.0-01", "1".repeat(21)])(
    "rejects %j",
    (input) => {
      expect(parseVersion(input)).toBeNull();
    },
  );

  it("knows the canonical form", () => {
    expect(isCanonical("1.0.0")).toBe(true);
    expect(isCanonical("1.0")).toBe(false);
    expect(isCanonical("01.0.0")).toBe(false);
  });
});

describe("ordering", () => {
  const ordered = [
    "0.9.0",
    "1.0.0-alpha",
    "1.0.0-alpha.1",
    "1.0.0-alpha.beta",
    "1.0.0-beta",
    "1.0.0-beta.2",
    "1.0.0-beta.11",
    "1.0.0-rc.1",
    "1.0.0",
    "1.2.0",
    "1.9.0",
    "1.10.0",
    "2.0.0",
    "10.0.0",
    "20211019.0.0",
  ];

  it("is numeric per component, pre-releases first (Flux's concatenated sum is not)", () => {
    for (let i = 0; i < ordered.length; i++)
      for (let j = 0; j < ordered.length; j++) {
        const c = compareVersions(parseVersion(ordered[i]!)!, parseVersion(ordered[j]!)!);
        expect(Math.sign(c), `${ordered[i]} vs ${ordered[j]}`).toBe(Math.sign(i - j));
      }
  });

  it("sort keys sort bytewise like compareVersions", () => {
    const extra = ["1.0.0-a", "1.0.0-a-b", "1.0.0-a.x", "1.0.0-ab", "1.0.0-1", "1.0.0-2.a", "1.0.0-10", "1.0.0-a.1.2", "0.0.0", "99999999999999999999.0.0"];
    const all = [...ordered, ...extra].map((v) => parseVersion(v)!);
    const byCompare = [...all].sort(compareVersions).map((v) => sortKey(v));
    const byKey = all.map((v) => sortKey(v)).sort((a, b) => (a < b ? -1 : a > b ? 1 : 0));
    expect(byKey).toEqual(byCompare);
  });
});

describe("constraints", () => {
  it.each([
    ["1.2.3", "1.2.3", true],
    ["1.2.3", "1.2.4", false],
    ["== 1.2.3", "1.2.3", true],
    ["== 1.2", "1.2.0", true],
    [">= 1.2", "1.2.0", true],
    [">= 1.2", "1.1.9", false],
    [">= 1.2", "3.0.0", true],
    ["~> 1.2", "1.2.0", true],
    ["~> 1.2", "1.9.9", true],
    ["~> 1.2", "2.0.0", false],
    ["~> 1.2", "1.1.0", false],
    ["~> 1.2.3", "1.2.3", true],
    ["~> 1.2.3", "1.2.9", true],
    ["~> 1.2.3", "1.3.0", false],
    ["~> 1.2.3", "1.2.2", false],
    ["~> 1", "1.9.0", true],
    ["~> 1", "2.0.0", false],
    ["~> 7.0", "7.0.9", true],
    ["< 2.0", "1.9.9", true],
    ["< 2.0", "2.0.0", false],
    ["<= 2.0", "2.0.0", true],
    ["> 1.0", "1.0.0", false],
    ["> 1.0", "1.0.1", true],
    ["*", "0.0.1", true],
    ["", "5.0.0", true],
    [">= 1.2, < 2.0", "1.5.0", true],
    [">= 1.2, < 2.0", "2.0.0", false],
    [">=1.2,<1.3", "1.2.5", true],
    // Only a constraint naming a pre-release of the same X.Y.Z admits one.
    [">= 2.0.0-beta", "2.0.0-rc1", true],
    [">= 2.0.0-beta", "2.0.0-beta", true],
    [">= 2.0.0-beta", "2.0.0", true],
    [">= 2.0.0-beta", "2.1.0-alpha", false],
    ["~> 1.2", "2.0.0-beta", false],
    ["< 2.0", "2.0.0-beta", false],
    ["*", "2.0.0-beta", false],
    ["", "2.0.0-beta", false],
    ["== 1.0.0-beta", "1.0.0-beta", true],
    ["1.0.0-beta", "1.0.0-beta", true],
    ["== 1.0.0-beta", "1.0.0-rc", false],
    [">= 1.0.0", "1.0.0-rc.1", false],
    [">= 1.0.0-alpha, < 1.0.0", "1.0.0-rc.1", true],
  ])("%j matches %s: %s", (constraint, version, expected) => {
    expect(matches(version, constraint)).toBe(expected);
  });

  it.each(["=> 1.0", "~ 1.0", "1.0 2.0", ">= x", ",", ">= 1.0,", "= 1.0", "*, 1.0"])("rejects %j", (c) => {
    expect(parseConstraint(c)).toBeNull();
  });
});
