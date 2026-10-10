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
    let component = COMPONENT_REGISTRY.get_or_load_binary("user_tailwind", wasm_bytes)?;
    *USER_TAILWIND_COMPONENT.write().unwrap() = Some(component);

    // Invalidate thread-local instance so it re-instantiates with the new binary
    ACTIVE_TAILWIND.with(|cell| {
        *cell.borrow_mut() = None;
    });

    Ok(())
}

/// Attempt to find a component either from dynamic registration or runtime filesystem.
fn get_or_load_component() -> Option<Arc<Component>> {
    if let Some(comp) = USER_TAILWIND_COMPONENT.read().unwrap().as_ref() {
        return Some(comp.clone());
    }

    // Optional runtime file fallback
    let candidate_paths = [
        "railwind.wasm",
        "components/script/wasm_host/railwind.wasm",
        "../railwind/target/wasm32-wasip1/release/railwind.wasm",
    ];

    for path in candidate_paths {
        if let Ok(bytes) = std::fs::read(path) {
            if let Ok(comp) = COMPONENT_REGISTRY.get_or_load_binary(path, &bytes) {
                return Some(comp);
            }
        }
    }

    None
}

/// Invokes the guest `compile-candidates` export function on the Wasm Component.
pub fn compile_candidates(candidates: &[String]) -> wasmtime::Result<String> {
    if !servo_config::pref!(dom_wasm_tailwindcss_enabled) || candidates.is_empty() {
        return Ok(String::new());
    }

    ACTIVE_TAILWIND.with(|instance_cell| {
        let mut borrow = instance_cell.borrow_mut();

        if borrow.is_none() {
            let Some(component) = get_or_load_component() else {
                return Ok(String::new());
            };

            let engine = &*SERVO_WASM_ENGINE;
            let linker = &*TAILWIND_LINKER;

            let mut store = Store::new(engine, ());
            let tailwind = Tailwind::instantiate(&mut store, &component, linker)?;

            *borrow = Some((store, tailwind));
        }

        let (store, tailwind) = borrow.as_mut().unwrap();

        tailwind
            .servo_tailwind_compiler()
            .call_compile_candidates(store, candidates)
    })
}
