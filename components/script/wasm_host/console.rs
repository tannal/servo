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
/// matching any target instance name (e.g. "servo:tailwind/console" or "servo:dom/console").
pub fn add_to_linker<T>(linker: &mut Linker<T>, instance_name: &str) -> wasmtime::Result<()> {
    let mut inst = linker.instance(instance_name)?;
    inst.func_wrap("log", |_caller: StoreContextMut<'_, T>, (message,): (String,)| {
        host_log(&message);
        Ok(())
    })?;
    inst.func_wrap("error", |_caller: StoreContextMut<'_, T>, (message,): (String,)| {
        host_error(&message);
        Ok(())
    })?;
    Ok(())
}