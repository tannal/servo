/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

 use std::cell::RefCell;
 use std::sync::{Arc, LazyLock, RwLock};
 
 use servo_wasm::{COMPONENT_REGISTRY, SERVO_WASM_ENGINE};
 use wasmtime::Store;
 use wasmtime::component::{Component, Linker};
 
 wasmtime::component::bindgen!({
     world: "tailwind",
     path: "wasm_host/wit/tailwind",
 });
 
 static TAILWIND_LINKER: LazyLock<Linker<()>> = LazyLock::new(|| {
     let engine = &*SERVO_WASM_ENGINE;
     let mut linker = Linker::new(engine);
     // Automatically link the console implementation from console.rs
     crate::wasm_host::console::add_to_linker(&mut linker)
         .expect("[Servo Tailwind]: Failed to auto-link console implementation");
     linker
 });
 
 // COMPONENT_REGISTRY caches components inside Arc<Component>
 static USER_TAILWIND_COMPONENT: RwLock<Option<Arc<Component>>> = RwLock::new(None);
 
 thread_local! {
     static ACTIVE_TAILWIND: RefCell<Option<(Store<()>, Tailwind)>> = const { RefCell::new(None) };
 }
 
 /// Register a user-provided or dynamically fetched Tailwind Wasm component.
 pub fn register_tailwind_binary(wasm_bytes: &[u8]) -> wasmtime::Result<()> {
     eprintln!("[Tailwind Host]: Registering user tailwind binary ({} bytes)...", wasm_bytes.len());
     let component = COMPONENT_REGISTRY.get_or_load_binary("user_tailwind", wasm_bytes)?;
     *USER_TAILWIND_COMPONENT.write().unwrap() = Some(component);
 
     // Invalidate thread-local instance so it re-instantiates with the new binary
     ACTIVE_TAILWIND.with(|cell| {
         *cell.borrow_mut() = None;
     });
 
     eprintln!("[Tailwind Host]: User tailwind component registered successfully");
     Ok(())
 }
 
 /// Attempt to find a component either from dynamic registration or runtime filesystem.
 fn get_or_load_component() -> Option<Arc<Component>> {
     if let Some(comp) = USER_TAILWIND_COMPONENT.read().unwrap().as_ref() {
         eprintln!("[Tailwind Host]: Using user-registered Tailwind component");
         return Some(comp.clone());
     }
 
     let candidate_paths = [
         "railwind.wasm",
         "components/script/wasm_host/railwind.wasm",
         "../railwind/target/wasm32-wasip1/release/railwind.wasm",
     ];
 
     let cwd = std::env::current_dir()
         .map(|p| p.display().to_string())
         .unwrap_or_else(|_| "unknown".to_string());
 
     eprintln!("[Tailwind Host]: Searching for railwind component (cwd: {})...", cwd);
 
     for path in candidate_paths {
         match std::fs::read(path) {
             Ok(bytes) => {
                 eprintln!("[Tailwind Host]: Found '{}' ({} bytes), compiling component...", path, bytes.len());
                 match COMPONENT_REGISTRY.get_or_load_binary(path, &bytes) {
                     Ok(comp) => {
                         eprintln!("[Tailwind Host]: Successfully compiled Wasm component from '{}'", path);
                         return Some(comp);
                     }
                     Err(err) => {
                         eprintln!("[Tailwind Host ERROR]: Failed to compile component from '{}': {:?}", path, err);
                     }
                 }
             }
             Err(err) => {
                 eprintln!("[Tailwind Host]: Candidate path '{}' not available: {}", path, err);
             }
         }
     }
 
     eprintln!("[Tailwind Host ERROR]: Could not find or compile any railwind.wasm binary in candidate paths: {:?}", candidate_paths);
     None
 }
 
 /// Invokes the guest `compile-candidates` export function on the Wasm Component.
 pub fn compile_candidates(candidates: &[String]) -> wasmtime::Result<String> {
     if !servo_config::pref!(dom_wasm_tailwindcss_enabled) {
         eprintln!("[Tailwind Host]: compile_candidates skipped: pref 'dom_wasm_tailwindcss_enabled' is false");
         return Ok(String::new());
     }
 
     if candidates.is_empty() {
         eprintln!("[Tailwind Host]: compile_candidates skipped: candidate list is empty");
         return Ok(String::new());
     }
 
     eprintln!(
         "[Tailwind Host]: compile_candidates entered with {} candidate(s): {:?}",
         candidates.len(),
         candidates
     );
 
     ACTIVE_TAILWIND.with(|instance_cell| {
         let mut borrow = instance_cell.borrow_mut();
 
         if borrow.is_none() {
             eprintln!("[Tailwind Host]: No active guest instance on this thread, loading component...");
             let Some(component) = get_or_load_component() else {
                 return Err(wasmtime::Error::msg(
                     "get_or_load_component() failed: railwind.wasm not found or invalid component",
                 ));
             };
 
             let engine = &*SERVO_WASM_ENGINE;
             let linker = &*TAILWIND_LINKER;
 
             let mut store = Store::new(engine, ());
             eprintln!("[Tailwind Host]: Instantiating Tailwind component into store...");
             let tailwind = match Tailwind::instantiate(&mut store, &component, linker) {
                 Ok(inst) => {
                     eprintln!("[Tailwind Host]: Tailwind::instantiate succeeded");
                     inst
                 }
                 Err(err) => {
                     eprintln!("[Tailwind Host ERROR]: Tailwind::instantiate failed: {:?}", err);
                     return Err(err);
                 }
             };
 
             *borrow = Some((store, tailwind));
         }
 
         let (store, tailwind) = borrow.as_mut().unwrap();
 
         eprintln!(
             "[Tailwind Host]: Invoking guest call_compile_candidates with {:?}",
             candidates
         );
 
         let res = tailwind
             .servo_tailwind_compiler()
             .call_compile_candidates(store, candidates);
 
         match &res {
             Ok(css) => {
                 if css.is_empty() {
                     eprintln!(
                         "[Tailwind Host WARN]: Guest executed successfully but returned 0 bytes of CSS for candidates: {:?}",
                         candidates
                     );
                 } else {
                     eprintln!(
                         "[Tailwind Host]: Guest returned {} bytes of CSS",
                         css.len()
                     );
                 }
             }
             Err(e) => {
                 eprintln!("[Tailwind Host ERROR]: Guest call_compile_candidates trapped/failed: {:?}", e);
             }
         }
 
         res
     })
 }