// A zip writer for test archives: every field the validator checks can be set or falsified.

import { deflateSync } from "fflate";
import { crc32 } from "../src/lib/zip";

export interface Member {
  name: string;
  data?: Uint8Array | string;
  /** 0 store, 8 deflate (default: deflate from 64 bytes up, like fpkg pack). */
  method?: number;
  /** External attributes (default: a 0644 regular file). */
  attrs?: number;
  /** "Version made by" host (default 3, Unix). */
  madeBy?: number;
  flags?: number;
  /** Uncompressed size to claim instead of the real one. */
  claimSize?: number;
  /** Name in the local header, if different. */
  localName?: string;
}

export interface Options {
  /** Bytes before the first member. */
  prefix?: Uint8Array;
  /** Central directory entries pointing at the local header of another member (by index). */
  offsetOf?: Record<number, number>;
  comment?: string;
}

const enc = new TextEncoder();

function le(n: number, bytes: number): number[] {
  const out: number[] = [];
  for (let i = 0; i < bytes; i++) out.push(Math.floor(n / 2 ** (8 * i)) & 0xff);
  return out;
}

export function zip(members: Member[], options: Options = {}): Uint8Array {
  const parts: number[][] = [];
  const central: number[][] = [];
  let offset = options.prefix?.length ?? 0;
  if (options.prefix) parts.push([...options.prefix]);
  const offsets: number[] = [];
  for (const m of members) {
    const raw = typeof m.data === "string" ? enc.encode(m.data) : (m.data ?? new Uint8Array(0));
    const method = m.method ?? (raw.length >= 64 ? 8 : 0);
    const body = method === 8 ? deflateSync(raw, { level: 6 }) : raw;
    const crc = crc32(0, raw);
    const name = enc.encode(m.name);
    const localName = enc.encode(m.localName ?? m.name);
    const size = m.claimSize ?? raw.length;
    const flags = m.flags ?? 0x800;
    const local = [...le(0x04034b50, 4), ...le(20, 2), ...le(flags, 2), ...le(method, 2), ...le(0, 2), ...le(0x21, 2),
      ...le(crc, 4), ...le(body.length, 4), ...le(size, 4), ...le(localName.length, 2), ...le(0, 2), ...localName, ...body];
    offsets.push(offset);
    parts.push(local);
    offset += local.length;
    const attrs = m.attrs ?? (0o100644 << 16) >>> 0;
    central.push([...le(0x02014b50, 4), 20, m.madeBy ?? 3, ...le(20, 2), ...le(flags, 2), ...le(method, 2), ...le(0, 2), ...le(0x21, 2),
      ...le(crc, 4), ...le(body.length, 4), ...le(size, 4), ...le(name.length, 2), ...le(0, 2), ...le(0, 2), ...le(0, 2), ...le(0, 2),
      ...le(attrs, 4), ...le(0, 4) /* offset, patched below */, ...name]);
  }
  central.forEach((c, i) => {
    const o = offsets[options.offsetOf?.[i] ?? i]!;
    c.splice(42, 4, ...le(o, 4));
  });
  const cdStart = offset;
  const cd = central.flat();
  const comment = enc.encode(options.comment ?? "");
  const eocd = [...le(0x06054b50, 4), ...le(0, 2), ...le(0, 2), ...le(members.length, 2), ...le(members.length, 2),
    ...le(cd.length, 4), ...le(cdStart, 4), ...le(comment.length, 2), ...comment];
  return Uint8Array.from([...parts.flat(), ...cd, ...eocd]);
}

/** A small valid package archive. */
export function packageZip(id: string, version = "1.0.0", name = "Test Package", extra: Member[] = []): Uint8Array {
  return zip([
    { name: `${id}/packagespec.lua`, data: `Package:describe(function(s)\n  s.name = '${name}'\n  s.version = '${version}'\n  s.summary = 'A test package.'\nend)\n` },
    { name: `${id}/lib/sh_${id}.lua`, data: `--- Says hello.\n-- @param who [String] whom to greet\nfunction ${id}_hello(who)\n  print('hello ' .. who)\nend\n`.repeat(2) },
    ...extra,
  ]);
}
