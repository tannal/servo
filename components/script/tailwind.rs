/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Native Tailwind CSS support.
//!
//! `railwind.wasm` is a separate WebAssembly component.  The host records every
//! `class` token written through the native DOM interface and, at the end of the
//! current task, asks the railwind component to compile the accumulated tokens.
//! The resulting stylesheet (preflight + utility rules) is injected into the
//! document.  The application component itself contains no CSS.

#![allow(unsafe_code)]

use std::cell::RefCell;
use std::collections::HashSet;
use std::sync::{Arc, LazyLock};

use js::context::JSContext;
use script_bindings::codegen::GenericUnionTypes::{
    StringOrElementCreationOptions, TrustedHTMLOrTrustedScriptOrTrustedScriptURLOrString,
};
use servo_wasm::SERVO_WASM_ENGINE;
use wasmtime::component::{Component, HasSelf, Linker};
use wasmtime::Store;

use crate::dom::bindings::codegen::Bindings::DocumentBinding::DocumentMethods;
use crate::dom::bindings::codegen::Bindings::ElementBinding::ElementMethods;
use crate::dom::bindings::codegen::Bindings::NodeBinding::NodeMethods;
use crate::dom::bindings::codegen::Bindings::WindowBinding::WindowMethods;
use crate::dom::bindings::inheritance::Castable;
use crate::dom::bindings::root::DomRoot;
use crate::dom::bindings::str::DOMString;
use crate::dom::element::Element;
use crate::dom::globalscope::GlobalScope;
use crate::dom::node::Node;

pub mod wit_tailwind {
    wasmtime::component::bindgen!({
        world: "railwind",
        path: "wit/railwind.wit",
    });
}

use wit_tailwind::Railwind;
use wit_tailwind::servo::tailwind::console::Host as ConsoleHost;

#[derive(Default)]
pub struct RailwindState;

impl ConsoleHost for RailwindState {
    fn log(&mut self, message: String) {
        println!("[Servo Tailwind]: {message}");
    }

    fn error(&mut self, message: String) {
        eprintln!("[Servo Tailwind]: {message}");
    }
}

static TAILWIND_LINKER: LazyLock<Linker<RailwindState>> = LazyLock::new(|| {
    let mut linker = Linker::new(&*SERVO_WASM_ENGINE);
    Railwind::add_to_linker::<_, HasSelf<_>>(&mut linker, |state: &mut RailwindState| state)
        .expect("[Servo Tailwind]: failed to register console host API");
    linker
});

/// Tailwind's preflight.  This is runtime infrastructure, not application CSS:
/// pages load only their `.wasm` components.  It also contains the small number
/// of browser-level rules (scrollbars, keyframes, file picker) that are not
/// expressible as utilities in the current railwind subset.
const PREFLIGHT: &str = r#"
*,::before,::after{box-sizing:border-box;border-width:0;border-style:solid;border-color:#334155}
html,body{height:100%;margin:0}
body{background:#020617;color:#e8ebf0;font-family:Inter,"Segoe UI","Noto Sans",system-ui,sans-serif;font-size:14px;line-height:1.55;-webkit-font-smoothing:antialiased}
button,input,textarea{font:inherit;color:inherit;background:transparent}
button{cursor:pointer}
textarea{resize:none}
::placeholder{color:#64748b}
::-webkit-scrollbar{width:10px;height:10px}
::-webkit-scrollbar-thumb{background:#2a313c;border-radius:8px;border:3px solid transparent;background-clip:padding-box}
::-webkit-scrollbar-track{background:transparent}
input[type=file]{font-size:11px;color:#94a3b8}
input[type=file]::file-selector-button{background:#1e293b;color:#e2e8f0;border:1px solid #334155;border-radius:8px;padding:6px 10px;margin-right:8px;cursor:pointer;font-size:11px}
@keyframes servo-blink{0%,80%,100%{opacity:.25;transform:translateY(0)}40%{opacity:1;transform:translateY(-2px)}}
.servo-typing>span{width:6px;height:6px;border-radius:9999px;background:#94a3b8;display:inline-block;animation:servo-blink 1.2s infinite ease-in-out}
.servo-typing>span:nth-child(2){animation-delay:.18s}
.servo-typing>span:nth-child(3){animation-delay:.36s}
"#;

thread_local! {
    static ACTIVE_TAILWIND: RefCell<Option<(Store<RailwindState>, Railwind)>> = const { RefCell::new(None) };
    static SEEN: RefCell<HashSet<String>> = RefCell::new(HashSet::new());
    static DIRTY: RefCell<bool> = const { RefCell::new(true) };
    static GLOBAL: RefCell<Option<DomRoot<GlobalScope>>> = const { RefCell::new(None) };
    static STYLE: RefCell<Option<DomRoot<Element>>> = const { RefCell::new(None) };
}

/// A newer document invalidates the collected tokens and the injected sheet.
fn ensure_global(global: &GlobalScope) -> DomRoot<GlobalScope> {
    GLOBAL.with(|slot| {
        let mut slot = slot.borrow_mut();
        let same_document = slot
            .as_ref()
            .is_some_and(|existing| existing.pipeline_id() == global.pipeline_id());
        if !same_document {
            *slot = Some(DomRoot::from_ref(global));
            STYLE.with(|style| *style.borrow_mut() = None);
            DIRTY.with(|dirty| *dirty.borrow_mut() = true);
        }
        slot.as_ref().expect("global was just stored").clone()
    })
}

/// Instantiate `railwind.wasm` and make its compiler available to the document.
pub(crate) fn register_component(
    cx: &mut JSContext,
    component: &Arc<Component>,
    global: DomRoot<GlobalScope>,
) -> wasmtime::Result<()> {
    let mut store = Store::new(&*SERVO_WASM_ENGINE, RailwindState);
    let tailwind = Railwind::instantiate(&mut store, component, &TAILWIND_LINKER)?;
    ACTIVE_TAILWIND.with(|slot| *slot.borrow_mut() = Some((store, tailwind)));

    let _global = ensure_global(&global);
    DIRTY.with(|dirty| *dirty.borrow_mut() = true);
    flush(cx);
    Ok(())
}

/// Record class tokens written by an application component.
pub(crate) fn note_classes(global: &GlobalScope, classes: &str) {
    let added = SEEN.with(|seen| {
        let mut seen = seen.borrow_mut();
        let mut added = false;
        for token in classes.split_whitespace() {
            if seen.insert(token.to_string()) {
                added = true;
            }
        }
        added
    });
    if !added {
        return;
    }
    let _global = ensure_global(global);
    DIRTY.with(|dirty| *dirty.borrow_mut() = true);
}

/// Compile the current document's class tokens and refresh the injected sheet.
/// This is called synchronously after every guest entry point (run, event,
/// fetch completion) and when railwind itself is registered.  Keeping it off
/// the task queue makes styling deterministic during document loading.
pub(crate) fn flush(cx: &mut JSContext) {
    let dirty = DIRTY.with(|dirty| dirty.replace(false));
    if !dirty && STYLE.with(|style| style.borrow().is_some()) {
        return;
    }
    let Some(global) = GLOBAL.with(|slot| slot.borrow().clone()) else {
        return;
    };

    // Classes can be written through several paths (static template
    // attributes, dynamic attribute patches, host-created nodes).  Scanning the
    // live document makes the stylesheet independent of mutation ordering and
    // of whether the application happened to run before railwind loaded.
    let document = global.as_window().Document();
    let mut class_set: HashSet<String> = SEEN.with(|seen| seen.borrow().clone());
    if let Some(root) = document.GetDocumentElement() {
        collect_document_classes(cx, &root, &mut class_set);
    }
    let classes: Vec<String> = class_set.into_iter().collect();
    let compiled = compile(&classes).unwrap_or_else(|error| {
        eprintln!("[Servo Tailwind]: compilation failed: {error:?}");
        String::new()
    });
    println!(
        "[Servo Tailwind]: compiled {} classes ({} bytes)",
        classes.len(),
        compiled.len()
    );
    let stylesheet = format!("{PREFLIGHT}\n{compiled}");

    let style = match STYLE.with(|style| style.borrow().clone()) {
        Some(style) => style,
        None => {
            let style = match document.CreateElement(
                cx,
                DOMString::from_static("style"),
                StringOrElementCreationOptions::String(DOMString::new()),
            ) {
                Ok(style) => style,
                Err(_) => return,
            };
            let id = TrustedHTMLOrTrustedScriptOrTrustedScriptURLOrString::String(
                DOMString::from_static("servo-tailwind"),
            );
            let _ = style.SetAttribute(cx, DOMString::from_static("id"), id);
            if let Some(head) = document.GetHead() {
                let _ = head.upcast::<Node>().AppendChild(cx, style.upcast::<Node>());
            } else if let Some(body) = document.GetBody() {
                let _ = body.upcast::<Node>().AppendChild(cx, style.upcast::<Node>());
            }
            STYLE.with(|slot| *slot.borrow_mut() = Some(style.clone()));
            style
        },
    };

    let _ = style.upcast::<Node>().SetTextContent(cx, Some(DOMString::from(stylesheet)));
}

fn collect_document_classes(
    cx: &mut JSContext,
    root: &Element,
    classes: &mut HashSet<String>,
) {
    let mut stack: Vec<DomRoot<Node>> = vec![DomRoot::from_ref(root.upcast::<Node>())];
    while let Some(node) = stack.pop() {
        if let Some(element) = node.downcast::<Element>() {
            if let Some(class) = element.GetAttribute(cx, DOMString::from_static("class")) {
                for token in class.to_string().split_whitespace() {
                    classes.insert(token.to_string());
                }
            }
        }
        let mut child = node.GetFirstChild();
        while let Some(child_node) = child {
            child = child_node.GetNextSibling();
            stack.push(child_node);
        }
    }
}

fn compile(classes: &[String]) -> wasmtime::Result<String> {
    ACTIVE_TAILWIND.with(|slot| {
        let mut slot = slot.borrow_mut();
        let Some((store, tailwind)) = slot.as_mut() else {
            return Ok(String::new());
        };
        tailwind
            .servo_tailwind_compiler()
            .call_compile_candidates(store, classes)
    })
}

