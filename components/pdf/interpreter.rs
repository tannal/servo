/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use std::collections::HashMap;

use kurbo::{Affine, BezPath, Cap, Join, Point, Rect, Shape, Stroke};
use lopdf::Object;
use lopdf::content::{Content, Operation};
use peniko::{Color, Fill, FontData};
use read_fonts::{FontRef as ReadFontRef, TableProvider};
use vello_cpu::{Glyph, RenderContext, Resources};

#[derive(Clone)]
pub struct LoadedFont {
    pub font_data: FontData,
    pub default_width: f64,
}

#[derive(Clone, Debug)]
pub struct GraphicsState {
    pub ctm: Affine,
    pub stroke_color: Color,
    pub fill_color: Color,
    pub line_width: f64,
    pub line_cap: Cap,
    pub line_join: Join,
    pub miter_limit: f64,

    pub in_text_object: bool,
    pub text_matrix: Affine,
    pub text_line_matrix: Affine,
    pub font_name: Vec<u8>,
    pub font_size: f64,
    pub char_spacing: f64,
    pub word_spacing: f64,
    pub horizontal_scaling: f64,
    pub text_leading: f64,
}

impl Default for GraphicsState {
    fn default() -> Self {
        Self {
            ctm: Affine::IDENTITY,
            stroke_color: Color::BLACK,
            fill_color: Color::BLACK,
            line_width: 1.0,
            line_cap: Cap::Butt,
            line_join: Join::Miter,
            miter_limit: 10.0,
            in_text_object: false,
            text_matrix: Affine::IDENTITY,
            text_line_matrix: Affine::IDENTITY,
            font_name: Vec::new(),
            font_size: 12.0,
            char_spacing: 0.0,
            word_spacing: 0.0,
            horizontal_scaling: 100.0,
            text_leading: 0.0,
        }
    }
}

pub struct ContentStreamInterpreter<'a> {
    ctx: &'a mut RenderContext,
    resources: &'a mut Resources,
    fonts: &'a HashMap<Vec<u8>, LoadedFont>,
    state_stack: Vec<GraphicsState>,
    current_state: GraphicsState,
    current_path: BezPath,
}

impl<'a> ContentStreamInterpreter<'a> {
    pub fn new(
        ctx: &'a mut RenderContext,
        resources: &'a mut Resources,
        fonts: &'a HashMap<Vec<u8>, LoadedFont>,
        base_transform: Affine,
    ) -> Self {
        let initial_state = GraphicsState {
            ctm: base_transform,
            ..Default::default()
        };

        Self {
            ctx,
            resources,
            fonts,
            state_stack: Vec::new(),
            current_state: initial_state,
            current_path: BezPath::new(),
        }
    }

    pub fn current_state(&self) -> &GraphicsState {
        &self.current_state
    }

    pub fn evaluate(&mut self, content: &Content) {
        for op in &content.operations {
            self.execute_operation(op);
        }
    }

    pub fn execute_operation(&mut self, op: &Operation) {
        let name = op.operator.as_str();
        let args = &op.operands;

        match name {
            "BT" => {
                self.current_state.in_text_object = true;
                self.current_state.text_matrix = Affine::IDENTITY;
                self.current_state.text_line_matrix = Affine::IDENTITY;
            },
            "ET" => {
                self.current_state.in_text_object = false;
            },
            "Tf" => {
                if args.len() >= 2 {
                    if let Ok(name) = args[0].as_name() {
                        self.current_state.font_name = name.to_vec();
                    }
                    self.current_state.font_size = to_f64(&args[1]);
                }
            },
            "TL" => {
                if let Some(leading) = args.first() {
                    self.current_state.text_leading = to_f64(leading);
                }
            },
            "Tm" => {
                if args.len() >= 6 {
                    let m = Affine::new([
                        to_f64(&args[0]),
                        to_f64(&args[1]),
                        to_f64(&args[2]),
                        to_f64(&args[3]),
                        to_f64(&args[4]),
                        to_f64(&args[5]),
                    ]);
                    self.current_state.text_matrix = m;
                    self.current_state.text_line_matrix = m;
                }
            },
            "Td" => {
                if args.len() >= 2 {
                    let tx = to_f64(&args[0]);
                    let ty = to_f64(&args[1]);
                    let translation = Affine::translate((tx, ty));
                    self.current_state.text_line_matrix =
                        self.current_state.text_line_matrix * translation;
                    self.current_state.text_matrix = self.current_state.text_line_matrix;
                }
            },
            "T*" => {
                let dy = -self.current_state.text_leading;
                let translation = Affine::translate((0.0, dy));
                self.current_state.text_line_matrix =
                    self.current_state.text_line_matrix * translation;
                self.current_state.text_matrix = self.current_state.text_line_matrix;
            },
            "Tj" => {
                if let Some(Object::String(bytes, _)) = args.first() {
                    self.render_text_string(bytes);
                }
            },
            "TJ" => {
                if let Some(Object::Array(elements)) = args.first() {
                    for item in elements {
                        match item {
                            Object::String(bytes, _) => self.render_text_string(bytes),
                            Object::Real(adj) => self.apply_kerning_adjustment((*adj) as f64),
                            Object::Integer(adj) => self.apply_kerning_adjustment(*adj as f64),
                            _ => {},
                        }
                    }
                }
            },
            "q" => self.state_stack.push(self.current_state.clone()),
            "Q" => {
                if let Some(s) = self.state_stack.pop() {
                    self.current_state = s;
                }
            },
            "cm" => {
                if args.len() >= 6 {
                    let m = Affine::new([
                        to_f64(&args[0]),
                        to_f64(&args[1]),
                        to_f64(&args[2]),
                        to_f64(&args[3]),
                        to_f64(&args[4]),
                        to_f64(&args[5]),
                    ]);
                    self.current_state.ctm = self.current_state.ctm * m;
                }
            },
            "w" => {
                if let Some(w) = args.first() {
                    self.current_state.line_width = to_f64(w);
                }
            },
            "m" => {
                if args.len() >= 2 {
                    let pt =
                        self.current_state.ctm * Point::new(to_f64(&args[0]), to_f64(&args[1]));
                    self.current_path.move_to(pt);
                }
            },
            "l" => {
                if args.len() >= 2 {
                    let pt =
                        self.current_state.ctm * Point::new(to_f64(&args[0]), to_f64(&args[1]));
                    self.current_path.line_to(pt);
                }
            },
            "c" => {
                if args.len() >= 6 {
                    let p1 =
                        self.current_state.ctm * Point::new(to_f64(&args[0]), to_f64(&args[1]));
                    let p2 =
                        self.current_state.ctm * Point::new(to_f64(&args[2]), to_f64(&args[3]));
                    let p3 =
                        self.current_state.ctm * Point::new(to_f64(&args[4]), to_f64(&args[5]));
                    self.current_path.curve_to(p1, p2, p3);
                }
            },
            "re" => {
                if args.len() >= 4 {
                    let x = to_f64(&args[0]);
                    let y = to_f64(&args[1]);
                    let w = to_f64(&args[2]);
                    let h = to_f64(&args[3]);
                    let rect = Rect::new(x, y, x + w, y + h);
                    let mut bez = rect.to_path(0.1);
                    bez.apply_affine(self.current_state.ctm);
                    self.current_path.extend(bez);
                }
            },
            "rg" => {
                if args.len() >= 3 {
                    let r = (to_f64(&args[0]).clamp(0.0, 1.0) * 255.0) as u8;
                    let g = (to_f64(&args[1]).clamp(0.0, 1.0) * 255.0) as u8;
                    let b = (to_f64(&args[2]).clamp(0.0, 1.0) * 255.0) as u8;
                    self.current_state.fill_color = Color::from_rgb8(r, g, b);
                }
            },
            "S" => {
                self.paint_stroke();
                self.current_path.truncate(0);
            },
            "f" | "F" => {
                self.paint_fill(Fill::NonZero);
                self.current_path.truncate(0);
            },
            "f*" => {
                self.paint_fill(Fill::EvenOdd);
                self.current_path.truncate(0);
            },
            _ => {},
        }
    }

    fn render_text_string(&mut self, text_bytes: &[u8]) {
        let font = self
            .fonts
            .get(&self.current_state.font_name)
            .or_else(|| self.fonts.values().next());

        let Some(font) = font else {
            self.advance_text_matrix(text_bytes.len() as f64 * 0.6 * self.current_state.font_size);
            return;
        };

        // Read the font's CMAP table using read-fonts
        let font_bytes: &[u8] = font.font_data.data.as_ref();
        let charmap = ReadFontRef::from_index(font_bytes, 0)
            .ok()
            .and_then(|rf| rf.cmap().ok());

        let font_size = self.current_state.font_size;
        let scale_h = self.current_state.horizontal_scaling / 100.0;
        let text_render_matrix = self.current_state.ctm * self.current_state.text_matrix;

        let mut glyphs = Vec::with_capacity(text_bytes.len());
        let mut x_offset = 0.0;

        for &byte in text_bytes {
            let ch = byte as char;

            // Map character -> real GlyphId!
            let glyph_id = charmap
                .as_ref()
                .and_then(|cm| cm.map_codepoint(ch as u32))
                .map(|g| g.to_u32())
                .unwrap_or(byte as u32);

            glyphs.push(Glyph {
                id: glyph_id,
                x: x_offset as f32,
                y: 0.0,
            });

            let glyph_width = (font.default_width / 1000.0) * font_size * scale_h;
            let spacing = self.current_state.char_spacing
                + if byte == b' ' {
                    self.current_state.word_spacing
                } else {
                    0.0
                };
            x_offset += glyph_width + spacing;
        }

        self.ctx.set_transform(text_render_matrix);
        self.ctx.set_paint(self.current_state.fill_color);

        self.ctx
            .glyph_run(self.resources, &font.font_data)
            .font_size(font_size as f32)
            .fill_glyphs(glyphs.into_iter());

        self.advance_text_matrix(x_offset);
    }

    pub fn apply_kerning_adjustment(&mut self, adjustment: f64) {
        let font_size = self.current_state.font_size;
        let scale_h = self.current_state.horizontal_scaling / 100.0;
        let delta_x = (-adjustment / 1000.0) * font_size * scale_h;
        self.advance_text_matrix(delta_x);
    }

    fn advance_text_matrix(&mut self, dx: f64) {
        let advance = Affine::translate((dx, 0.0));
        self.current_state.text_matrix = self.current_state.text_matrix * advance;
    }

    fn paint_fill(&mut self, rule: Fill) {
        self.ctx.set_transform(Affine::IDENTITY);
        self.ctx.set_paint(self.current_state.fill_color);
        self.ctx.set_fill_rule(rule);
        self.ctx.fill_path(&self.current_path);
    }

    fn paint_stroke(&mut self) {
        self.ctx.set_transform(Affine::IDENTITY);
        self.ctx.set_paint(self.current_state.stroke_color);
        let stroke = Stroke {
            width: self.current_state.line_width,
            join: self.current_state.line_join,
            miter_limit: self.current_state.miter_limit,
            start_cap: self.current_state.line_cap,
            end_cap: self.current_state.line_cap,
            dash_pattern: Default::default(),
            dash_offset: 0.0,
        };
        self.ctx.set_stroke(stroke);
        self.ctx.stroke_path(&self.current_path);
    }
}

pub fn to_f64(obj: &Object) -> f64 {
    match obj {
        Object::Real(val) => (*val) as f64,
        Object::Integer(val) => *val as f64,
        _ => 0.0,
    }
}
