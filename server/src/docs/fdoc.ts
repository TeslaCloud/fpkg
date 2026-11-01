// fdoc wasm glue. wasm/fdoc.wasm imports nothing and exports a C ABI:
//   fdoc_alloc(len: u32) -> *mut u8
//   fdoc_free(ptr: *mut u8, len: u32)
//   fdoc_generate(in_ptr: *const u8, in_len: u32, out_len: *mut u32) -> *mut u8
//   fdoc_version() -> *const u8          NUL-terminated
//   fdoc_last_panic() -> *const u8       NUL-terminated, empty if none
// Each call gets a fresh instance, so grown memory is freed and a trap cannot break the next call.

import FDOC from "../../wasm/fdoc.wasm";

export interface FdocFile {
  path: string;
  text: string;
}

export interface FdocInput {
  title?: string | null;
  version?: string | null;
  source_url?: string | null;
  files: FdocFile[];
}

export interface FdocPage {
  path: string;
  content: string;
}

export interface FdocOutput {
  pages: FdocPage[];
  warnings: string[];
}

/** fdoc refused the input or failed; deterministic, so not worth retrying. */
export class FdocError extends Error {}

interface Exports {
  memory: WebAssembly.Memory;
  fdoc_alloc(len: number): number;
  fdoc_free(ptr: number, len: number): void;
  fdoc_generate(inPtr: number, inLen: number, outLenPtr: number): number;
  fdoc_version(): number;
  fdoc_last_panic(): number;
}

async function instantiate(module: WebAssembly.Module): Promise<Exports> {
  const instance = await WebAssembly.instantiate(module, {});
  return instance.exports as unknown as Exports;
}

function readCString(x: Exports, ptr: number): string {
  const mem = new Uint8Array(x.memory.buffer);
  let end = ptr;
  while (end < mem.length && mem[end] !== 0) end++;
  return new TextDecoder().decode(mem.subarray(ptr, end));
}

/** fdoc's crate version, from the module itself. */
export async function fdocVersion(module: WebAssembly.Module = FDOC): Promise<string> {
  const x = await instantiate(module);
  return readCString(x, x.fdoc_version());
}

/** Runs `fdoc::generate` on `input` in a fresh instance of the module. */
export async function fdocGenerate(input: FdocInput, module: WebAssembly.Module = FDOC): Promise<FdocOutput> {
  return fdocGenerateBytes(new TextEncoder().encode(JSON.stringify(input)), module);
}

/** `fdocGenerate` on input that is already UTF-8 JSON. */
export async function fdocGenerateBytes(bytes: Uint8Array, module: WebAssembly.Module = FDOC): Promise<FdocOutput> {
  const x = await instantiate(module);
  const inPtr = x.fdoc_alloc(bytes.length);
  // `memory.buffer` is replaced whenever memory grows; take it after every call.
  new Uint8Array(x.memory.buffer, inPtr, bytes.length).set(bytes);
  const lenPtr = x.fdoc_alloc(4);
  let outPtr: number;
  try {
    outPtr = x.fdoc_generate(inPtr, bytes.length, lenPtr);
  } catch (e) {
    let panic = "";
    try {
      panic = readCString(x, x.fdoc_last_panic());
    } catch {
      // The instance is unusable; it is dropped either way.
    }
    throw new FdocError(panic || `fdoc trapped: ${e instanceof Error ? e.message : String(e)}`);
  }
  const outLen = new DataView(x.memory.buffer).getUint32(lenPtr, true);
  if (outPtr === 0 || outLen === 0) throw new FdocError("fdoc returned no output");
  const text = new TextDecoder().decode(new Uint8Array(x.memory.buffer, outPtr, outLen));
  x.fdoc_free(outPtr, outLen);
  x.fdoc_free(inPtr, bytes.length);
  x.fdoc_free(lenPtr, 4);
  const out = JSON.parse(text) as Partial<FdocOutput> & { error?: string };
  if (typeof out.error === "string") throw new FdocError(out.error);
  if (!Array.isArray(out.pages)) throw new FdocError("fdoc returned no pages");
  return { pages: out.pages, warnings: out.warnings ?? [] };
}
