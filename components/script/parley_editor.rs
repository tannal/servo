/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Native `parley.wasm` text editor host integration.
//!
//! Instantiates WebAssembly components exporting `servo:parley-editor/editor@0.1.0`,
//! bridges Servo's `FontContext` (`servo:fonts`) and Canvas 2D / Vello backend
//! (`servo:surface`), activates OS IME via `DocumentEmbedderControls`, drives
//! native 530ms caret blinking on Servo's event loop, and routes keyboard,
//! pointer, wheel, IME composition/commit, and clipboard events into Parley.

#![allow(unsafe_code)]

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use app_units::Au;
use embedder_traits::{
    EmbedderControlRequest, EmbedderMsg, InputMethodRequest, InputMethodType,
};
use euclid::{Point2D as TypedPoint2D, Rect as TypedRect, Size2D as TypedSize2D};
use euclid::default::{Point2D, Rect as EuclidRect, Size2D, Transform2D};
use fonts::{FontDataAndIndex, FontIdentifier};
use icu_locale_core::LanguageIdentifier;
use js::context::JSContext;
use keyboard_types::{CompositionEvent as KeyboardCompositionEvent, CompositionState};
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
use style_traits::CSSPixel;
use wasmtime::Store;
use wasmtime::component::{Component, HasSelf, Linker};

use crate::canvas_context::CanvasContext;
use crate::dom::bindings::codegen::Bindings::DocumentBinding::DocumentMethods;
use crate::dom::bindings::codegen::Bindings::ElementBinding::ElementMethods;
use crate::dom::bindings::codegen::Bindings::EventBinding::EventMethods;
use crate::dom::bindings::codegen::Bindings::HTMLCanvasElementBinding::HTMLCanvasElementMethods;
use crate::dom::bindings::codegen::Bindings::HTMLElementBinding::HTMLElementMethods;
use crate::dom::bindings::codegen::Bindings::KeyboardEventBinding::KeyboardEventMethods;
use crate::dom::bindings::codegen::Bindings::MouseEventBinding::MouseEventMethods;
use crate::dom::bindings::codegen::Bindings::NodeBinding::NodeMethods;
use crate::dom::bindings::codegen::Bindings::UIEventBinding::UIEventMethods;
use crate::dom::bindings::codegen::Bindings::WheelEventBinding::WheelEventMethods;
use crate::dom::bindings::codegen::Bindings::WindowBinding::WindowMethods;
use crate::dom::bindings::codegen::DomTypeHolder::DomTypeHolder;
use crate::dom::bindings::inheritance::Castable;
use crate::dom::bindings::root::{Dom, DomRoot};
use crate::dom::bindings::str::DOMString;
use crate::dom::document_embedder_controls::ControlElement;
use crate::dom::event::Event;
use crate::dom::eventtarget::EventTarget;
use crate::dom::globalscope::GlobalScope;
use crate::dom::html::htmlcanvaselement::HTMLCanvasElement;
use crate::dom::html::htmlelement::HTMLElement;
use crate::dom::keyboardevent::KeyboardEvent;
use crate::dom::mouseevent::MouseEvent;
use crate::dom::node::Node;
use crate::dom::wheelevent::WheelEvent;
use crate::event_loop::timers::{OneshotTimerCallback, OneshotTimerHandle};

pub mod wit_parley {
    wasmtime::component::bindgen!({
        world: "parley-editor",
        path: "wit/parley_editor.wit",
    });
}

use wit_parley::ParleyEditor;
use wit_parley::servo::parley_editor::console::Host as ConsoleHost;
use wit_parley::servo::parley_editor::editing::{EditorEvent, Rect as EditRect};
use wit_parley::servo::parley_editor::fonts::{FontQuery, Host as FontsHost, ResolvedFont};
use wit_parley::servo::parley_editor::surface::{Host as SurfaceHost, PaintFrame, Rgba};

const DEFAULT_WIDTH: u32 = 960;
const DEFAULT_HEIGHT: u32 = 640;
const BLINK_INTERVAL: Duration = Duration::from_millis(530);

const INITIAL_TEXT: &str = "Parley.wasm Text Editor in Servo\n\n\
    Shaping, line breaking, bidi, selection, undo/redo, IME composition, and hit-testing \
    run in parley.wasm (wasm32-unknown-unknown Component Model).\n\
    Fonts come from Servo's FontContext and glyphs render directly through \
    Servo's Vello Canvas 2D backend.\n\n\
    Try typing (including Chinese/Japanese/Korean IME), dragging with the mouse, \
    double/triple-clicking, Ctrl/Cmd+Z / Shift+Z undo/redo, or pasting long text.";

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
        let font = match query.sample_char {
            Some(ch) => font_group.find_by_codepoint(
                &font_context,
                ch,
                None,
                LanguageIdentifier::UNKNOWN.language,
            )?,
            None => font_group.first(&font_context)?,
        };

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

        for (rect, color) in frame.rects {
            commands.push(CanvasCommand::FillRect(
                EuclidRect::new(Point2D::new(rect.x, rect.y), Size2D::new(rect.w, rect.h)),
                FillOrStrokeStyle::Color(rgba_to_color(color)),
                shadow,
                composition,
                identity,
            ));
        }

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
    focused: bool,
    blink_visible: bool,
    blink_timer: Option<OneshotTimerHandle>,
    last_caret: Option<EditRect>,
    ime_rect_synced: bool,
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

fn find_editor_id_for_node(node: &Node) -> Option<usize> {
    ACTIVE_EDITORS.with(|list| {
        let list = list.borrow();
        for (idx, slot) in list.iter().enumerate() {
            if let Some(active) = slot {
                if active.store.data().canvas.upcast::<Node>() == node {
                    return Some(idx);
                }
            }
        }
        None
    })
}

fn set_attr(
    cx: &mut JSContext,
    element: &crate::dom::element::Element,
    name: &'static str,
    val: &str,
) {
    let value = TrustedHTMLOrTrustedScriptOrTrustedScriptURLOrString::String(DOMString::from(val));
    let _ = element.SetAttribute(cx, DOMString::from_static(name), value);
}

fn canvas_local_point(
    cx: &mut JSContext,
    canvas: &HTMLCanvasElement,
    mouse: &MouseEvent,
) -> (f32, f32) {
    // Use `border_box(cx)` (exact viewport-relative rect) rather than `OffsetX/Y`,
    // and scale to the canvas backing dimensions so clicks and drags track the
    // pointer 1:1 even when centered or styled with borders/CSS sizing.
    let rect = canvas.upcast::<Node>().border_box(cx).unwrap_or_default();
    let css_x = mouse.ClientX() as f32 - rect.origin.x.to_f32_px();
    let css_y = mouse.ClientY() as f32 - rect.origin.y.to_f32_px();
    let box_w = rect.size.width.to_f32_px();
    let box_h = rect.size.height.to_f32_px();
    let scale_x = if box_w > 0.0 {
        canvas.Width() as f32 / box_w
    } else {
        1.0
    };
    let scale_y = if box_h > 0.0 {
        canvas.Height() as f32 / box_h
    } else {
        1.0
    };
    (css_x * scale_x, css_y * scale_y)
}

fn caret_viewport_rect(
    cx: &mut JSContext,
    canvas: &HTMLCanvasElement,
    caret: Option<EditRect>,
) -> Option<TypedRect<Au, CSSPixel>> {
    let rect = canvas.upcast::<Node>().border_box(cx)?;
    let box_w = rect.size.width.to_f32_px();
    let box_h = rect.size.height.to_f32_px();
    if box_w <= 0.0 || box_h <= 0.0 {
        return None;
    }
    let cw = canvas.Width().max(1) as f32;
    let ch = canvas.Height().max(1) as f32;
    let sx = box_w / cw;
    let sy = box_h / ch;
    let (lx, ly, lw, lh) = match caret {
        Some(c) => (c.x * sx, c.y * sy, (c.w * sx).max(2.0), (c.h * sy).max(20.0)),
        None => (24.0 * sx, 24.0 * sy, 2.0, 24.0 * sy),
    };
    Some(TypedRect::new(
        TypedPoint2D::new(
            rect.origin.x + Au::from_f32_px(lx),
            rect.origin.y + Au::from_f32_px(ly),
        ),
        TypedSize2D::new(Au::from_f32_px(lw), Au::from_f32_px(lh)),
    ))
}

fn show_ime(cx: &mut JSContext, active: &mut ActiveParleyEditor) {
    let global = active.store.data().global_scope.clone();
    let canvas = active.store.data().canvas.clone();
    let document = global.as_window().Document();
    let text = active
        .instance
        .servo_parley_editor_editor()
        .call_get_text(&mut active.store)
        .unwrap_or_default();
    let Some(rect) = caret_viewport_rect(cx, &canvas, active.last_caret) else {
        return;
    };
    if let Some(html_el) = canvas.downcast::<HTMLElement>() {
        document.embedder_controls().show_embedder_control_rect(
            ControlElement::Ime(Dom::from_ref(html_el)),
            EmbedderControlRequest::InputMethod(InputMethodRequest {
                input_method_type: InputMethodType::Text,
                text,
                insertion_point: None,
                multiline: true,
                allow_virtual_keyboard: true,
            }),
            rect,
        );
    }
}

fn hide_ime(active: &ActiveParleyEditor) {
    let global = active.store.data().global_scope.clone();
    let canvas = active.store.data().canvas.clone();
    let document = global.as_window().Document();
    document
        .embedder_controls()
        .hide_embedder_control_for_element(canvas.upcast());
}

fn reset_blink_timer(editor_id: usize, active: &mut ActiveParleyEditor) {
    let global = active.store.data().global_scope.clone();
    if let Some(handle) = active.blink_timer.take() {
        global.unschedule_callback(handle);
    }
    if active.focused {
        active.blink_visible = true;
        let handle = global.schedule_callback(
            OneshotTimerCallback::ParleyBlink(editor_id),
            BLINK_INTERVAL,
        );
        active.blink_timer = Some(handle);
    }
}

/// Called by Servo's native event loop timer every 530ms to toggle caret visibility.
pub(crate) fn on_blink_timer(cx: &mut JSContext, editor_id: usize) {
    let Some(mut active) = take_editor(editor_id) else {
        return;
    };
    active.blink_timer = None;

    if active.focused {
        active.store.data_mut().cx = cx;
        let global = active.store.data().global_scope.clone();
        active.blink_visible = !active.blink_visible;
        let visible = active.blink_visible;
        {
            let _realm = enter_auto_realm::<DomTypeHolder>(cx, &*global);
            if let Ok(fb) = active
                .instance
                .servo_parley_editor_editor()
                .call_handle_event(&mut active.store, &EditorEvent::SetCursorVisible(visible))
            {
                if fb.caret_rect.is_some() {
                    active.last_caret = fb.caret_rect;
                }
            }
        }
        if !active.ime_rect_synced {
            show_ime(cx, &mut active);
            active.ime_rect_synced = true;
        }
        let handle = global.schedule_callback(
            OneshotTimerCallback::ParleyBlink(editor_id),
            BLINK_INTERVAL,
        );
        active.blink_timer = Some(handle);
    }

    put_editor(editor_id, active);
}

/// Instantiate `parley.wasm`, mount `<canvas id="parley-editor">`, activate IME,
/// and bind keyboard, pointer, wheel, focus, and blink timers.
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
        set_attr(
            cx,
            canvas.upcast::<crate::dom::element::Element>(),
            "tabindex",
            "0",
        );
    }

    let width = canvas.Width().max(1);
    let height = canvas.Height().max(1);

    let editor_id = ACTIVE_EDITORS.with(|list| {
        let mut list = list.borrow_mut();
        list.push(None);
        list.len() - 1
    });

    let canvas_target = canvas.upcast::<EventTarget>();
    for event_type in ["keydown", "mousedown", "wheel", "focus", "blur"] {
        canvas_target.add_wasm_event_listener(
            DOMString::from(event_type),
            DOMString::from(format!("parley:{editor_id}:{event_type}")),
        );
    }
    // Listen for mousemove and mouseup on `window` so drag selection stays smooth
    // even when the pointer leaves the canvas bounds.
    let window_target = global_scope.as_window().upcast::<EventTarget>();
    for event_type in ["mousemove", "mouseup"] {
        window_target.add_wasm_event_listener(
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

    let mut active = ActiveParleyEditor {
        store,
        instance,
        focused: true,
        blink_visible: true,
        blink_timer: None,
        last_caret: None,
        ime_rect_synced: false,
    };
    reset_blink_timer(editor_id, &mut active);
    put_editor(editor_id, active);

    // Focus AFTER inserting into ACTIVE_EDITORS so the `focus` event activates IME.
    if let Some(html_el) = canvas.downcast::<HTMLElement>() {
        html_el.Focus(cx, &Default::default());
    }

    Ok(())
}

/// Forward an OS `ImeEvent::Commit(text)` directly into the focused `parley.wasm` editor.
pub(crate) fn handle_ime_commit(cx: &mut JSContext, focused_node: &Node, text: &str) -> bool {
    let Some(editor_id) = find_editor_id_for_node(focused_node) else {
        return false;
    };
    println!("[Servo Parley IME] commit: {text:?}");
    send_editor_event(cx, editor_id, EditorEvent::ComposeCommit(text.to_string()));
    true
}

/// Forward an OS `ImeEvent::Composition` (Start / Update / End) into the focused `parley.wasm` editor.
pub(crate) fn handle_ime_composition(
    cx: &mut JSContext,
    focused_node: &Node,
    comp: &KeyboardCompositionEvent,
) -> bool {
    let Some(editor_id) = find_editor_id_for_node(focused_node) else {
        return false;
    };
    println!("[Servo Parley IME] composition {:?}: {:?}", comp.state, comp.data);
    let event = match comp.state {
        CompositionState::Start => {
            if comp.data.is_empty() {
                return true;
            }
            let len = comp.data.len() as u32;
            EditorEvent::ComposeUpdate((comp.data.clone(), Some((len, len))))
        },
        CompositionState::Update => {
            if comp.data.is_empty() {
                EditorEvent::ComposeCancel
            } else {
                let len = comp.data.len() as u32;
                EditorEvent::ComposeUpdate((comp.data.clone(), Some((len, len))))
            }
        },
        CompositionState::End => {
            if comp.data.is_empty() {
                EditorEvent::ComposeCancel
            } else {
                EditorEvent::ComposeCommit(comp.data.clone())
            }
        },
    };
    send_editor_event(cx, editor_id, event);
    true
}

fn send_editor_event(cx: &mut JSContext, editor_id: usize, editor_event: EditorEvent) {
    let Some(mut active) = take_editor(editor_id) else {
        return;
    };
    active.store.data_mut().cx = cx;
    let global = active.store.data().global_scope.clone();
    let should_reset_blink = !matches!(
        editor_event,
        EditorEvent::SetCursorVisible(_)
            | EditorEvent::SetFocused(false)
            | EditorEvent::PointerUp
            | EditorEvent::Scroll(_)
    );

    let mut caret_changed = false;
    {
        let _realm = enter_auto_realm::<DomTypeHolder>(cx, &*global);
        match active
            .instance
            .servo_parley_editor_editor()
            .call_handle_event(&mut active.store, &editor_event)
        {
            Ok(feedback) => {
                if let Some(caret) = feedback.caret_rect {
                    let prev = active.last_caret;
                    caret_changed = prev.map_or(true, |p| {
                        (p.x - caret.x).abs() > 0.5 || (p.y - caret.y).abs() > 0.5
                    });
                    active.last_caret = Some(caret);
                }
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

    if should_reset_blink && active.focused {
        reset_blink_timer(editor_id, &mut active);
    }
    if caret_changed && active.focused && !matches!(editor_event, EditorEvent::ComposeUpdate(_)) {
        show_ime(cx, &mut active);
    }

    put_editor(editor_id, active);
}

/// Dispatch a DOM event from `<canvas id="parley-editor">` or `window` into `parley.wasm`.
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

    let event_type = event.type_().to_string();
    if event_type == "focus" {
        if let Some(mut active) = take_editor(editor_id) {
            active.focused = true;
            active.store.data_mut().cx = cx;
            let global = active.store.data().global_scope.clone();
            {
                let _realm = enter_auto_realm::<DomTypeHolder>(cx, &*global);
                let _ = active
                    .instance
                    .servo_parley_editor_editor()
                    .call_handle_event(&mut active.store, &EditorEvent::SetFocused(true));
            }
            reset_blink_timer(editor_id, &mut active);
            show_ime(cx, &mut active);
            put_editor(editor_id, active);
        }
        return;
    }
    if event_type == "blur" {
        if let Some(mut active) = take_editor(editor_id) {
            active.focused = false;
            active.store.data_mut().cx = cx;
            let global = active.store.data().global_scope.clone();
            if let Some(handle) = active.blink_timer.take() {
                global.unschedule_callback(handle);
            }
            hide_ime(&active);
            {
                let _realm = enter_auto_realm::<DomTypeHolder>(cx, &*global);
                let _ = active
                    .instance
                    .servo_parley_editor_editor()
                    .call_handle_event(&mut active.store, &EditorEvent::SetFocused(false));
            }
            put_editor(editor_id, active);
        }
        return;
    }

    let Some((global, canvas)) = ACTIVE_EDITORS.with(|list| {
        let list = list.borrow();
        let Some(Some(active)) = list.get(editor_id) else {
            return None;
        };
        Some((
            active.store.data().global_scope.clone(),
            active.store.data().canvas.clone(),
        ))
    }) else {
        return;
    };

    if let Some(editor_event) = translate_dom_event(cx, &global, &canvas, event) {
        send_editor_event(cx, editor_id, editor_event);
    }
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
                    "z" | "Z" => {
                        event.PreventDefault();
                        return Some(if shift {
                            EditorEvent::Redo
                        } else {
                            EditorEvent::Undo
                        });
                    },
                    "y" | "Y" => {
                        event.PreventDefault();
                        return Some(EditorEvent::Redo);
                    },
                    "Home" => {
                        event.PreventDefault();
                        return Some(EditorEvent::MoveTextStart(shift));
                    },
                    "End" => {
                        event.PreventDefault();
                        return Some(EditorEvent::MoveTextEnd(shift));
                    },
                    #[cfg(target_os = "macos")]
                    "ArrowLeft" => {
                        event.PreventDefault();
                        return Some(EditorEvent::MoveLineStart(shift));
                    },
                    #[cfg(target_os = "macos")]
                    "ArrowRight" => {
                        event.PreventDefault();
                        return Some(EditorEvent::MoveLineEnd(shift));
                    },
                    #[cfg(target_os = "macos")]
                    "ArrowUp" => {
                        event.PreventDefault();
                        return Some(EditorEvent::MoveTextStart(shift));
                    },
                    #[cfg(target_os = "macos")]
                    "ArrowDown" => {
                        event.PreventDefault();
                        return Some(EditorEvent::MoveTextEnd(shift));
                    },
                    _ => {},
                }
            }

            let mapped = match key.as_str() {
                "Backspace" if word_mod => EditorEvent::DeleteWordBackward,
                "Backspace" => EditorEvent::DeleteBackward,
                "Delete" if word_mod => EditorEvent::DeleteWordForward,
                "Delete" => EditorEvent::DeleteForward,
                "Enter" => EditorEvent::InsertText("\n".to_string()),
                "Tab" => EditorEvent::InsertText("    ".to_string()),
                "Escape" => EditorEvent::CollapseSelection,
                "ArrowLeft" if word_mod => EditorEvent::MoveWordLeft(shift),
                "ArrowRight" if word_mod => EditorEvent::MoveWordRight(shift),
                "ArrowLeft" => EditorEvent::MoveLeft(shift),
                "ArrowRight" => EditorEvent::MoveRight(shift),
                "ArrowUp" => EditorEvent::MoveUp(shift),
                "ArrowDown" => EditorEvent::MoveDown(shift),
                "Home" => EditorEvent::MoveLineStart(shift),
                "End" => EditorEvent::MoveLineEnd(shift),
                "PageUp" => EditorEvent::PageUp(shift),
                "PageDown" => EditorEvent::PageDown(shift),
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
            if mouse.Button() != 0 {
                return None;
            }
            event.PreventDefault();
            if let Some(html_el) = canvas.downcast::<HTMLElement>() {
                html_el.Focus(cx, &Default::default());
            }
            let (x, y) = canvas_local_point(cx, canvas, mouse);
            let click_count = mouse
                .upcast::<crate::dom::uievent::UIEvent>()
                .Detail()
                .max(1) as u32;
            Some(EditorEvent::PointerDown((x, y, click_count, mouse.ShiftKey())))
        },
        "mousemove" => {
            let mouse = event.downcast::<MouseEvent>()?;
            if mouse.Buttons() & 1 == 0 {
                return Some(EditorEvent::PointerUp);
            }
            let (x, y) = canvas_local_point(cx, canvas, mouse);
            Some(EditorEvent::PointerMove((x, y)))
        },
        "mouseup" => Some(EditorEvent::PointerUp),
        "wheel" => {
            let wheel = event.downcast::<WheelEvent>()?;
            event.PreventDefault();
            Some(EditorEvent::Scroll(*wheel.DeltaY() as f32))
        },
        _ => None,
    }
}
