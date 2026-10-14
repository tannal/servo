/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

 use std::cell::RefCell;
 use std::fmt::Write;
 use std::sync::{Arc, LazyLock, RwLock};
 
 use crate::{COMPONENT_REGISTRY, SERVO_WASM_ENGINE};
 use log::{error, info, warn};
 use wasmtime::component::{Component, Linker};
 use wasmtime::Store;
 
 wasmtime::component::bindgen!({
     world: "pdf-engine",
     path: "wit/pdf.wit",
 });
 
 
 static PDF_LINKER: LazyLock<Linker<()>> = LazyLock::new(|| {
     let mut linker = Linker::new(&SERVO_WASM_ENGINE);
     linker
 });
 
 static USER_PDF_COMPONENT: RwLock<Option<Arc<Component>>> = RwLock::new(None);
 
 thread_local! {
     static ACTIVE_PDF_ENGINE: RefCell<Option<(Store<()>, PdfEngine)>> = const { RefCell::new(None) };
 }
 
 const VIEWER_BASE_CSS: &str = r#"
 * { margin: 0; padding: 0; box-sizing: border-box; }
 body {
     background: #525659;
     display: flex;
     flex-direction: column;
     align-items: center;
     gap: 20px;
     padding: 30px 0;
     font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, "Liberation Sans", "DejaVu Sans", Arial, sans-serif;
     -webkit-font-smoothing: antialiased;
     user-select: text;
 }
 .pdf-page {
     position: relative;
     background: #ffffff;
     box-shadow: 0 4px 12px rgba(0, 0, 0, 0.25);
     overflow: hidden;
 }
 .pdf-node {
     position: absolute;
     white-space: pre;
     line-height: 1;
 }
 "#;
 
 fn get_or_load_component() -> Option<Arc<Component>> {
     if let Some(comp) = USER_PDF_COMPONENT.read().unwrap().as_ref() {
         return Some(comp.clone());
     }
 
     let candidate_paths = [
         "../servo-pdf/target/wasm32-unknown-unknown/release/servo_pdf.wasm",
         "target/wasm32-unknown-unknown/release/servo_pdf.wasm",
         "components/pdf/target/wasm32-unknown-unknown/release/servo_pdf.wasm",
         "servo_pdf.wasm",
         "resources/servo_pdf.wasm",
     ];
 
     for path in candidate_paths {
         if let Ok(bytes) = std::fs::read(path) {
             info!("[PDF Host]: Found '{}' ({} bytes), compiling...", path, bytes.len());
             match COMPONENT_REGISTRY.get_or_load_binary(path, &bytes) {
                 Ok(comp) => return Some(comp),
                 Err(err) => error!("[PDF Host ERROR]: Failed to compile '{path}': {err:?}"),
             }
         }
     }
 
     error!("[PDF Host ERROR]: Could not find servo_pdf.wasm in candidate paths");
     None
 }
 
 /// Renders a PDF document into an HTML string.
 pub fn render_pdf_to_html(data: &[u8]) -> Result<String, String> {
     ACTIVE_PDF_ENGINE.with(|cell| {
         let mut borrow = cell.borrow_mut();
 
         if borrow.is_none() {
             let Some(component) = get_or_load_component() else {
                 return Err("servo_pdf.wasm not found or invalid component".to_string());
             };
 
             let mut store = Store::new(&*SERVO_WASM_ENGINE, ());
             let pdf_engine = PdfEngine::instantiate(&mut store, &component, &*PDF_LINKER)
                 .map_err(|e| format!("Failed to instantiate PdfEngine: {e}"))?;
 
             *borrow = Some((store, pdf_engine));
         }
 
         let (store, engine) = borrow.as_mut().unwrap();
         let viewer = engine.servo_pdf_viewer();
 
         // 1. Open document (< 5ms)
         let doc = viewer
             .call_open(&mut *store, data)
             .map_err(|e| format!("Wasm trap on Document::open: {e}"))?
             .map_err(|err| format!("PDF parse error: {err}"))?;
 
         // 2. Query page count
         let page_count = viewer
             .document()
             .call_page_count(&mut *store, doc)
             .map_err(|e| format!("Wasm trap on page_count: {e}"))?;
 
         if page_count == 0 {
             let _ = doc.resource_drop(&mut *store);
             return Err("PDF document contains 0 pages".to_string());
         }
 
         let mut pages_html = String::with_capacity(page_count as usize * 2048);
 
         // 3. Strict 0-based indexing: `0..page_count` (exclusive)
         for page_idx in 0..page_count {
             let human_num = page_idx + 1;
 
             let (width, height) = viewer
                 .document()
                 .call_page_dimensions(&mut *store, doc, page_idx)
                 .map_err(|e| format!("Wasm trap on page_dimensions: {e}"))?
                 .map_err(|err| format!("Dimensions error page {human_num}: {err}"))?;
 
             let page_html = viewer
                 .document()
                 .call_render_page_html(&mut *store, doc, page_idx)
                 .map_err(|e| format!("Wasm trap on render_page_html: {e}"))?
                 .map_err(|err| format!("Render error page {human_num}: {err}"))?;
 
             write!(
                 pages_html,
                 r#"<div id="page-{human_num}" class="pdf-page" style="width: {width:.1}pt; height: {height:.1}pt;">{page_html}</div>"#
             )
             .unwrap();
         }
 
         // 4. Drop guest handle exactly once
         if let Err(e) = doc.resource_drop(&mut *store) {
             warn!("[PDF Host]: Failed to drop guest document handle: {e}");
         }
 
         Ok(format!(
             r#"<!DOCTYPE html>
 <html lang="en">
 <head>
     <meta charset="utf-8">
     <meta name="viewport" content="width=device-width, initial-scale=1">
     <title>PDF Document</title>
     <style>{VIEWER_BASE_CSS}</style>
 </head>
 <body>
 {pages_html}
 </body>
 </html>"#
         ))
     })
 }
 