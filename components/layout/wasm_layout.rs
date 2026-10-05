/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use crate::geom::{PhysicalPoint, PhysicalRect, PhysicalSize};
use app_units::Au;
use servo_wasm::{COMPONENT_REGISTRY, SERVO_WASM_ENGINE};
use std::cell::RefCell;
use std::sync::LazyLock;
use wasmtime::Store;
use wasmtime::component::Linker;

wasmtime::component::bindgen!({
    world: "layout-worklet",
    path: "wit/servo_layout.wit",
});

use exports::servo::layout::engine as wit_engine;

static LAYOUT_LINKER: LazyLock<Linker<()>> = LazyLock::new(|| {
    let engine = &*SERVO_WASM_ENGINE;
    Linker::new(engine)
});

pub struct CustomBoxMetrics {
    pub width: Au,
    pub height: Au,
    pub ascent: Au,
    pub descent: Au,
}

pub struct CustomLayoutResult {
    pub width: Au,
    pub height: Au,
    pub ascent: Au,
    pub descent: Au,
    pub children_offsets: Vec<(Au, Au)>,
    pub extra_rects: Vec<PhysicalRect<Au>>,
}

// Thread-local cache: Instantiate once per thread, reuse forever
thread_local! {
    static THREAD_WORKLET: RefCell<Option<(Store<()>, LayoutWorklet)>> = const { RefCell::new(None) };
}

pub fn layout_custom_element(
    tag: &str,
    children: &[CustomBoxMetrics],
    font_size: Au,
    math_axis_height: Au,
    line_thickness: Au,
    custom_thickness: Option<Au>,
) -> Option<CustomLayoutResult> {
    THREAD_WORKLET.with(|cell| {
        let mut worklet_opt = cell.borrow_mut();
        if worklet_opt.is_none() {
            let engine = &*SERVO_WASM_ENGINE;
            let linker = &*LAYOUT_LINKER;
            let component = match COMPONENT_REGISTRY.get_or_load_file("mathml_core.wasm") {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("[Wasm Layout Error]: Failed to get/compile mathml_core.wasm: {e}");
                    return None;
                },
            };
            let mut store = Store::new(engine, ());
            match LayoutWorklet::instantiate(&mut store, &component, linker) {
                Ok(inst) => *worklet_opt = Some((store, inst)),
                Err(e) => {
                    eprintln!("[Wasm Layout Error]: Failed to instantiate component: {e}");
                    return None;
                },
            }
        }

        let (store, instance) = worklet_opt.as_mut().unwrap();

        let input = wit_engine::LayoutInput {
            element_tag: tag.to_string(),
            children: children
                .iter()
                .map(|c| wit_engine::BoxMetrics {
                    width: c.width.0 as u32,
                    height: c.height.0 as u32,
                    ascent: c.ascent.0 as u32,
                    descent: c.descent.0 as u32,
                })
                .collect(),
            font: wit_engine::FontMetrics {
                font_size: font_size.0 as u32,
                math_axis_height: math_axis_height.0 as u32,
                default_line_thickness: line_thickness.0 as u32,
            },
            custom_thickness: custom_thickness.map(|t| t.0 as u32),
        };

        let out = match instance
            .servo_layout_engine()
            .call_layout_element(store, &input)
        {
            Ok(out) => out,
            Err(e) => {
                eprintln!("[Wasm Layout Error]: call_layout_element trapped: {e}");
                return None;
            },
        };

        let children_offsets = out
            .children_offsets
            .into_iter()
            .map(|o| (Au(o.x as i32), Au(o.y as i32)))
            .collect();

        let extra_rects = out
            .extra_rects
            .into_iter()
            .map(|r| {
                PhysicalRect::new(
                    PhysicalPoint::new(Au(r.x as i32), Au(r.y as i32)),
                    PhysicalSize::new(Au(r.width as i32), Au(r.height as i32)),
                )
            })
            .collect();

        Some(CustomLayoutResult {
            width: Au(out.width as i32),
            height: Au(out.height as i32),
            ascent: Au(out.ascent as i32),
            descent: Au(out.descent as i32),
            children_offsets,
            extra_rects,
        })
    })
}
