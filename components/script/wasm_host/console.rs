/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use super::state::WasmHostState;
use super::wit_dom::servo::dom::console::Host as ConsoleHost;

impl ConsoleHost for WasmHostState {
    fn log(&mut self, message: String) {
        println!("[Servo Wasm Console.log]: {}", message);
    }

    fn error(&mut self, message: String) {
        eprintln!("[Servo Wasm Console.error]: {}", message);
    }
}
