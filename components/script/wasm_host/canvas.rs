/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use js::rust::HandleValue;
use script_bindings::codegen::GenericBindings::CanvasRenderingContext2DBinding::{CanvasLineCap, CanvasRenderingContext2DMethods};
use script_bindings::codegen::GenericUnionTypes::StringOrCanvasGradientOrCanvasPattern;
use wasmtime::component::Resource;

use crate::dom::bindings::codegen::Bindings::CanvasRenderingContext2DBinding::CanvasFillRule;
use crate::dom::bindings::codegen::Bindings::HTMLCanvasElementBinding::{
    HTMLCanvasElementMethods, RenderingContext as RootedRenderingContext,
};
use crate::dom::bindings::inheritance::Castable;
use crate::dom::bindings::str::DOMString;
use crate::dom::html::embedded_content::htmlcanvaselement::HTMLCanvasElement;

use super::state::WasmHostState;
use super::wit_dom::servo::dom::canvas2d::{Context2d, Host as CanvasHost, HostContext2d};
use super::wit_dom::servo::dom::document::Element as WitElement;

impl CanvasHost for WasmHostState {
    fn get_context(
        &mut self,
        canvas_rep: Resource<WitElement>,
    ) -> Result<Resource<Context2d>, String> {
        let id = canvas_rep.rep() as usize;
        let node = self
            .elements
            .get(id)
            .and_then(|n| n.as_ref())
            .ok_or_else(|| "Invalid element reference".to_string())?;

        let canvas = node
            .downcast::<HTMLCanvasElement>()
            .ok_or_else(|| "Element is not an HTMLCanvasElement".to_string())?;

        // Extract active JSContext from the ambient host state
        let cx = unsafe { &mut *self.cx };

        // Invoke WHATWG GetContext with context ID "2d" and null options
        match canvas.GetContext(cx, DOMString::from("2d"), HandleValue::null()) {
            Ok(Some(RootedRenderingContext::CanvasRenderingContext2D(ctx))) => {
                let ctx_id = self.contexts.len() as u32;
                self.contexts.push(Some(ctx));
                Ok(Resource::new_own(ctx_id))
            },
            Ok(_) => Err("Canvas context is not a 2D rendering context".to_string()),
            Err(e) => Err(format!("Failed to initialize 2D context: {:?}", e)),
        }
    }
}

impl HostContext2d for WasmHostState {
    fn clear_rect(&mut self, self_rep: Resource<Context2d>, x: f64, y: f64, w: f64, h: f64) {
        let id = self_rep.rep() as usize;
        if let Some(Some(ctx)) = self.contexts.get(id) {
            ctx.ClearRect(x, y, w, h);
        }
    }

    fn begin_path(&mut self, self_rep: Resource<Context2d>) {
        let id = self_rep.rep() as usize;
        if let Some(Some(ctx)) = self.contexts.get(id) {
            ctx.BeginPath();
        }
    }

    fn close_path(&mut self, self_rep: Resource<Context2d>) {
        let id = self_rep.rep() as usize;
        if let Some(Some(ctx)) = self.contexts.get(id) {
            ctx.ClosePath();
        }
    }

    fn move_to(&mut self, self_rep: Resource<Context2d>, x: f64, y: f64) {
        let id = self_rep.rep() as usize;
        if let Some(Some(ctx)) = self.contexts.get(id) {
            ctx.MoveTo(x, y);
        }
    }

    fn line_to(&mut self, self_rep: Resource<Context2d>, x: f64, y: f64) {
        let id = self_rep.rep() as usize;
        if let Some(Some(ctx)) = self.contexts.get(id) {
            ctx.LineTo(x, y);
        }
    }

    fn quadratic_curve_to(
        &mut self,
        self_rep: Resource<Context2d>,
        cpx: f64,
        cpy: f64,
        x: f64,
        y: f64,
    ) {
        let id = self_rep.rep() as usize;
        if let Some(Some(ctx)) = self.contexts.get(id) {
            ctx.QuadraticCurveTo(cpx, cpy, x, y);
        }
    }

    fn bezier_curve_to(
        &mut self,
        self_rep: Resource<Context2d>,
        cp1x: f64,
        cp1y: f64,
        cp2x: f64,
        cp2y: f64,
        x: f64,
        y: f64,
    ) {
        let id = self_rep.rep() as usize;
        if let Some(Some(ctx)) = self.contexts.get(id) {
            ctx.BezierCurveTo(cp1x, cp1y, cp2x, cp2y, x, y);
        }
    }

    fn arc(
        &mut self,
        self_rep: Resource<Context2d>,
        x: f64,
        y: f64,
        radius: f64,
        start_angle: f64,
        end_angle: f64,
        anticlockwise: bool,
    ) {
        let id = self_rep.rep() as usize;
        if let Some(Some(ctx)) = self.contexts.get(id) {
            let _ = ctx.Arc(x, y, radius, start_angle, end_angle, anticlockwise);
        }
    }

    fn rect(&mut self, self_rep: Resource<Context2d>, x: f64, y: f64, w: f64, h: f64) {
        let id = self_rep.rep() as usize;
        if let Some(Some(ctx)) = self.contexts.get(id) {
            ctx.Rect(x, y, w, h);
        }
    }

    fn fill(&mut self, self_rep: Resource<Context2d>) {
        let id = self_rep.rep() as usize;
        if let Some(Some(ctx)) = self.contexts.get(id) {
            ctx.Fill(CanvasFillRule::Nonzero);
        }
    }

    fn stroke(&mut self, self_rep: Resource<Context2d>) {
        let id = self_rep.rep() as usize;
        if let Some(Some(ctx)) = self.contexts.get(id) {
            ctx.Stroke();
        }
    }

    fn set_stroke_style(&mut self, self_rep: Resource<Context2d>, style: String) {
        let id = self_rep.rep() as usize;
        if let Some(Some(ctx)) = self.contexts.get(id) {
            let dom_str = DOMString::from(style);
            let cx = unsafe { &mut *self.cx };
            let val = StringOrCanvasGradientOrCanvasPattern::String(dom_str);
            let _ = ctx.SetStrokeStyle(val);
        }
    }

    fn set_fill_style(&mut self, self_rep: Resource<Context2d>, style: String) {
        let id = self_rep.rep() as usize;
        if let Some(Some(ctx)) = self.contexts.get(id) {
            let dom_str = DOMString::from(style);
            let cx = unsafe { &mut *self.cx };
            let val = StringOrCanvasGradientOrCanvasPattern::String(dom_str);
            let _ = ctx.SetFillStyle(val);
        }
    }

    fn set_line_width(&mut self, self_rep: Resource<Context2d>, width: f64) {
        let id = self_rep.rep() as usize;
        if let Some(Some(ctx)) = self.contexts.get(id) {
            ctx.SetLineWidth(width);
        }
    }

    fn set_line_dash(&mut self, self_rep: Resource<Context2d>, segments: Vec<f64>) {
        let id = self_rep.rep() as usize;
        let cx = unsafe { &mut *self.cx };
        if let Some(Some(ctx)) = self.contexts.get(id) {
            let _ = ctx.SetLineDash(segments);
        }
    }

    fn set_line_cap(&mut self, self_rep: Resource<Context2d>, cap: String) {
        let id = self_rep.rep() as usize;
        if let Some(Some(ctx)) = self.contexts.get(id) {
            let line_cap = match cap.as_str() {
                "round" => CanvasLineCap::Round,
                "square" => CanvasLineCap::Square,
                _ => CanvasLineCap::Butt,
            };
            ctx.SetLineCap(line_cap);
        }
    }

    fn drop(&mut self, rep: Resource<Context2d>) -> wasmtime::Result<()> {
        let id = rep.rep() as usize;
        if id < self.contexts.len() {
            self.contexts[id] = None;
        }
        Ok(())
    }
}
