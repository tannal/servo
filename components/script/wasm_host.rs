/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

#![allow(unsafe_code)]

use std::cell::RefCell;
use std::sync::LazyLock;

use js::context::JSContext;
use script_bindings::codegen::GenericUnionTypes::{
    BooleanOrScrollIntoViewOptions, StringOrElementCreationOptions,
    TrustedHTMLOrTrustedScriptOrTrustedScriptURLOrString,
};
use script_bindings::realms::enter_auto_realm;
use servo_wasm::net::{HttpResponse, fetch_blocking};
use servo_wasm::{COMPONENT_REGISTRY, SERVO_WASM_ENGINE};
use wasmtime::component::{HasSelf, Linker, Resource};
use wasmtime::Store;

use crate::dom::bindings::codegen::Bindings::DocumentBinding::DocumentMethods;
use crate::dom::bindings::codegen::Bindings::ElementBinding::ElementMethods;
use crate::dom::bindings::codegen::Bindings::HTMLElementBinding::HTMLElementMethods;
use crate::dom::bindings::codegen::Bindings::HTMLInputElementBinding::{
    HTMLInputElement, HTMLInputElementMethods,
};
use crate::dom::bindings::codegen::Bindings::HTMLTextAreaElementBinding::{
    HTMLTextAreaElement, HTMLTextAreaElementMethods,
};
use crate::dom::bindings::codegen::Bindings::KeyboardEventBinding::KeyboardEventMethods;
use crate::dom::bindings::codegen::Bindings::MouseEventBinding::MouseEventMethods;
use crate::dom::bindings::codegen::Bindings::NodeBinding::NodeMethods;
use crate::dom::bindings::codegen::Bindings::WindowBinding::WindowMethods;
use crate::dom::bindings::codegen::DomTypeHolder::DomTypeHolder;
use crate::dom::bindings::inheritance::Castable;
use crate::dom::bindings::root::DomRoot;
use crate::dom::bindings::str::DOMString;
use crate::dom::element::Element;
use crate::dom::event::Event;
use crate::dom::eventtarget::EventTarget;
use crate::dom::globalscope::GlobalScope;
use crate::dom::html::htmlelement::HTMLElement;
use crate::dom::keyboardevent::KeyboardEvent;
use crate::dom::mouseevent::MouseEvent;
use crate::dom::node::Node;
use crate::tasks::task::TaskOnce;

// Generate Rust types from the WIT definition using Wasmtime's Component Model
// bindgen.  The same WIT file is consumed by `cargo component` on the guest
// side, so this is the single source of truth for the ABI.
pub mod wit_dom {
    wasmtime::component::bindgen!({
        world: "app",
        path: "wit/servo_dom.wit",
    });
}

use wit_dom::servo::dom::document::EventData;
use wit_dom::App;
use wit_dom::servo::dom::console::Host as ConsoleHost;
use wit_dom::servo::dom::document::{Host as DocumentHost, HostElement};
use wit_dom::servo::dom::fetch::Host as FetchHost;

/// One instantiated guest component.  The `Store` is deliberately kept alive
/// for the lifetime of the document so that DOM event callbacks and fetch
/// completions can re-enter the same instance without re-instantiating it.
struct WasmApp {
    store: Store<WasmHostState>,
    app: App,
}

thread_local! {
    /// Slots are indexed by `app_id`, which is also encoded into every event
    /// handler id.  An empty slot means "instantiation in progress or failed".
    static ACTIVE_WASM_APPS: RefCell<Vec<Option<WasmApp>>> = const { RefCell::new(Vec::new()) };
}

fn take_app(app_id: usize) -> Option<WasmApp> {
    ACTIVE_WASM_APPS.with(|apps| {
        apps.borrow_mut()
            .get_mut(app_id)
            .and_then(|slot| slot.take())
    })
}

fn put_app(app_id: usize, app: WasmApp) {
    ACTIVE_WASM_APPS.with(|apps| {
        if let Some(slot) = apps.borrow_mut().get_mut(app_id) {
            *slot = Some(app);
        }
    });
}

/// Run `f` with the guest instance temporarily removed from TLS.  Taking the
/// instance out prevents a host callback from re-entering the same `Store`
/// while Wasmtime still has it borrowed.
fn with_app(
    cx: &mut JSContext,
    app_id: usize,
    f: impl FnOnce(&mut Store<WasmHostState>, &App) -> wasmtime::Result<()>,
) {
    let Some(mut instance) = take_app(app_id) else {
        return;
    };

    instance.store.data_mut().cx = cx;
    let global = instance.store.data().global_scope.clone();
    {
        let _realm = enter_auto_realm::<DomTypeHolder>(cx, &*global);
        if let Err(error) = f(&mut instance.store, &instance.app) {
            eprintln!("[Servo Wasm Error]: {error:?}");
        }
    }

    // The guest may have written new Tailwind classes while handling this
    // callback.  Refresh the stylesheet before the next rendering opportunity.
    crate::tailwind::flush(cx);

    put_app(app_id, instance);
}

/// Global dispatcher called when a DOM event fires for a Wasm listener.
pub fn dispatch_wasm_event(
    cx: &mut JSContext,
    handler_id: &str,
    target: &EventTarget,
    event: &Event,
) {
    let (app_id, handler_id) = split_handler_id(handler_id);
    let event_data = build_event_data(handler_id, target, event);

    with_app(cx, app_id, |store, app| {
        app.call_on_event(store, &event_data)
    });
}

/// Handler ids are stored as `<app-id>:<guest-handler-id>` on the EventTarget.
/// This keeps components isolated without threading an app id through Servo's
/// generic event listener structures.
fn split_handler_id(handler_id: &str) -> (usize, &str) {
    match handler_id.split_once(':') {
        Some((app_id, rest)) => match app_id.parse::<usize>() {
            Ok(app_id) => (app_id, rest),
            Err(_) => (0, handler_id),
        },
        None => (0, handler_id),
    }
}

fn build_event_data(handler_id: &str, target: &EventTarget, event: &Event) -> EventData {
    let event_type = event.type_().to_string();

    let key = event
        .downcast::<KeyboardEvent>()
        .map(|event| event.Key().to_string())
        .unwrap_or_default();
    let shift = event
        .downcast::<KeyboardEvent>()
        .map(|event| event.ShiftKey())
        .unwrap_or(false);

    let (client_x, client_y) = event
        .downcast::<MouseEvent>()
        .map(|event| (event.ClientX() as f64, event.ClientY() as f64))
        .unwrap_or((0.0, 0.0));

    let (value, checked) = if let Some(input) = target.downcast::<HTMLInputElement>() {
        (input.Value().to_string(), input.Checked())
    } else if let Some(textarea) = target.downcast::<HTMLTextAreaElement>() {
        (textarea.Value().to_string(), false)
    } else {
        (String::new(), false)
    };

    EventData {
        handler_id: handler_id.to_string(),
        event_type,
        key,
        value,
        checked,
        shift,
        client_x,
        client_y,
    }
}

/// The host state held inside the Wasmtime `Store` for one component instance.
pub struct WasmHostState {
    /// Refreshed at the beginning of every event/fetch callback turn.  Servo
    /// owns the `JSContext`; the guest may call DOM APIs that need it.
    pub cx: *mut JSContext,
    pub global_scope: DomRoot<GlobalScope>,
    pub app_id: usize,
    /// Handle table for DOM resources exposed to the guest.  The guest owns
    /// handles; dropping a WIT `element` resource frees its slot.
    elements: Vec<Option<DomRoot<Node>>>,
    free_elements: Vec<u32>,
    next_fetch_id: u32,
}

impl WasmHostState {
    pub fn new(cx: &mut JSContext, global_scope: DomRoot<GlobalScope>, app_id: usize) -> Self {
        Self {
            cx: cx as *mut JSContext,
            global_scope,
            app_id,
            elements: Vec::new(),
            free_elements: Vec::new(),
            next_fetch_id: 1,
        }
    }

    fn alloc_node(&mut self, node: DomRoot<Node>) -> u32 {
        if let Some(id) = self.free_elements.pop() {
            self.elements[id as usize] = Some(node);
            id
        } else {
            let id = self.elements.len() as u32;
            self.elements.push(Some(node));
            id
        }
    }

    fn node(&self, rep: &Resource<wit_dom::servo::dom::document::Element>) -> Option<DomRoot<Node>> {
        self.elements.get(rep.rep() as usize)?.clone()
    }

    fn element(
        &self,
        rep: &Resource<wit_dom::servo::dom::document::Element>,
    ) -> Option<DomRoot<Element>> {
        let node = self.node(rep)?;
        Some(DomRoot::from_ref(node.downcast::<Element>()?))
    }

    fn input(
        &self,
        rep: &Resource<wit_dom::servo::dom::document::Element>,
    ) -> Option<DomRoot<HTMLInputElement>> {
        let node = self.node(rep)?;
        Some(DomRoot::from_ref(node.downcast::<HTMLInputElement>()?))
    }

    fn cx(&self) -> &mut JSContext {
        unsafe { &mut *self.cx }
    }
}

impl ConsoleHost for WasmHostState {
    fn log(&mut self, message: String) {
        println!("[Servo Wasm Console.log]: {message}");
    }

    fn error(&mut self, message: String) {
        eprintln!("[Servo Wasm Console.error]: {message}");
    }
}

impl HostElement for WasmHostState {
    fn parent_node(
        &mut self,
        self_rep: Resource<wit_dom::servo::dom::document::Element>,
    ) -> Option<Resource<wit_dom::servo::dom::document::Element>> {
        let parent = self.node(&self_rep)?.GetParentNode()?;
        let id = self.alloc_node(parent);
        Some(Resource::new_own(id))
    }

    fn next_sibling(
        &mut self,
        self_rep: Resource<wit_dom::servo::dom::document::Element>,
    ) -> Option<Resource<wit_dom::servo::dom::document::Element>> {
        let next = self.node(&self_rep)?.GetNextSibling()?;
        let id = self.alloc_node(next);
        Some(Resource::new_own(id))
    }

    fn replace_child(
        &mut self,
        self_rep: Resource<wit_dom::servo::dom::document::Element>,
        new_rep: Resource<wit_dom::servo::dom::document::Element>,
        old_rep: Resource<wit_dom::servo::dom::document::Element>,
    ) {
        let (Some(parent), Some(new_node), Some(old_node)) = (
            self.node(&self_rep),
            self.node(&new_rep),
            self.node(&old_rep),
        ) else {
            return;
        };
        let cx = self.cx();
        let _ = parent.ReplaceChild(cx, &new_node, &old_node);
    }

    fn add_event_listener(
        &mut self,
        self_rep: Resource<wit_dom::servo::dom::document::Element>,
        event_type: String,
        handler_id: String,
    ) {
        let Some(node) = self.node(&self_rep) else {
            return;
        };
        let handler_id = format!("{}:{}", self.app_id, handler_id);
        node.upcast::<EventTarget>()
            .add_wasm_event_listener(DOMString::from(event_type), DOMString::from(handler_id));
    }

    fn remove_event_listener(
        &mut self,
        _self_rep: Resource<wit_dom::servo::dom::document::Element>,
        _event_type: String,
        _handler_id: String,
    ) {
        // Removing native listeners is not needed for virtual-DOM patches,
        // which only create listeners on live nodes.  Kept for ABI symmetry.
    }

    fn drop(
        &mut self,
        rep: Resource<wit_dom::servo::dom::document::Element>,
    ) -> wasmtime::Result<()> {
        let id = rep.rep();
        if let Some(slot) = self.elements.get_mut(id as usize) {
            if slot.take().is_some() {
                self.free_elements.push(id);
            }
        }
        Ok(())
    }

    fn set_attribute(
        &mut self,
        self_rep: Resource<wit_dom::servo::dom::document::Element>,
        name: String,
        value: String,
    ) {
        let Some(element) = self.element(&self_rep) else {
            return;
        };
        if name == "class" {
            crate::tailwind::note_classes(&self.global_scope, &value);
        }
        let cx = self.cx();
        let value = TrustedHTMLOrTrustedScriptOrTrustedScriptURLOrString::String(DOMString::from(value));
        let _ = element.SetAttribute(cx, DOMString::from(name), value);
    }

    fn get_attribute(
        &mut self,
        self_rep: Resource<wit_dom::servo::dom::document::Element>,
        name: String,
    ) -> Option<String> {
        let element = self.element(&self_rep)?;
        let cx = self.cx();
        element
            .GetAttribute(cx, DOMString::from(name))
            .map(|value| value.to_string())
    }

    fn remove_attribute(
        &mut self,
        self_rep: Resource<wit_dom::servo::dom::document::Element>,
        name: String,
    ) {
        let Some(element) = self.element(&self_rep) else {
            return;
        };
        let cx = self.cx();
        element.RemoveAttribute(cx, DOMString::from(name));
    }

    fn get_property(
        &mut self,
        self_rep: Resource<wit_dom::servo::dom::document::Element>,
        name: String,
    ) -> String {
        if name == "value" {
            if let Some(input) = self.input(&self_rep) {
                return input.Value().to_string();
            }
            if let Some(node) = self.node(&self_rep) {
                if let Some(textarea) = node.downcast::<HTMLTextAreaElement>() {
                    return textarea.Value().to_string();
                }
            }
        } else if name == "checked" {
            if let Some(input) = self.input(&self_rep) {
                return input.Checked().to_string();
            }
        }
        String::new()
    }

    fn set_property(
        &mut self,
        self_rep: Resource<wit_dom::servo::dom::document::Element>,
        name: String,
        value: String,
    ) {
        if name == "value" {
            if let Some(input) = self.input(&self_rep) {
                let cx = self.cx();
                input.SetValue(cx, DOMString::from(value));
                return;
            }
            if let Some(node) = self.node(&self_rep) {
                if let Some(textarea) = node.downcast::<HTMLTextAreaElement>() {
                    let cx = self.cx();
                    textarea.SetValue(cx, DOMString::from(value));
                }
            }
        } else if name == "checked" {
            if let Some(input) = self.input(&self_rep) {
                let checked = matches!(value.as_str(), "true" | "checked" | "1");
                let cx = self.cx();
                input.SetChecked(cx, checked);
            }
        }
    }

    fn append_child(
        &mut self,
        self_rep: Resource<wit_dom::servo::dom::document::Element>,
        child_rep: Resource<wit_dom::servo::dom::document::Element>,
    ) {
        let (Some(parent), Some(child)) = (self.node(&self_rep), self.node(&child_rep)) else {
            return;
        };
        let cx = self.cx();
        let _ = parent.AppendChild(cx, &child);
    }

    fn insert_before(
        &mut self,
        self_rep: Resource<wit_dom::servo::dom::document::Element>,
        child_rep: Resource<wit_dom::servo::dom::document::Element>,
        reference_rep: Resource<wit_dom::servo::dom::document::Element>,
    ) {
        let (Some(parent), Some(child)) = (self.node(&self_rep), self.node(&child_rep)) else {
            return;
        };
        let cx = self.cx();
        let reference = self.node(&reference_rep);
        let _ = parent.InsertBefore(cx, &child, reference.as_deref());
    }

    fn remove_child(
        &mut self,
        self_rep: Resource<wit_dom::servo::dom::document::Element>,
        child_rep: Resource<wit_dom::servo::dom::document::Element>,
    ) {
        let (Some(parent), Some(child)) = (self.node(&self_rep), self.node(&child_rep)) else {
            return;
        };
        let cx = self.cx();
        let _ = parent.RemoveChild(cx, &child);
    }

    fn set_text_content(
        &mut self,
        self_rep: Resource<wit_dom::servo::dom::document::Element>,
        text: String,
    ) {
        let Some(node) = self.node(&self_rep) else {
            return;
        };
        let cx = self.cx();
        let _ = node.SetTextContent(cx, Some(DOMString::from(text)));
    }

    fn get_text_content(
        &mut self,
        self_rep: Resource<wit_dom::servo::dom::document::Element>,
    ) -> String {
        let Some(node) = self.node(&self_rep) else {
            return String::new();
        };
        node.GetTextContent()
            .map(|text| text.to_string())
            .unwrap_or_default()
    }

    fn focus(&mut self, self_rep: Resource<wit_dom::servo::dom::document::Element>) {
        let Some(element) = self.element(&self_rep) else {
            return;
        };
        let Some(html_element) = element.downcast::<HTMLElement>() else {
            return;
        };
        let cx = self.cx();
        html_element.Focus(cx, &Default::default());
    }

    fn blur(&mut self, self_rep: Resource<wit_dom::servo::dom::document::Element>) {
        let Some(element) = self.element(&self_rep) else {
            return;
        };
        let Some(html_element) = element.downcast::<HTMLElement>() else {
            return;
        };
        let cx = self.cx();
        html_element.Blur(cx);
    }

    fn scroll_into_view(&mut self, self_rep: Resource<wit_dom::servo::dom::document::Element>) {
        let Some(element) = self.element(&self_rep) else {
            return;
        };
        let cx = self.cx();
        element.ScrollIntoView(cx, BooleanOrScrollIntoViewOptions::Boolean(true));
    }

    fn file_count(&mut self, self_rep: Resource<wit_dom::servo::dom::document::Element>) -> u32 {
        let Some(input) = self.input(&self_rep) else {
            return 0;
        };
        let Some(files) = input.GetFiles() else {
            return 0;
        };
        files.iter_files().count() as u32
    }

    fn file_name(
        &mut self,
        self_rep: Resource<wit_dom::servo::dom::document::Element>,
        index: u32,
    ) -> String {
        let Some(input) = self.input(&self_rep) else {
            return String::new();
        };
        let Some(files) = input.GetFiles() else {
            return String::new();
        };
        files
            .iter_files()
            .nth(index as usize)
            .map(|file| file.name().to_string())
            .unwrap_or_default()
    }

    fn file_type(
        &mut self,
        self_rep: Resource<wit_dom::servo::dom::document::Element>,
        index: u32,
    ) -> String {
        let Some(input) = self.input(&self_rep) else {
            return String::new();
        };
        let Some(files) = input.GetFiles() else {
            return String::new();
        };
        files
            .iter_files()
            .nth(index as usize)
            .map(|file| file.file_type())
            .unwrap_or_default()
    }

    fn file_bytes(
        &mut self,
        self_rep: Resource<wit_dom::servo::dom::document::Element>,
        index: u32,
    ) -> Result<Vec<u8>, String> {
        let input = self
            .input(&self_rep)
            .ok_or_else(|| "element is not an <input>".to_string())?;
        let files = input
            .GetFiles()
            .ok_or_else(|| "no files selected".to_string())?;
        let file = files
            .iter_files()
            .nth(index as usize)
            .ok_or_else(|| format!("file index {index} out of range"))?;
        file.file_bytes()
            .map_err(|_| "could not read selected file".to_string())
    }

    fn clear_files(&mut self, self_rep: Resource<wit_dom::servo::dom::document::Element>) {
        // Setting `value` on a file input is a no-op in the current Servo DOM.
        // The guest recreates the input to reset the selection when needed.
        let _ = self_rep;
    }
}

impl DocumentHost for WasmHostState {
    fn create_text_node(
        &mut self,
        text: String,
    ) -> Result<Resource<wit_dom::servo::dom::document::Element>, String> {
        let cx = self.cx();
        let document = self.global_scope.as_window().Document();
        let text_node = document.CreateTextNode(cx, DOMString::from(text));
        let id = self.alloc_node(DomRoot::upcast(text_node));
        Ok(Resource::new_own(id))
    }

    fn create_element(
        &mut self,
        tag: String,
    ) -> Result<Resource<wit_dom::servo::dom::document::Element>, String> {
        let cx = self.cx();
        let document = self.global_scope.as_window().Document();
        let options = StringOrElementCreationOptions::String(DOMString::new());
        match document.CreateElement(cx, DOMString::from(tag), options) {
            Ok(element) => {
                let id = self.alloc_node(DomRoot::upcast(element));
                Ok(Resource::new_own(id))
            },
            Err(_) => Err("failed to create DOM element".to_string()),
        }
    }

    fn get_body(&mut self) -> Result<Resource<wit_dom::servo::dom::document::Element>, String> {
        let document = self.global_scope.as_window().Document();
        let body = document
            .GetBody()
            .ok_or_else(|| "document has no body".to_string())?;
        let id = self.alloc_node(DomRoot::upcast(body));
        Ok(Resource::new_own(id))
    }

    fn get_head(&mut self) -> Result<Resource<wit_dom::servo::dom::document::Element>, String> {
        let document = self.global_scope.as_window().Document();
        let head = document
            .GetHead()
            .ok_or_else(|| "document has no head".to_string())?;
        let id = self.alloc_node(DomRoot::upcast(head));
        Ok(Resource::new_own(id))
    }

    fn get_document_element(
        &mut self,
    ) -> Result<Resource<wit_dom::servo::dom::document::Element>, String> {
        let document = self.global_scope.as_window().Document();
        let root = document
            .GetDocumentElement()
            .ok_or_else(|| "document has no document element".to_string())?;
        let id = self.alloc_node(DomRoot::upcast(root));
        Ok(Resource::new_own(id))
    }

    fn get_element_by_id(
        &mut self,
        id: String,
    ) -> Result<Resource<wit_dom::servo::dom::document::Element>, String> {
        let cx = self.cx();
        let document = self.global_scope.as_window().Document();
        match document.GetElementById(cx, DOMString::from(id)) {
            Some(element) => {
                let id = self.alloc_node(DomRoot::upcast(element));
                Ok(Resource::new_own(id))
            },
            None => Err("element not found".to_string()),
        }
    }

    fn query_selector(
        &mut self,
        selector: String,
    ) -> Option<Resource<wit_dom::servo::dom::document::Element>> {
        let cx = self.cx();
        let document = self.global_scope.as_window().Document();
        match document.QuerySelector(cx, DOMString::from(selector)) {
            Ok(Some(element)) => {
                let id = self.alloc_node(DomRoot::upcast(element));
                Some(Resource::new_own(id))
            },
            _ => None,
        }
    }
}

/// A completed HTTP request, posted back to the ScriptThread as a task.
struct WasmFetchTask {
    app_id: usize,
    request_id: u32,
    result: Result<HttpResponse, String>,
}

impl TaskOnce for WasmFetchTask {
    fn name(&self) -> &'static str {
        "ServoWasmFetch"
    }

    fn run_once(self, cx: &mut JSContext) {
        let (ok, status, body, error) = match self.result {
            Ok(response) => (true, response.status, response.body, None),
            Err(error) => (false, 0, Vec::new(), Some(error)),
        };
        with_app(cx, self.app_id, |store, app| {
            app.call_on_fetch(store, self.request_id, ok, status, &body, error.as_deref())
        });
    }
}

impl FetchHost for WasmHostState {
    fn fetch(
        &mut self,
        url: String,
        method: String,
        headers: Vec<(String, String)>,
        body: Option<Vec<u8>>,
    ) -> u32 {
        let request_id = self.next_fetch_id;
        self.next_fetch_id = self.next_fetch_id.wrapping_add(1);
        let app_id = self.app_id;
        let task_source = self
            .global_scope
            .task_manager()
            .networking_task_source()
            .to_sendable();

        let thread_name = format!("servo-wasm-fetch-{request_id}");
        let spawn_result = std::thread::Builder::new()
            .name(thread_name)
            .spawn(move || {
                let result = fetch_blocking(&url, &method, headers, body);
                task_source.queue(WasmFetchTask {
                    app_id,
                    request_id,
                    result,
                });
            });
        if let Err(error) = spawn_result {
            eprintln!("[Servo Wasm]: could not spawn fetch thread: {error}");
        }

        request_id
    }
}

static SCRIPT_LINKER: LazyLock<Linker<WasmHostState>> = LazyLock::new(|| {
    let engine = &*SERVO_WASM_ENGINE;
    let mut linker = Linker::new(engine);
    App::add_to_linker::<_, HasSelf<_>>(&mut linker, |state: &mut WasmHostState| state)
        .expect("[Servo Wasm Host]: Failed to register host APIs in Linker");
    linker
});

fn component_cache_key(bytes: &[u8]) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut hasher);
    format!("inline:{:016x}", hasher.finish())
}

/// Runner entrypoint invoked from `HTMLScriptElement::execute`.
pub fn run_wasm_component(
    cx: &mut JSContext,
    wasm_bytes: &[u8],
    global_scope: DomRoot<GlobalScope>,
) -> wasmtime::Result<()> {
    let engine = &*SERVO_WASM_ENGINE;
    let component = COMPONENT_REGISTRY
        .get_or_load_binary(component_cache_key(wasm_bytes), wasm_bytes)?;

    // Every native component is loaded through the same `<script>` path.  The
    // component type tells us which host world it belongs to: an application
    // exports `run`, while the Tailwind component exports
    // `servo:tailwind/compiler`.  This is a typed check, not a filename guess.
    let is_application = component
        .component_type()
        .exports(engine)
        .any(|(name, _)| name == "run");
    if !is_application {
        return crate::tailwind::register_component(cx, &component, global_scope);
    }

    let app_id = ACTIVE_WASM_APPS.with(|apps| {
        let mut apps = apps.borrow_mut();
        apps.push(None);
        apps.len() - 1
    });

    let mut store = Store::new(
        engine,
        WasmHostState::new(cx, global_scope, app_id),
    );
    match App::instantiate(&mut store, &component, &SCRIPT_LINKER) {
        Ok(app) => {
            app.call_run(&mut store)?;
            crate::tailwind::flush(cx);
            put_app(app_id, WasmApp { store, app });
            Ok(())
        },
        Err(error) => {
            ACTIVE_WASM_APPS.with(|apps| {
                if let Some(slot) = apps.borrow_mut().get_mut(app_id) {
                    *slot = None;
                }
            });
            Err(error)
        },
    }
}
