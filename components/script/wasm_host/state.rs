/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

 use js::context::JSContext;
 use crate::dom::bindings::root::DomRoot;
 use crate::dom::canvasrenderingcontext2d::CanvasRenderingContext2D;
 use crate::dom::globalscope::GlobalScope;
 use crate::dom::node::Node;
 
 /// The host state held inside the Wasmtime Store for each component invocation
 pub struct WasmHostState {
     pub cx: *mut JSContext,
     pub global_scope: DomRoot<GlobalScope>,
     pub elements: Vec<Option<DomRoot<Node>>>,
     pub contexts: Vec<Option<DomRoot<CanvasRenderingContext2D>>>,
 }
 
 impl WasmHostState {
     pub fn new(cx: &mut JSContext, global_scope: DomRoot<GlobalScope>) -> Self {
         Self {
             cx: cx as *mut JSContext,
             global_scope,
             elements: Vec::new(),
             contexts: Vec::new(),
         }
     }
 }