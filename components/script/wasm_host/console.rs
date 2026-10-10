/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use super::state::WasmHostState;
use super::wit_dom::servo::dom::console::Host as ConsoleHost;
use wasmtime::StoreContextMut;
use wasmtime::component::Linker;

pub fn host_log(message: &str) {
    println!("[Servo Wasm console.log]: {message}");
}
pub fn host_error(message: &str) {
    eprintln!("[Servo Wasm console.error]: {message}");
}

impl ConsoleHost for WasmHostState {
    fn log(&mut self, message: String) {
        host_log(&message);
    }
    fn error(&mut self, message: String) {
        host_error(&message);
    }
}

/// Automatically registers the console interface onto any Wasmtime component Linker,
pub fn add_to_linker<T: Send + 'static>(linker: &mut Linker<T>) -> wasmtime::Result<()> {
    // Support both servo:dom/console@0.1.0 and servo:tailwind/console@0.1.0:
    for instance_name in &[
        "servo:dom/console@0.1.0",
        "servo:dom/console",
        "servo:tailwind/console@0.1.0",
        "servo:tailwind/console",
    ] {
        let mut instance = linker.instance(instance_name)?;
        // 1-tuple (message,): (String,) is required:
        instance.func_wrap("log", |_caller, (message,): (String,)| {
            host_log(&message);
            Ok(())
        })?;
        // Add the missing error implementation:
        instance.func_wrap("error", |_caller, (message,): (String,)| {
            host_error(&message);
            Ok(())
        })?;
    }
    Ok(())
}