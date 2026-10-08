/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

#![allow(unsafe_code)]

pub mod canvas;
pub mod console;
pub mod video;
pub mod dom;
pub mod state;

pub use state::WasmHostState;

use std::cell::RefCell;
use std::sync::LazyLock;

use js::context::JSContext;
use script_bindings::realms::enter_auto_realm;
use servo_wasm::{COMPONENT_REGISTRY, SERVO_WASM_ENGINE};
use wit_dom::servo::dom::events::{EventPayload, KeyboardData, Modifiers, MouseData};
use wasmtime::Store;
use wasmtime::component::{HasSelf, Linker};

use crate::dom::bindings::root::DomRoot;
use crate::dom::globalscope::GlobalScope;

pub mod wit_dom {
    wasmtime::component::bindgen!({
        world: "app",
        path: "wasm_host/wit",
    });
}

impl wit_dom::servo::dom::events::Host for WasmHostState {}

use wit_dom::App;

thread_local! {
    pub static ACTIVE_WASM_APPS: RefCell<Vec<Option<(Store<WasmHostState>, App)>>> = RefCell::new(Vec::new());
}

static SCRIPT_LINKER: LazyLock<Linker<WasmHostState>> = LazyLock::new(|| {
    let engine = &*SERVO_WASM_ENGINE;
    let mut linker = Linker::new(engine);
    App::add_to_linker::<_, HasSelf<_>>(&mut linker, |state: &mut WasmHostState| state)
        .expect("[Servo Wasm Host]: Failed to register host APIs in Linker");
    linker
});

pub fn dispatch_wasm_event(
    cx: &mut JSContext,
    app_index: usize,
    handler_id: &str,
    event_type: &str,
    mouse_data: Option<(f64, f64, f64, f64, u16, u16, bool, bool, bool, bool)>,
    key_data: Option<(String, String, bool, bool, bool, bool, bool)>,
) {
    let instance = ACTIVE_WASM_APPS.with(|apps| {
        let mut apps_borrow = apps.borrow_mut();
        if app_index < apps_borrow.len() {
            apps_borrow[app_index].take()
        } else {
            None
        }
    });
    // Construct the typed WIT EventPayload
    let payload = if let Some((ox, oy, cx, cy, btn, btns, s, c, a, m)) = mouse_data {
        EventPayload::Mouse(MouseData {
            offset_x: ox,
            offset_y: oy,
            client_x: cx,
            client_y: cy,
            button: btn,
            buttons: btns,
            mods: Modifiers { shift: s, ctrl: c, alt: a, meta: m },
        })
    } else if let Some((k, code, repeat, s, c, a, m)) = key_data {
        EventPayload::Keyboard(KeyboardData {
            key: k,
            code,
            repeat,
            mods: Modifiers { shift: s, ctrl: c, alt: a, meta: m },
        })
    } else {
        EventPayload::None
    };
    if let Some((mut store, app)) = instance {
        store.data_mut().cx = cx;
        let global = store.data().global_scope.clone();
        let _realm = enter_auto_realm::<crate::dom::bindings::codegen::DomTypeHolder::DomTypeHolder>(
            cx, &*global,
        );
        if let Err(e) = app.call_on_event(&mut store, handler_id, event_type, &payload) {
            eprintln!("[Servo Wasm Error on-event]: {:?}", e);
        }
        ACTIVE_WASM_APPS.with(|apps| {
            let mut apps_borrow = apps.borrow_mut();
            if app_index < apps_borrow.len() {
                apps_borrow[app_index] = Some((store, app));
            }
        });
    }
}

pub fn run_wasm_component(
    cx: &mut JSContext,
    wasm_bytes: &[u8],
    global_scope: DomRoot<GlobalScope>,
) -> wasmtime::Result<()> {
    println!(
        "[Servo Wasm Debug]: run_wasm_component received {} bytes",
        wasm_bytes.len()
    );
    let engine = &*SERVO_WASM_ENGINE;
    let linker = &*SCRIPT_LINKER;

    let component = COMPONENT_REGISTRY.get_or_load_binary("inline_script", wasm_bytes)?;
    let mut store = Store::new(engine, WasmHostState::new(cx, global_scope));

    let app = App::instantiate(&mut store, &component, linker)?;

    println!("[Servo Wasm Host]: Starting component execution...");
    app.call_run(&mut store)?;
    println!("[Servo Wasm Host]: Component execution finished successfully.");

    ACTIVE_WASM_APPS.with(|apps| {
        apps.borrow_mut().push(Some((store, app)));
    });

    Ok(())
}
