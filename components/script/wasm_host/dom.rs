/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use script_bindings::codegen::GenericUnionTypes::{
    StringOrElementCreationOptions, TrustedHTMLOrTrustedScriptOrTrustedScriptURLOrString,
};
use wasmtime::component::Resource;

use crate::dom::eventtarget::EventTarget;

use crate::dom::bindings::codegen::Bindings::DocumentBinding::DocumentMethods;
use crate::dom::bindings::codegen::Bindings::ElementBinding::ElementMethods;
use crate::dom::bindings::codegen::Bindings::HTMLInputElementBinding::{
    HTMLInputElement, HTMLInputElementMethods,
};
use crate::dom::bindings::codegen::Bindings::HTMLTextAreaElementBinding::{
    HTMLTextAreaElement, HTMLTextAreaElementMethods,
};
use crate::dom::bindings::codegen::Bindings::NodeBinding::NodeMethods;
use crate::dom::bindings::codegen::Bindings::WindowBinding::WindowMethods;
use crate::dom::bindings::inheritance::Castable;
use crate::dom::bindings::root::DomRoot;
use crate::dom::bindings::str::DOMString;
use crate::dom::element::Element;
use crate::dom::node::Node;

use super::state::WasmHostState;
use super::wit_dom::servo::dom::document::{
    Element as WitElement, Host as DocumentHost, HostElement,
};

impl HostElement for WasmHostState {
    fn parent_node(&mut self, self_rep: Resource<WitElement>) -> Option<Resource<WitElement>> {
        let id = self_rep.rep() as usize;
        let node = self.elements.get(id)?.as_ref()?;
        let parent = node.GetParentNode()?;
        let new_id = self.elements.len() as u32;
        self.elements.push(Some(parent));
        Some(Resource::new_own(new_id))
    }

    fn replace_child(
        &mut self,
        self_rep: Resource<WitElement>,
        new_rep: Resource<WitElement>,
        old_rep: Resource<WitElement>,
    ) {
        let parent_id = self_rep.rep() as usize;
        let new_id = new_rep.rep() as usize;
        let old_id = old_rep.rep() as usize;
        if let (Some(Some(parent)), Some(Some(new_node)), Some(Some(old_node))) = (
            self.elements.get(parent_id),
            self.elements.get(new_id),
            self.elements.get(old_id),
        ) {
            let cx = unsafe { &mut *self.cx };
            let _ = parent.ReplaceChild(cx, new_node, old_node);
        }
    }

    fn add_event_listener(
        &mut self,
        self_rep: Resource<WitElement>,
        event_type: String,
        handler_id: String,
    ) {
        let id = self_rep.rep() as usize;
        if let Some(Some(element)) = self.elements.get(id) {
            let event_target = element.upcast::<EventTarget>();
            event_target.add_wasm_event_listener(
                DOMString::from(event_type.clone()),
                DOMString::from(handler_id.clone()),
            );
            println!(
                "[Servo Wasm Host]: Registered native Wasm listener on EventTarget for '{}' with handler_id '{}'",
                event_type, handler_id
            );
        }
    }

    fn remove_event_listener(
        &mut self,
        self_rep: Resource<WitElement>,
        event_type: String,
        handler_id: String,
    ) {
        let id: usize = self_rep.rep() as usize;
        if let Some(Some(node)) = self.elements.get(id) {
            let target: &EventTarget = node.upcast::<EventTarget>();
            target.remove_wasm_event_listener(
                DOMString::from(event_type.clone()),
                &handler_id,
            );
            println!(
                "[Servo Wasm Host]: Removed native Wasm listener on EventTarget for '{}' with handler_id '{}'",
                event_type, handler_id
            );
        }
    }

    fn drop(&mut self, rep: Resource<WitElement>) -> wasmtime::Result<()> {
        let id = rep.rep() as usize;
        if id < self.elements.len() {
            self.elements[id] = None;
        }
        Ok(())
    }

    fn set_attribute(&mut self, self_rep: Resource<WitElement>, name: String, value: String) {
        let id = self_rep.rep() as usize;
        if let Some(Some(node)) = self.elements.get(id) {
            if let Some(element) = node.downcast::<Element>() {
                let cx = unsafe { &mut *self.cx };
                let val = TrustedHTMLOrTrustedScriptOrTrustedScriptURLOrString::String(
                    DOMString::from(value),
                );
                let _ = element.SetAttribute(cx, DOMString::from(name), val);
            }
        }
    }

    fn get_attribute(&mut self, self_rep: Resource<WitElement>, name: String) -> Option<String> {
        let id = self_rep.rep() as usize;
        let cx = unsafe { &mut *self.cx };
        if let Some(Some(node)) = self.elements.get(id) {
            if let Some(element) = node.downcast::<Element>() {
                element
                    .GetAttribute(cx, DOMString::from(name))
                    .map(|s| s.to_string())
            } else {
                None
            }
        } else {
            None
        }
    }

    fn get_property(&mut self, self_rep: Resource<WitElement>, name: String) -> String {
        let id = self_rep.rep() as usize;
        if let Some(Some(node)) = self.elements.get(id) {
            if name == "value" {
                if let Some(input) = node.downcast::<HTMLInputElement>() {
                    return input.Value().to_string();
                } else if let Some(textarea) = node.downcast::<HTMLTextAreaElement>() {
                    return textarea.Value().to_string();
                }
            }
        }
        String::new()
    }

    fn set_property(&mut self, self_rep: Resource<WitElement>, name: String, value: String) {
        let id = self_rep.rep() as usize;
        if let Some(Some(node)) = self.elements.get(id) {
            let cx = unsafe { &mut *self.cx };
            if name == "value" {
                if let Some(input) = node.downcast::<HTMLInputElement>() {
                    input.SetValue(cx, DOMString::from(value));
                } else if let Some(textarea) = node.downcast::<HTMLTextAreaElement>() {
                    textarea.SetValue(cx, DOMString::from(value));
                }
            }
        }
    }

    fn append_child(&mut self, self_rep: Resource<WitElement>, child_rep: Resource<WitElement>) {
        let parent_id = self_rep.rep() as usize;
        let child_id = child_rep.rep() as usize;

        let parent = self
            .elements
            .get(parent_id)
            .and_then(|e| e.as_ref())
            .cloned();
        let child = self
            .elements
            .get(child_id)
            .and_then(|e| e.as_ref())
            .cloned();

        if let (Some(parent), Some(child)) = (parent, child) {
            let cx = unsafe { &mut *self.cx };
            let parent_node = parent.upcast::<Node>();
            let child_node = child.upcast::<Node>();
            let _ = parent_node.AppendChild(cx, child_node);
        }
    }

    fn insert_before(
        &mut self,
        self_rep: Resource<WitElement>,
        child_rep: Resource<WitElement>,
        ref_child_rep: Resource<WitElement>,
    ) {
        let parent_id = self_rep.rep() as usize;
        let child_id = child_rep.rep() as usize;
        let ref_id = ref_child_rep.rep() as usize;

        let parent = self
            .elements
            .get(parent_id)
            .and_then(|e| e.as_ref())
            .cloned();
        let child = self
            .elements
            .get(child_id)
            .and_then(|e| e.as_ref())
            .cloned();
        let ref_child = self.elements.get(ref_id).and_then(|e| e.as_ref()).cloned();

        if let (Some(parent), Some(child)) = (parent, child) {
            let cx = unsafe { &mut *self.cx };
            let parent_node = parent.upcast::<Node>();
            let child_node = child.upcast::<Node>();
            let ref_node = ref_child.as_ref().map(|r| r.upcast::<Node>());
            let _ = parent_node.InsertBefore(cx, child_node, ref_node);
        }
    }

    fn remove_child(&mut self, self_rep: Resource<WitElement>, child_rep: Resource<WitElement>) {
        let parent_id = self_rep.rep() as usize;
        let child_id = child_rep.rep() as usize;

        let parent = self
            .elements
            .get(parent_id)
            .and_then(|e| e.as_ref())
            .cloned();
        let child = self
            .elements
            .get(child_id)
            .and_then(|e| e.as_ref())
            .cloned();

        if let (Some(parent), Some(child)) = (parent, child) {
            let cx = unsafe { &mut *self.cx };
            let parent_node = parent.upcast::<Node>();
            let child_node = child.upcast::<Node>();
            let _ = parent_node.RemoveChild(cx, child_node);
        }
    }

    fn set_text_content(&mut self, self_rep: Resource<WitElement>, text: String) {
        let id = self_rep.rep() as usize;
        if let Some(Some(element)) = self.elements.get(id) {
            let cx = unsafe { &mut *self.cx };
            let node = element.upcast::<Node>();
            let _ = node.SetTextContent(cx, Some(DOMString::from(text)));
        }
    }

    fn get_text_content(&mut self, self_rep: Resource<WitElement>) -> String {
        let id = self_rep.rep() as usize;
        if let Some(Some(element)) = self.elements.get(id) {
            let node = element.upcast::<Node>();
            node.GetTextContent()
                .map(|s| s.to_string())
                .unwrap_or_default()
        } else {
            String::new()
        }
    }
}

impl DocumentHost for WasmHostState {
    fn create_text_node(&mut self, text: String) -> Result<Resource<WitElement>, String> {
        let window = self.global_scope.as_window();
        let doc = window.Document();
        let cx = unsafe { &mut *self.cx };
        let text_node = doc.CreateTextNode(cx, DOMString::from(text));
        let id = self.elements.len() as u32;
        self.elements.push(Some(DomRoot::upcast(text_node)));
        Ok(Resource::new_own(id))
    }

    fn create_element(&mut self, tag: String) -> Result<Resource<WitElement>, String> {
        let window = self.global_scope.as_window();
        let doc = window.Document();
        let dom_tag = DOMString::from(tag);
        let options = StringOrElementCreationOptions::String(DOMString::new());
        let cx = unsafe { &mut *self.cx };

        match doc.CreateElement(cx, dom_tag, options) {
            Ok(element) => {
                let id = self.elements.len() as u32;
                self.elements.push(Some(DomRoot::upcast(element)));
                Ok(Resource::new_own(id))
            },
            Err(_) => Err("Failed to create DOM element".to_string()),
        }
    }

    fn get_body(&mut self) -> Result<Resource<WitElement>, String> {
        let window = self.global_scope.as_window();
        let doc = window.Document();
        match doc.GetBody() {
            Some(body_element) => {
                let element = body_element.upcast::<Element>();
                let root = DomRoot::from_ref(element);
                let id = self.elements.len() as u32;
                self.elements.push(Some(DomRoot::upcast(root)));
                Ok(Resource::new_own(id))
            },
            None => Err("Document has no body element".to_string()),
        }
    }

    fn get_element_by_id(&mut self, id: String) -> Result<Resource<WitElement>, String> {
        let window = self.global_scope.as_window();
        let doc = window.Document();
        let dom_id = DOMString::from(id);
        let cx = unsafe { &mut *self.cx };

        match doc.GetElementById(cx, dom_id) {
            Some(element) => {
                let id = self.elements.len() as u32;
                self.elements.push(Some(DomRoot::upcast(element)));
                Ok(Resource::new_own(id))
            },
            None => Err("Element not found".to_string()),
        }
    }

    fn query_selector(&mut self, selector: String) -> Option<Resource<WitElement>> {
        let window = self.global_scope.as_window();
        let doc = window.Document();
        let cx = unsafe { &mut *self.cx };

        if let Ok(Some(elem)) = doc.QuerySelector(cx, DOMString::from(selector)) {
            let id = self.elements.len() as u32;
            self.elements.push(Some(DomRoot::upcast(elem)));
            Some(Resource::new_own(id))
        } else {
            None
        }
    }
}
