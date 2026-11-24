/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Native `parley.wasm` text editor host integration.
//!
//! Instantiates WebAssembly components exporting `servo:parley-editor/editor@0.1.0`,
//! bridges Servo's `FontContext` (`servo:fonts`) and Canvas 2D / Vello backend
//! (`servo:surface`), and routes keyboard, pointer, IME composition, and
//! clipboard events from a `<canvas id="parley-editor">` element into Parley.

#![allow(unsafe_code)]

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::{Arc, LazyLock};

use embedder_traits::EmbedderMsg;
use euclid::default::{Point2D, Rect as EuclidRect, Size2D, Transform2D};
use fonts::{FontDataAndIndex, FontIdentifier};
use js::context::JSContext;
use script_bindings::codegen::GenericUnionTypes::{
    StringOrElementCreationOptions, TrustedHTMLOrTrustedScriptOrTrustedScriptURLOrString,
};
use script_bindings::realms::enter_auto_realm;
use servo_base::generic_channel::GenericCallback;
use servo_canvas_traits::canvas::{
    CanvasCommand, CanvasFont, CompositionOptions, FillOrStrokeStyle, GlyphAndPosition,
    ShadowOptions, TextRun,
};
use servo_wasm::SERVO_WASM_ENGINE;
use style::color::AbsoluteColor;
use wasmtime::Store;
use wasmtime::component::{Component, HasSelf, Linker};

use crate::canvas_context::CanvasContext;
use crate::dom::bindings::codegen::Bindings::CompositionEventBinding::CompositionEventMethods;
use crate::dom::bindings::codegen::Bindings::DocumentBinding::DocumentMethods;
use crate::dom::bindings::codegen::Bindings::ElementBinding::ElementMethods;
use crate::dom::bindings::codegen::Bindings::EventBinding::EventMethods;
use crate::dom::bindings::codegen::Bindings::HTMLCanvasElementBinding::HTMLCanvasElementMethods;
use crate::dom::bindings::codegen::Bindings::HTMLElementBinding::HTMLElementMethods;
use crate::dom::bindings::codegen::Bindings::KeyboardEventBinding::KeyboardEventMethods;
use crate::dom::bindings::codegen::Bindings::MouseEventBinding::MouseEventMethods;
use crate::dom::bindings::codegen::Bindings::NodeBinding::NodeMethods;
use crate::dom::bindings::codegen::Bindings::UIEventBinding::UIEventMethods;
use crate::dom::bindings::codegen::Bindings::WindowBinding::WindowMethods;
use crate::dom::bindings::codegen::DomTypeHolder::DomTypeHolder;
use crate::dom::bindings::inheritance::Castable;
use crate::dom::bindings::root::DomRoot;
use crate::dom::bindings::str::DOMString;
use crate::dom::compositionevent::CompositionEvent;
use crate::dom::event::Event;
use crate::dom::eventtarget::EventTarget;
use crate::dom::globalscope::GlobalScope;
use crate::dom::html::htmlcanvaselement::HTMLCanvasElement;
use crate::dom::html::htmlelement::HTMLElement;
use crate::dom::keyboardevent::KeyboardEvent;
use crate::dom::mouseevent::MouseEvent;
use crate::dom::node::Node;

pub mod wit_parley {
    wasmtime::component::bindgen!({
        world: "parley-editor",
        path: "wit/parley_editor.wit",
    });
}

use wit_parley::ParleyEditor;
use wit_parley::servo::parley_editor::console::Host as ConsoleHost;
use wit_parley::servo::parley_editor::editing::EditorEvent;
use wit_parley::servo::parley_editor::fonts::{FontQuery, Host as FontsHost, ResolvedFont};
use wit_parley::servo::parley_editor::surface::{Host as SurfaceHost, PaintFrame, Rgba};

const DEFAULT_WIDTH: u32 = 960;
const DEFAULT_HEIGHT: u32 = 640;

const INITIAL_TEXT: &str = "Parley.wasm Text Editor in Servo\n\n\
    Shaping, line breaking, bidi, selection, and hit-testing run in parley.wasm \
    (wasm32-unknown-unknown Component Model).\n\
    Fonts come from Servo's FontContext and glyphs render directly through \
    Servo's Vello Canvas 2D backend.\n\n\
    Click anywhere and start typing, select with mouse or Shift+Arrows, \
    or copy/cut/paste with Ctrl/Cmd+C/X/V.";

struct CachedFont {
    identifier: FontIdentifier,
    data: Option<FontDataAndIndex>,
}

struct ParleyHostState {
    cx: *mut JSContext,
    global_scope: DomRoot<GlobalScope>,
    canvas: DomRoot<HTMLCanvasElement>,
    next_font_id: u64,
    fonts: HashMap<u64, CachedFont>,
    font_ids: HashMap<FontIdentifier, u64>,
}

impl ParleyHostState {
    fn cx(&self) -> &mut JSContext {
        unsafe { &mut *self.cx }
    }
}

impl wit_parley::servo::parley_editor::editing::Host for ParleyHostState {}

impl ConsoleHost for ParleyHostState {
    fn log(&mut self, message: String) {
        println!("[Servo Parley]: {message}");
    }

    fn error(&mut self, message: String) {
        eprintln!("[Servo Parley Error]: {message}");
    }
}

impl FontsHost for ParleyHostState {
    fn resolve(&mut self, query: FontQuery) -> Option<ResolvedFont> {
        let cx = self.cx();
        let window = self.global_scope.as_window();
        let node = self.canvas.upcast::<Node>();

        let style = if query.italic { "italic" } else { "normal" };
        let weight = query.weight.round() as u32;
        let families = if query.families.is_empty() {
            "sans-serif".to_string()
        } else {
            query
                .families
                .iter()
                .map(|family| {
                    if family.contains(' ') && !family.starts_with('"') {
                        format!("\"{family}\"")
                    } else {
                        family.clone()
                    }
                })
                .collect::<Vec<_>>()
                .join(", ")
        };
        let css_font = format!("{style} {weight} 16px {families}");

        let font_style = window.resolved_font_style_query(cx, node, css_font)?;
        let font_context = self.global_scope.font_context();
        let font_group = font_context.font_group(font_style);
        let font = font_group.first(&font_context)?;

        let identifier = font.identifier().to_owned();
        let font_data_and_index = font.font_data_and_index().ok().cloned();
        let face_index = font_data_and_index.as_ref().map_or(0, |d| d.index);

        if let Some(&existing_id) = self.font_ids.get(&identifier) {
            return Some(ResolvedFont {
                font_id: existing_id,
                face_index,
            });
        }

        let font_id = self.next_font_id;
        self.next_font_id += 1;
        self.font_ids.insert(identifier.clone(), font_id);
        self.fonts.insert(
            font_id,
            CachedFont {
                identifier,
                data: font_data_and_index,
            },
        );

        Some(ResolvedFont {
            font_id,
            face_index,
        })
    }

    fn load_font_bytes(&mut self, font_id: u64) -> Option<Vec<u8>> {
        let cached = self.fonts.get(&font_id)?;
        if let Some(ref data) = cached.data {
            return Some(data.data.as_ref().to_vec());
        }
        if let FontIdentifier::Local(ref local) = cached.identifier {
            return local
                .font_data_and_index()
                .map(|d| d.data.as_ref().to_vec());
        }
        None
    }
}

fn rgba_to_color(c: Rgba) -> AbsoluteColor {
    AbsoluteColor::srgb_legacy(c.r, c.g, c.b, c.a as f32 / 255.0)
}

impl SurfaceHost for ParleyHostState {
    fn present(&mut self, width: u32, height: u32, frame: PaintFrame) {
        let cx = self.cx();
        let Some(context) = self.canvas.get_or_init_default_2d_context(cx) else {
            return;
        };

        let mut commands = Vec::with_capacity(1 + frame.rects.len() + frame.glyph_runs.len());
        let identity = Transform2D::identity();
        let shadow = ShadowOptions::default();
        let composition = CompositionOptions::default();

        // 1. Background fill
        commands.push(CanvasCommand::FillRect(
            EuclidRect::new(
                Point2D::new(0.0, 0.0),
                Size2D::new(width as f32, height as f32),
            ),
            FillOrStrokeStyle::Color(rgba_to_color(frame.background)),
            shadow,
            composition,
            identity,
        ));

        // 2. Selection highlights, IME underlines, and caret
        for (rect, color) in frame.rects {
            commands.push(CanvasCommand::FillRect(
                EuclidRect::new(Point2D::new(rect.x, rect.y), Size2D::new(rect.w, rect.h)),
                FillOrStrokeStyle::Color(rgba_to_color(color)),
                shadow,
                composition,
                identity,
            ));
        }

        // 3. Positioned glyph runs directly into Servo's Vello/Canvas 2D backend
        let bounds = EuclidRect::new(
            Point2D::new(0.0, 0.0),
            Size2D::new(width as f64, height as f64),
        );
        for run in frame.glyph_runs {
            let Some(cached) = self.fonts.get(&run.font_id) else {
                continue;
            };
            let canvas_font = CanvasFont {
                identifier: cached.identifier.clone(),
                data: match &cached.identifier {
                    FontIdentifier::Local(_) => None,
                    FontIdentifier::Web(_) | FontIdentifier::ArrayBuffer(_) => cached.data.clone(),
                },
            };
            let glyphs_and_positions = run
                .glyphs
                .into_iter()
                .map(|g| GlyphAndPosition {
                    id: g.id,
                    point: Point2D::new(g.x, g.y),
                })
                .collect();
            let text_run = TextRun {
                font: canvas_font,
                pt_size: run.font_size,
                glyphs_and_positions,
                advance: 0.0,
                bounds,
            };
            commands.push(CanvasCommand::FillText(
                bounds,
                vec![text_run],
                FillOrStrokeStyle::Color(rgba_to_color(run.color)),
                shadow,
                composition,
                identity,
            ));
        }

        context.send_canvas_command_immediate(CanvasCommand::ProcessBatchMessages(commands));
        context.mark_as_dirty();
    }
}

static PARLEY_LINKER: LazyLock<Linker<ParleyHostState>> = LazyLock::new(|| {
    let mut linker = Linker::new(&*SERVO_WASM_ENGINE);
    ParleyEditor::add_to_linker::<_, HasSelf<_>>(&mut linker, |state: &mut ParleyHostState| state)
        .expect("[Servo Parley]: failed to register host APIs in Linker");
    linker
});

struct ActiveParleyEditor {
    store: Store<ParleyHostState>,
    instance: ParleyEditor,
}

thread_local! {
    static ACTIVE_EDITORS: RefCell<Vec<Option<ActiveParleyEditor>>> = const { RefCell::new(Vec::new()) };
}

fn take_editor(id: usize) -> Option<ActiveParleyEditor> {
    ACTIVE_EDITORS.with(|list| list.borrow_mut().get_mut(id).and_then(Option::take))
}

fn put_editor(id: usize, editor: ActiveParleyEditor) {
    ACTIVE_EDITORS.with(|list| {
        if let Some(slot) = list.borrow_mut().get_mut(id) {
            *slot = Some(editor);
        }
    });
}

fn set_attr(cx: &mut JSContext, element: &crate::dom::element::Element, name: &'static str, val: &str) {
    let value = TrustedHTMLOrTrustedScriptOrTrustedScriptURLOrString::String(DOMString::from(val));
    let _ = element.SetAttribute(cx, DOMString::from_static(name), value);
}

/// Instantiate `parley.wasm`, mount `<canvas id="parley-editor">`, and bind input events.
pub(crate) fn register_component(
    cx: &mut JSContext,
    component: &Arc<Component>,
    global_scope: DomRoot<GlobalScope>,
) -> wasmtime::Result<()> {
    let document = global_scope.as_window().Document();

    let canvas = match document
        .GetElementById(cx, DOMString::from_static("parley-editor"))
        .and_then(|el| el.downcast::<HTMLCanvasElement>().map(DomRoot::from_ref))
    {
        Some(existing) => existing,
        None => {
            let elem = document
                .CreateElement(
                    cx,
                    DOMString::from_static("canvas"),
                    StringOrElementCreationOptions::String(DOMString::new()),
                )
                .map_err(|_| wasmtime::Error::msg("failed to create canvas element"))?;
            set_attr(cx, &elem, "id", "parley-editor");
            set_attr(cx, &elem, "tabindex", "0");
            set_attr(
                cx,
                &elem,
                "style",
                "display:block;outline:none;cursor:text;width:960px;height:640px;",
            );
            let canvas = elem
                .downcast::<HTMLCanvasElement>()
                .map(DomRoot::from_ref)
                .ok_or_else(|| wasmtime::Error::msg("created element is not HTMLCanvasElement"))?;
            let _ = canvas.SetWidth(cx, DEFAULT_WIDTH);
            let _ = canvas.SetHeight(cx, DEFAULT_HEIGHT);
            if let Some(body) = document.GetBody() {
                let _ = body.upcast::<Node>().AppendChild(cx, canvas.upcast::<Node>());
            }
            canvas
        },
    };

    if canvas
        .upcast::<crate::dom::element::Element>()
        .GetAttribute(cx, DOMString::from_static("tabindex"))
        .is_none()
    {
        set_attr(cx, canvas.upcast::<crate::dom::element::Element>(), "tabindex", "0");
    }

    let width = canvas.Width().max(1);
    let height = canvas.Height().max(1);

    let editor_id = ACTIVE_EDITORS.with(|list| {
        let mut list = list.borrow_mut();
        list.push(None);
        list.len() - 1
    });

    let target = canvas.upcast::<EventTarget>();
    for event_type in [
        "keydown",
        "mousedown",
        "mousemove",
        "mouseup",
        "compositionupdate",
        "compositionend",
    ] {
        target.add_wasm_event_listener(
            DOMString::from(event_type),
            DOMString::from(format!("parley:{editor_id}:{event_type}")),
        );
    }

    let host_state = ParleyHostState {
        cx: cx as *mut JSContext,
        global_scope,
        canvas: canvas.clone(),
        next_font_id: 1,
        fonts: HashMap::new(),
        font_ids: HashMap::new(),
    };

    let mut store = Store::new(&*SERVO_WASM_ENGINE, host_state);
    let instance = ParleyEditor::instantiate(&mut store, component, &PARLEY_LINKER)?;
    instance.servo_parley_editor_editor().call_init(
        &mut store,
        INITIAL_TEXT,
        width as f32,
        height as f32,
    )?;

    if let Some(html_el) = canvas.downcast::<HTMLElement>() {
        html_el.Focus(cx, &Default::default());
    }

    put_editor(editor_id, ActiveParleyEditor { store, instance });
    Ok(())
}

/// Dispatch a DOM event from `<canvas id="parley-editor">` into `parley.wasm`.
pub(crate) fn dispatch_event(
    cx: &mut JSContext,
    handler_id: &str,
    _target: &EventTarget,
    event: &Event,
) {
    let rest = handler_id.strip_prefix("parley:").unwrap_or(handler_id);
    let Some((id_str, _kind)) = rest.split_once(':') else {
        return;
    };
    let Ok(editor_id) = id_str.parse::<usize>() else {
        return;
    };

    let Some(mut active) = take_editor(editor_id) else {
        return;
    };

    active.store.data_mut().cx = cx;
    let global = active.store.data().global_scope.clone();
    let canvas = active.store.data().canvas.clone();

    let editor_event = translate_dom_event(cx, &global, &canvas, event);
    if let Some(editor_event) = editor_event {
        let _realm = enter_auto_realm::<DomTypeHolder>(cx, &*global);
        match active
            .instance
            .servo_parley_editor_editor()
            .call_handle_event(&mut active.store, &editor_event)
        {
            Ok(feedback) => {
                if let Some(text) = feedback.clipboard_write {
                    let window = global.as_window();
                    window
                        .send_to_embedder(EmbedderMsg::SetClipboardText(window.webview_id(), text));
                }
            },
            Err(error) => {
                eprintln!("[Servo Parley Error]: {error:?}");
            },
        }
    }

    put_editor(editor_id, active);
}

fn read_clipboard(global: &GlobalScope) -> Option<String> {
    let window = global.as_window();
    let (callback, rx) = GenericCallback::new_blocking().ok()?;
    window.send_to_embedder(EmbedderMsg::GetClipboardText(window.webview_id(), callback));
    rx.recv().ok()?.ok()
}

fn translate_dom_event(
    cx: &mut JSContext,
    global: &GlobalScope,
    canvas: &HTMLCanvasElement,
    event: &Event,
) -> Option<EditorEvent> {
    let event_type = event.type_().to_string();
    match event_type.as_str() {
        "keydown" => {
            let kb = event.downcast::<KeyboardEvent>()?;
            let key = kb.Key().to_string();
            let shift = kb.ShiftKey();
            let primary = if cfg!(target_os = "macos") {
                kb.MetaKey()
            } else {
                kb.CtrlKey()
            };
            let word_mod = if cfg!(target_os = "macos") {
                kb.AltKey()
            } else {
                kb.CtrlKey()
            };

            if primary {
                match key.as_str() {
                    "a" | "A" => {
                        event.PreventDefault();
                        return Some(EditorEvent::SelectAll);
                    },
                    "c" | "C" => {
                        event.PreventDefault();
                        return Some(EditorEvent::Copy);
                    },
                    "x" | "X" => {
                        event.PreventDefault();
                        return Some(EditorEvent::Cut);
                    },
                    "v" | "V" => {
                        event.PreventDefault();
                        let text = read_clipboard(global).unwrap_or_default();
                        return Some(EditorEvent::Paste(text));
                    },
                    _ => {},
                }
            }

            let mapped = match key.as_str() {
                "Backspace" if word_mod => EditorEvent::DeleteWordBackward,
                "Backspace" => EditorEvent::DeleteBackward,
                "Delete" => EditorEvent::DeleteForward,
                "Enter" => EditorEvent::InsertText("\n".to_string()),
                "Tab" => EditorEvent::InsertText("    ".to_string()),
                "ArrowLeft" if word_mod => EditorEvent::MoveWordLeft(shift),
                "ArrowRight" if word_mod => EditorEvent::MoveWordRight(shift),
                "ArrowLeft" => EditorEvent::MoveLeft(shift),
                "ArrowRight" => EditorEvent::MoveRight(shift),
                "ArrowUp" => EditorEvent::MoveUp(shift),
                "ArrowDown" => EditorEvent::MoveDown(shift),
                "Home" => EditorEvent::MoveLineStart(shift),
                "End" => EditorEvent::MoveLineEnd(shift),
                s if !primary && !kb.CtrlKey() && !kb.MetaKey() && s.chars().count() == 1 => {
                    EditorEvent::InsertText(s.to_string())
                },
                _ => return None,
            };
            event.PreventDefault();
            Some(mapped)
        },
        "mousedown" => {
            let mouse = event.downcast::<MouseEvent>()?;
            if let Some(html_el) = canvas.downcast::<HTMLElement>() {
                html_el.Focus(cx, &Default::default());
            }
            let click_count = mouse.upcast::<crate::dom::uievent::UIEvent>().Detail().max(1) as u32;
            Some(EditorEvent::PointerDown((
                mouse.OffsetX(cx) as f32,
                mouse.OffsetY(cx) as f32,
                click_count,
                mouse.ShiftKey(),
            )))
        },
        "mousemove" => {
            let mouse = event.downcast::<MouseEvent>()?;
            Some(EditorEvent::PointerMove((
                mouse.OffsetX(cx) as f32,
                mouse.OffsetY(cx) as f32,
            )))
        },
        "mouseup" => Some(EditorEvent::PointerUp),
        "compositionupdate" => {
            let comp = event.downcast::<CompositionEvent>()?;
            Some(EditorEvent::ComposeUpdate((comp.Data().to_string(), None)))
        },
        "compositionend" => {
            let comp = event.downcast::<CompositionEvent>()?;
            Some(EditorEvent::ComposeCommit(comp.Data().to_string()))
        },
        _ => None,
    }
}
