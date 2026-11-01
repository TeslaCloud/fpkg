// Zip archives: reading, validating (untrusted: headers cross-checked, members tiled without
// overlap, ratio and size limits enforced while inflating) and extracting members.

import { Inflate, inflateSync } from "fflate";

export interface Limits {
  members: number;
  compressed: number;
  uncompressed: number;
  ratio: number;
  component: number;
}

export const LIMITS: Readonly<Limits> = Object.freeze({
  members: 10_000,
  compressed: 64 * 1024 * 1024,
  uncompressed: 256 * 1024 * 1024,
  /** Uncompressed : compressed must stay under this, per member. */
  ratio: 100,
  component: 255,
});

/** How much compressed input is pushed into the inflater at once; bounds
 * what one push can produce to about 1 MiB (deflate tops out near 1032:1). */
const INFLATE_STEP = 1024;

export class ZipError extends Error {
  constructor(public readonly details: string[]) {
    super(details.join("; "));
  }
}

export interface ZipEntry {
  name: string;
  /** 0 store, 8 deflate. */
  method: number;
  flags: number;
  crc32: number;
  compressedSize: number;
  size: number;
  localOffset: number;
  /** "Version made by" high byte: 3 is Unix, 0 MS-DOS. */
  madeBy: number;
  externalAttrs: number;
}

const EOCD_SIG = 0x06054b50;
const CD_SIG = 0x02014b50;
const LOCAL_SIG = 0x04034b50;
const ZIP64_LOCATOR_SIG = 0x07064b50;
const EOCD_MIN = 22;
/** The EOCD record is within this many bytes of the end (comment max 64 KiB). */
export const EOCD_SEARCH = EOCD_MIN + 0xffff;

function u16(b: Uint8Array, o: number): number {
  return b[o]! | (b[o + 1]! << 8);
}

function u32(b: Uint8Array, o: number): number {
  return (b[o]! | (b[o + 1]! << 8) | (b[o + 2]! << 16) | (b[o + 3]! << 24)) >>> 0;
}

let CRC_TABLE: Uint32Array | null = null;

function crcTable(): Uint32Array {
  if (CRC_TABLE) return CRC_TABLE;
  const t = new Uint32Array(256);
  for (let n = 0; n < 256; n++) {
    let c = n;
    for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
    t[n] = c >>> 0;
  }
  CRC_TABLE = t;
  return t;
}

/** Incremental CRC-32 (the zip polynomial). Start with 0. */
export function crc32(crc: number, data: Uint8Array): number {
  const t = crcTable();
  let c = ~crc >>> 0;
  for (let i = 0; i < data.length; i++) c = t[(c ^ data[i]!) & 0xff]! ^ (c >>> 8);
  return ~c >>> 0;
}

export interface EndOfCentralDirectory {
  entries: number;
  cdSize: number;
  cdOffset: number;
  /** Offset of the EOCD record in the archive. */
  offset: number;
}

/**
 * Finds the end of central directory record in `tail`, the last bytes of
 * an archive of `totalSize` bytes.
 */
export function findEndOfCentralDirectory(tail: Uint8Array, totalSize: number): EndOfCentralDirectory {
  const tailStart = totalSize - tail.length;
  for (let i = tail.length - EOCD_MIN; i >= 0; i--) {
    if (u32(tail, i) !== EOCD_SIG) continue;
    const commentLen = u16(tail, i + 20);
    if (i + EOCD_MIN + commentLen !== tail.length) continue;
    const disk = u16(tail, i + 4);
    const cdDisk = u16(tail, i + 6);
    const entriesHere = u16(tail, i + 8);
    const entries = u16(tail, i + 10);
    const cdSize = u32(tail, i + 12);
    const cdOffset = u32(tail, i + 16);
    if (disk !== 0 || cdDisk !== 0 || entriesHere !== entries) throw new ZipError(["multi-disk archives are not supported"]);
    if (entries === 0xffff || cdSize === 0xffffffff || cdOffset === 0xffffffff) throw new ZipError(["zip64 archives are not supported"]);
    if (i >= 20 && u32(tail, i - 20) === ZIP64_LOCATOR_SIG) throw new ZipError(["zip64 archives are not supported"]);
    const offset = tailStart + i;
    if (cdOffset + cdSize !== offset) throw new ZipError(["the central directory does not end where the end record starts"]);
    return { entries, cdSize, cdOffset, offset };
  }
  throw new ZipError(["not a zip archive (no end of central directory record)"]);
}

/** Parses `count` central directory entries from `cd`. */
export function parseCentralDirectory(cd: Uint8Array, count: number): ZipEntry[] {
  const entries: ZipEntry[] = [];
  const decoder = new TextDecoder("utf-8", { fatal: true, ignoreBOM: true });
  let o = 0;
  for (let n = 0; n < count; n++) {
    if (o + 46 > cd.length || u32(cd, o) !== CD_SIG) throw new ZipError([`central directory entry ${n} is malformed`]);
    const nameLen = u16(cd, o + 28);
    const extraLen = u16(cd, o + 30);
    const commentLen = u16(cd, o + 32);
    if (o + 46 + nameLen + extraLen + commentLen > cd.length) throw new ZipError([`central directory entry ${n} is truncated`]);
    const rawName = cd.subarray(o + 46, o + 46 + nameLen);
    let name: string;
    try {
      name = decoder.decode(rawName);
    } catch {
      throw new ZipError([`member ${n}: the path is not UTF-8`]);
    }
    entries.push({
      name,
      madeBy: cd[o + 5]!,
      flags: u16(cd, o + 8),
      method: u16(cd, o + 10),
      crc32: u32(cd, o + 16),
      compressedSize: u32(cd, o + 20),
      size: u32(cd, o + 24),
      externalAttrs: u32(cd, o + 38),
      localOffset: u32(cd, o + 42),
    });
    o += 46 + nameLen + extraLen + commentLen;
  }
  if (o !== cd.length) throw new ZipError(["the central directory has trailing bytes"]);
  return entries;
}

/** Reads the central directory of a whole archive in memory. */
export function readEntries(buf: Uint8Array): ZipEntry[] {
  const tail = buf.subarray(Math.max(0, buf.length - EOCD_SEARCH));
  const eocd = findEndOfCentralDirectory(tail, buf.length);
  return parseCentralDirectory(buf.subarray(eocd.cdOffset, eocd.cdOffset + eocd.cdSize), eocd.entries);
}

/** The local header's length (30 + name + extra) at `offset`, checked against the entry. */
export function localHeaderLength(header: Uint8Array, entry: ZipEntry): number {
  if (header.length < 30 || u32(header, 0) !== LOCAL_SIG) throw new ZipError([`${entry.name}: bad local header`]);
  const nameLen = u16(header, 26);
  const extraLen = u16(header, 28);
  return 30 + nameLen + extraLen;
}

const S_IFMT = 0o170000;
const S_IFREG = 0o100000;

/** "Version made by" hosts whose attributes are understood: MS-DOS, Unix, OS X. */
const KNOWN_HOSTS = new Set([0, 3, 19]);

/** Why an entry is not a regular file, or null. */
export function nonRegular(entry: ZipEntry): string | null {
  if (!KNOWN_HOSTS.has(entry.madeBy)) return `was made on host system ${entry.madeBy}, whose file attributes are not understood (only MS-DOS, Unix and OS X)`;
  if (entry.name.endsWith("/")) return "is a directory entry";
  if (entry.externalAttrs & 0x10) return "is a directory (MS-DOS attribute)";
  // A Unix mode in the high 16 bits counts whatever the made-by host says.
  const mode = entry.externalAttrs >>> 16;
  const type = mode & S_IFMT;
  if (type === 0o120000) return "is a symbolic link";
  if (type !== 0 && type !== S_IFREG) return "is not a regular file";
  return null;
}

/**
 * The problem with a member path of package `id`, or null: forward slashes, no `.`/`..`, not
 * absolute, no drive letters, components of at most 255 bytes, all under `<id>/` (UTF-8 is checked when decoding).
 */
export function pathProblem(id: string, name: string, maxComponent: number = LIMITS.component): string | null {
  if (name.length === 0) return "an empty path";
  if (name.includes("\\")) return "a backslash";
  if (/[\u0000-\u001f\u007f]/.test(name)) return "a control character";
  if (name.startsWith("/")) return "an absolute path";
  if (/^[A-Za-z]:/.test(name)) return "a drive letter";
  const comps = name.split("/");
  const encoder = new TextEncoder();
  for (const c of comps) {
    if (c === "") return "an empty component";
    if (c === "." || c === "..") return `a \`${c}\` component`;
    if (encoder.encode(c).length > maxComponent) return `a component longer than ${maxComponent} bytes`;
    if (/^[A-Za-z]:$/.test(c)) return "a drive letter";
  }
  if (comps[0] !== id || comps.length < 2) return `a path outside \`${id}/\``;
  return null;
}

export interface ValidatedArchive {
  entries: ZipEntry[];
  /** The text of `<id>/packagespec.lua`. */
  spec: string;
  /** Sum of the uncompressed sizes. */
  size: number;
}

/**
 * Inflates (or copies) a member's data, calling `onData` per chunk, and
 * enforces its declared size, the ratio limit and the CRC as it goes.
 */
export function streamMember(data: Uint8Array, entry: ZipEntry, onData: (chunk: Uint8Array) => void): void {
  const limit = entry.size;
  let produced = 0;
  let crc = 0;
  const take = (chunk: Uint8Array) => {
    produced += chunk.length;
    if (produced > limit) throw new ZipError([`${entry.name}: inflates beyond its declared size of ${limit} bytes`]);
    crc = crc32(crc, chunk);
    onData(chunk);
  };
  if (entry.method === 0) {
    for (let o = 0; o < data.length; o += 1 << 20) take(data.subarray(o, Math.min(data.length, o + (1 << 20))));
  } else if (entry.method === 8) {
    let ended = false;
    const inflater = new Inflate((chunk: Uint8Array, final: boolean) => {
      take(chunk);
      if (final) ended = true;
    });
    try {
      for (let o = 0; o < data.length; o += INFLATE_STEP) {
        const end = Math.min(data.length, o + INFLATE_STEP);
        inflater.push(data.subarray(o, end), end === data.length);
      }
      if (data.length === 0) inflater.push(new Uint8Array(0), true);
    } catch (e) {
      if (e instanceof ZipError) throw e;
      throw new ZipError([`${entry.name}: invalid deflate data (${e instanceof Error ? e.message : String(e)})`]);
    }
    if (!ended) throw new ZipError([`${entry.name}: truncated deflate data`]);
  } else {
    throw new ZipError([`${entry.name}: unsupported compression method ${entry.method}`]);
  }
  if (produced !== limit) throw new ZipError([`${entry.name}: inflates to ${produced} bytes, not the declared ${limit}`]);
  if (crc !== entry.crc32) throw new ZipError([`${entry.name}: CRC-32 mismatch`]);
}

/** Validates a package archive for `id`, returning its entries and packagespec text; a ZipError lists every problem. */
export function validateArchive(buf: Uint8Array, id: string, limits: Limits = LIMITS): ValidatedArchive {
  if (buf.length > limits.compressed) throw new ZipError([`the archive is larger than ${limits.compressed} bytes`]);
  const entries = readEntries(buf);
  const problems: string[] = [];
  if (entries.length === 0) throw new ZipError(["the archive is empty"]);
  if (entries.length > limits.members) throw new ZipError([`the archive has ${entries.length} members, more than ${limits.members}`]);

  const names = new Set<string>();
  let total = 0;
  for (const e of entries) {
    const label = `member \`${e.name}\``;
    const path = pathProblem(id, e.name, limits.component);
    if (path) problems.push(`${label} has ${path}`);
    const kind = nonRegular(e);
    if (kind) problems.push(`${label} ${kind}`);
    if (names.has(e.name)) problems.push(`${label} appears more than once`);
    names.add(e.name);
    if (e.flags & 0x1) problems.push(`${label} is encrypted`);
    if (e.method !== 0 && e.method !== 8) problems.push(`${label} uses compression method ${e.method} (only store and deflate are allowed)`);
    if (e.method === 0 && e.size !== e.compressedSize) problems.push(`${label} is stored with differing sizes`);
    if (e.size === 0xffffffff || e.compressedSize === 0xffffffff || e.localOffset === 0xffffffff) problems.push(`${label} needs zip64`);
    if (e.size > 0 && e.size >= limits.ratio * Math.max(1, e.compressedSize))
      problems.push(`${label} has a compression ratio of ${Math.floor(e.size / Math.max(1, e.compressedSize))}:1, the limit is under ${limits.ratio}:1`);
    total += e.size;
  }
  // A file and a directory of the same path cannot both be extracted.
  for (const name of names) {
    const comps = name.split("/");
    for (let i = 1; i < comps.length; i++) {
      const dir = comps.slice(0, i).join("/");
      if (names.has(dir)) problems.push(`member \`${dir}\` is both a file and a directory`);
    }
  }
  if (total > limits.uncompressed) problems.push(`the archive inflates to ${total} bytes, more than ${limits.uncompressed}`);
  const specName = `${id}/packagespec.lua`;
  if (!names.has(specName)) problems.push(`the archive has no \`${specName}\``);
  if (problems.length > 0) throw new ZipError([...new Set(problems)]);

  // Members must tile [0, central directory) in order: no shared or hidden bytes.
  const tail = buf.subarray(Math.max(0, buf.length - EOCD_SEARCH));
  const cdStart = findEndOfCentralDirectory(tail, buf.length).cdOffset;
  const byOffset = [...entries].sort((a, b) => a.localOffset - b.localOffset);
  let expected = 0;
  const dataOf = new Map<ZipEntry, Uint8Array>();
  for (const e of byOffset) {
    if (e.localOffset !== expected) throw new ZipError([`member \`${e.name}\` does not start where the previous one ends`]);
    const header = buf.subarray(e.localOffset, e.localOffset + 30);
    const headerLen = localHeaderLength(header, e);
    const localNameLen = u16(header, 26);
    const localName = buf.subarray(e.localOffset + 30, e.localOffset + 30 + localNameLen);
    if (new TextDecoder().decode(localName) !== e.name) throw new ZipError([`member \`${e.name}\` has a different name in its local header`]);
    if (u16(header, 8) !== e.method) throw new ZipError([`member \`${e.name}\` has a different method in its local header`]);
    const start = e.localOffset + headerLen;
    const end = start + e.compressedSize;
    if (end > cdStart) throw new ZipError([`member \`${e.name}\` runs into the central directory`]);
    dataOf.set(e, buf.subarray(start, end));
    expected = end;
    // A data descriptor (flag bit 3) may follow: 12 bytes, or 16 with its optional signature.
    if (e.flags & 0x8) {
      expected += u32(buf, end) === 0x08074b50 ? 16 : 12;
    }
  }
  if (expected !== cdStart) throw new ZipError(["there are bytes between the last member and the central directory"]);

  let spec = "";
  for (const e of entries) {
    const isSpec = e.name === specName;
    const chunks: Uint8Array[] = [];
    streamMember(dataOf.get(e)!, e, (chunk) => {
      if (isSpec) chunks.push(chunk.slice());
    });
    if (isSpec) spec = new TextDecoder().decode(concat(chunks));
  }
  return { entries, spec, size: total };
}

function concat(chunks: Uint8Array[]): Uint8Array {
  const out = new Uint8Array(chunks.reduce((n, c) => n + c.length, 0));
  let o = 0;
  for (const c of chunks) {
    out.set(c, o);
    o += c.length;
  }
  return out;
}

/** The uncompressed bytes of `entry`, from an archive in memory. */
export function extract(buf: Uint8Array, entry: ZipEntry): Uint8Array {
  const header = buf.subarray(entry.localOffset, entry.localOffset + 30);
  const start = entry.localOffset + localHeaderLength(header, entry);
  return inflateMember(buf.subarray(start, start + entry.compressedSize), entry);
}

/** Inflates a member's raw data (already validated at publish). */
export function inflateMember(data: Uint8Array, entry: ZipEntry): Uint8Array {
  if (entry.method === 0) return data;
  if (entry.method === 8) return inflateSync(data, { out: new Uint8Array(entry.size) });
  throw new ZipError([`${entry.name}: unsupported compression method ${entry.method}`]);
}

/**
 * Reads one member from an R2 object with ranged gets (end record, central
 * directory, local header, data), without fetching the whole archive.
 */
export async function readMemberFromR2(
  bucket: R2Bucket,
  key: string,
  memberName: string,
  maxSize = Infinity,
): Promise<{ entry: ZipEntry; bytes: Uint8Array | null; object: R2Object } | null> {
  const head = await bucket.head(key);
  if (!head) return null;
  const size = head.size;
  const tailLen = Math.min(size, EOCD_SEARCH);
  const tail = await rangeBytes(bucket, key, size - tailLen, tailLen);
  const eocd = findEndOfCentralDirectory(tail, size);
  const cd = await rangeBytes(bucket, key, eocd.cdOffset, eocd.cdSize);
  const entry = parseCentralDirectory(cd, eocd.entries).find((e) => e.name === memberName);
  if (!entry) return null;
  if (entry.size > maxSize) return { entry, bytes: null, object: head };
  const header = await rangeBytes(bucket, key, entry.localOffset, 30);
  const start = entry.localOffset + localHeaderLength(header, entry);
  const data = entry.compressedSize === 0 ? new Uint8Array(0) : await rangeBytes(bucket, key, start, entry.compressedSize);
  return { entry, bytes: inflateMember(data, entry), object: head };
}

async function rangeBytes(bucket: R2Bucket, key: string, offset: number, length: number): Promise<Uint8Array> {
  const obj = await bucket.get(key, { range: { offset, length } });
  if (!obj) throw new Error(`R2 object ${key} disappeared`);
  return new Uint8Array(await obj.arrayBuffer());
}
