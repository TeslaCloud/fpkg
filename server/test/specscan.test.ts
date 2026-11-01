import { describe, expect, it } from "vitest";
import { checkSpec } from "../src/api/publish";
import { scanSpec } from "../src/lib/specscan";

// The 17 packagespec.lua files of flux-ce at ab82c2e4 (the root one as flux.lua).
const fixtures = import.meta.glob("./fixtures/specs/*.lua", { query: "?raw", import: "default", eager: true }) as Record<string, string>;

const expected: Record<string, [string, string]> = {
  flux: ["Flux", "0.9.0"],
  active_network: ["Active Network", "1.0"],
  active_record: ["Active Record", "1.0"],
  active_ui: ["Active UI", "1.0"],
  bolt: ["Bolt", "1.0"],
  cable: ["Cable", "1.0.0"],
  cami: ["CAMI", "20211019"],
  client_settings: ["Client Settings", "1.0"],
  conditions: ["Conditions", "1.0"],
  flow: ["Flow", "0.9.0"],
  fontawesome: ["Font Awesome", "7.3.1"],
  llm: ["LLM", "1.0.0"],
  lumen: ["Lumen", "1.0"],
  sfs: ["Srlion's Fast Serializer", "7.0.9"],
  tween: ["Tween", "2.1.1"],
  utf8: ["UTF-8", "1.0.0"],
  yaml: ["YAML", "1.2.2"],
};

describe("the literal scanner on flux-ce's specs", () => {
  it("has all 17 fixtures", () => {
    expect(Object.keys(fixtures)).toHaveLength(17);
  });

  for (const [path, src] of Object.entries(fixtures)) {
    const id = path.replace(/^.*\/(.*)\.lua$/, "$1");
    it(`reads ${id}`, () => {
      const scan = scanSpec(src);
      const [name, version] = expected[id]!;
      expect(scan.error).toBeNull();
      expect(scan.version).toEqual({ literals: [version], computed: [] });
      expect(scan.name).toEqual({ literals: [name], computed: [] });
    });
  }

  it("accepts each spec for its canonical version and refuses another", () => {
    for (const [path, src] of Object.entries(fixtures)) {
      const id = path.replace(/^.*\/(.*)\.lua$/, "$1");
      const [name, version] = expected[id]!;
      const canonical = version.split("-")[0]!.split(".").concat(["0", "0"]).slice(0, 3).join(".");
      expect(checkSpec(src, id, canonical, name), id).toEqual([]);
      expect(checkSpec(src, id, "99.0.0", name).join(" "), id).toMatch(/is not the published version/);
      expect(checkSpec(src, id, canonical, "Something Else").join(" "), id).toMatch(/disagrees/);
    }
  });
});

describe("the literal scanner on awkward specs", () => {
  const scan = (src: string) => scanSpec(src);

  it("reads all three string forms", () => {
    expect(scan(`s.version = "1.2.3"`).version.literals).toEqual(["1.2.3"]);
    expect(scan(`s.version = '1.2.3'`).version.literals).toEqual(["1.2.3"]);
    expect(scan(`s.version = [[1.2.3]]`).version.literals).toEqual(["1.2.3"]);
    expect(scan(`s.version = [==[\n1.2.3]==]`).version.literals).toEqual(["1.2.3"]);
    expect(scan(`s.name = 'It\\'s \\"x\\"\\65'`).name.literals).toEqual([`It's "x"A`]);
  });

  it("decodes byte escapes as UTF-8, like the game", () => {
    expect(scan(`s.name = 'Caf\\xc3\\xa9'`).name.literals).toEqual(["Café"]);
    expect(scan(`s.name = "Caf\\195\\169"`).name.literals).toEqual(["Café"]);
    expect(scan(`s.name = 'Café \\u{1F600} 😀'`).name.literals).toEqual(["Café \u{1F600} 😀"]);
    expect(checkSpec(`s.name = 'Caf\\xc3\\xa9'\ns.version = '1.0.0'`, "x", "1.0.0", "Café")).toEqual([]);
    expect(scan(`s.name = '\\256'`).error).toMatch(/malformed string/);
  });

  it("ignores comments and strings", () => {
    const src = `-- s.version = '0.1'
--[[ s.version = '0.2' ]]
--[==[ s.version = '0.3' ]==]
/* s.version = '0.4' */
// s.version = '0.5'
local x = "s.version = '0.6'"
local y = [[ s.name = 'nope' ]]
s.version = '1.0.0'`;
    expect(scan(src).version).toEqual({ literals: ["1.0.0"], computed: [] });
    expect(scan(src).name.literals).toEqual([]);
  });

  it("works with any variable name and spacing", () => {
    expect(scan(`Package:describe(function(spec)\n  spec . version\t=  '2.0'\nend)`).version.literals).toEqual(["2.0"]);
  });

  it("reports computed values instead of guessing", () => {
    expect(scan(`s.version = '1.' .. minor`).version.computed).toEqual([1]);
    expect(scan(`s.version = VERSION`).version.computed).toEqual([1]);
    expect(scan(`\n\ns.version = get_version()`).version.computed).toEqual([3]);
    expect(scan(`s.version = 'a' or 'b'`).version.computed).toEqual([1]);
  });

  it("does not take comparisons or other tables' fields", () => {
    expect(scan(`if s.version == '1.0' then end`).version).toEqual({ literals: [], computed: [] });
    expect(scan(`s.meta.version = '9.9.9'`).version).toEqual({ literals: [], computed: [] });
    expect(scan(`self:x().version = '9.9.9'`).version).toEqual({ literals: [], computed: [] });
  });

  it("sees both branches of a conditional", () => {
    const src = `if IS_DEVELOPMENT then s.version = '1.0.0-dev' else s.version = '1.0.0' end`;
    expect(scan(src).version.literals).toEqual(["1.0.0-dev", "1.0.0"]);
    expect(checkSpec(src, "x", "1.0.0", null).join(" ")).toMatch(/different values/);
  });

  it("refuses a spec without a version and an unterminated string", () => {
    expect(checkSpec(`s.name = 'X'`, "x", "1.0.0", null).join(" ")).toMatch(/no `s.version/);
    expect(scan(`s.version = '1.0`).error).toMatch(/unterminated/);
  });
});
