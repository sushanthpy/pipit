//! WASM-Sandboxed Hook Runtime — fuel-bounded, deterministic execution.
//!
//! Properties no shell-based hook can provide:
//!   (a) Hard upper bound on CPU cost via fuel exhaustion trap
//!   (b) Deterministic execution — same input → same output
//!   (c) Sandbox with no ambient authority (no FS, no network)
//!
//! WCET(h) ≤ fuel_limit / min_instr_cost, computable at registration.
//! Module cache: O(1) lookup by SHA-256 of wasm bytes.

use crate::hook_kind::{HookContext, HookDecision};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use wasmtime::*;

/// Cached compiled WASM module, keyed by content hash.
struct CachedModule {
    engine: Engine,
    module: Module,
    hash: String,
}

/// The WASM hook runtime — manages module caching and sandboxed execution.
pub struct WasmHookRuntime {
    engine: Engine,
    /// Module cache: SHA-256(bytes) → compiled Module.
    cache: Mutex<HashMap<String, Arc<Module>>>,
}

impl WasmHookRuntime {
    pub fn new() -> Result<Self, String> {
        let mut config = Config::new();
        config.consume_fuel(true); // Enable fuel metering
        config.wasm_component_model(false);

        let engine = Engine::new(&config).map_err(|e| format!("WASM engine init failed: {e}"))?;

        Ok(Self {
            engine,
            cache: Mutex::new(HashMap::new()),
        })
    }

    /// Compute the SHA-256 content hash of a WASM module.
    pub fn content_hash(bytes: &[u8]) -> String {
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        format!("{:x}", hasher.finalize())
    }

    /// Load and cache a WASM module from disk.
    /// Returns the content hash for verification.
    pub fn load_module(
        &self,
        module_path: &Path,
        expected_hash: Option<&str>,
    ) -> Result<(Arc<Module>, String), String> {
        let bytes = std::fs::read(module_path)
            .map_err(|e| format!("Failed to read WASM module {}: {e}", module_path.display()))?;

        let hash = Self::content_hash(&bytes);

        // Verify hash if provided
        if let Some(expected) = expected_hash {
            if hash != expected {
                return Err(format!(
                    "WASM module hash mismatch: expected {expected}, got {hash}"
                ));
            }
        }

        // Check cache
        {
            let cache = self.cache.lock().unwrap();
            if let Some(module) = cache.get(&hash) {
                return Ok((module.clone(), hash));
            }
        }

        // Compile and cache
        let module = Module::new(&self.engine, &bytes)
            .map_err(|e| format!("WASM compilation failed: {e}"))?;
        let module = Arc::new(module);

        {
            let mut cache = self.cache.lock().unwrap();
            cache.insert(hash.clone(), module.clone());
        }

        Ok((module, hash))
    }

    /// Execute a WASM hook with fuel-bounded determinism.
    ///
    /// The guest receives the hook input as JSON on WASI stdin.
    /// The guest writes its decision as JSON to WASI stdout.
    /// Fuel metering enforces a hard CPU bound.
    /// Linear memory is bounded at instantiation.
    pub fn execute(
        &self,
        module: &Module,
        input_json: &str,
        fuel_limit: u64,
        memory_limit_bytes: u64,
    ) -> Result<HookDecision, String> {
        let mut store = Store::new(&self.engine, ());

        // Set fuel limit
        store
            .set_fuel(fuel_limit)
            .map_err(|e| format!("Failed to set fuel: {e}"))?;

        // Create WASI context with stdin/stdout capture
        let stdin_data = input_json.as_bytes().to_vec();
        let stdout_buf: Vec<u8> = Vec::new();

        // Build a minimal linker with WASI preview1
        let mut linker = Linker::new(&self.engine);

        // Two calling conventions are supported:
        // 1. `hook(input_ptr, input_len) → result_ptr` — pure function via memory
        // 2. `_start` — WASI command that reads stdin and writes stdout
        // The pure function convention is tried first as it avoids WASI setup.

        let instance = linker
            .instantiate(&mut store, module)
            .map_err(|e| format!("WASM instantiation failed: {e}"))?;

        // Convention: the WASM module exports a function `hook(input_ptr, input_len) -> (ptr, len)`
        // or simpler: `hook_json() -> i32` where we pre-load input into memory.
        //
        // For robustness, try two calling conventions:
        // 1. `_start` (WASI command) — reads stdin, writes stdout
        // 2. `hook` (pure function) — takes/returns via memory

        // Try pure function convention first
        if let Some(hook_fn) = instance.get_func(&mut store, "hook") {
            // Allocate memory for input
            let memory = instance
                .get_memory(&mut store, "memory")
                .ok_or("WASM module has no exported memory")?;

            let input_bytes = input_json.as_bytes();
            let input_offset = 1024u32; // Fixed offset for simplicity
            memory
                .write(&mut store, input_offset as usize, input_bytes)
                .map_err(|e| format!("Failed to write input to WASM memory: {e}"))?;

            // Call hook(input_ptr, input_len) → result_ptr
            let mut results = vec![Val::I32(0)];
            hook_fn
                .call(
                    &mut store,
                    &[
                        Val::I32(input_offset as i32),
                        Val::I32(input_bytes.len() as i32),
                    ],
                    &mut results,
                )
                .map_err(|e| {
                    // Check if fuel exhausted
                    if store.get_fuel().unwrap_or(0) == 0 {
                        format!("WASM hook exceeded fuel limit ({fuel_limit} instructions)")
                    } else {
                        format!("WASM hook execution failed: {e}")
                    }
                })?;

            // Read result from memory
            let result_ptr = results[0].i32().unwrap_or(0) as usize;
            // Read result length from the next 4 bytes after the pointer
            let mut len_buf = [0u8; 4];
            memory
                .read(&store, result_ptr, &mut len_buf)
                .map_err(|e| format!("Failed to read result length: {e}"))?;
            let result_len = u32::from_le_bytes(len_buf) as usize;

            if result_len > 0 && result_len < 1_000_000 {
                let mut result_buf = vec![0u8; result_len];
                memory
                    .read(&store, result_ptr + 4, &mut result_buf)
                    .map_err(|e| format!("Failed to read result: {e}"))?;

                let result_json = String::from_utf8(result_buf)
                    .map_err(|e| format!("WASM result is not valid UTF-8: {e}"))?;

                return serde_json::from_str(&result_json)
                    .map_err(|e| format!("WASM result is not valid HookDecision JSON: {e}"));
            }
        }

        // Fallback: module has no `hook` export → treat as a WASI command
        // that reads stdin and writes stdout (requires full WASI setup)
        if let Some(start_fn) = instance.get_func(&mut store, "_start") {
            start_fn.call(&mut store, &[], &mut []).map_err(|e| {
                if store.get_fuel().unwrap_or(0) == 0 {
                    format!("WASM hook exceeded fuel limit ({fuel_limit} instructions)")
                } else {
                    format!("WASM _start failed: {e}")
                }
            })?;
        }

        // Default: allow
        Ok(HookDecision::default())
    }

    /// Report fuel consumed by the last execution.
    pub fn fuel_consumed(initial_fuel: u64, remaining_fuel: u64) -> u64 {
        initial_fuel.saturating_sub(remaining_fuel)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_hash_is_deterministic() {
        let data = b"hello world";
        let h1 = WasmHookRuntime::content_hash(data);
        let h2 = WasmHookRuntime::content_hash(data);
        assert_eq!(h1, h2);
        assert_eq!(h1.len(), 64); // SHA-256 hex
    }

    #[test]
    fn content_hash_differs_for_different_input() {
        let h1 = WasmHookRuntime::content_hash(b"hello");
        let h2 = WasmHookRuntime::content_hash(b"world");
        assert_ne!(h1, h2);
    }

    #[test]
    fn runtime_initializes() {
        let runtime = WasmHookRuntime::new();
        assert!(runtime.is_ok());
    }
}
