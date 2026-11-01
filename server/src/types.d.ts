// Imported .wasm files are compiled modules (the CompiledWasm rule in wrangler.toml).
declare module "*.wasm" {
  const module: WebAssembly.Module;
  export default module;
}
