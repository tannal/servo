/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use js::context::NoGC;
use script_bindings::refcounted::Trusted;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread;
use std::time::Duration;
use style::Atom;

// Use the existing servo_media dependency from Cargo.toml
use servo_media::player::video::{Buffer, VideoFrame, VideoFrameData};

use crate::dom::Document;
use crate::dom::bindings::codegen::Bindings::WindowBinding::WindowMethods;
use crate::dom::bindings::inheritance::Castable;
use crate::dom::eventtarget::EventTarget;
use crate::dom::htmlvideoelement::HTMLVideoElement;

// Import the correct generated trait from `wit_dom`
use super::wit_dom::servo::dom::video::{Host as VideoHost, PixelFormat, VideoMetadata};

// Import the correct state struct from `state.rs`
use super::state::WasmHostState;

pub struct WasmMediaClock {
    pub is_playing: Arc<AtomicBool>,
}
impl WasmMediaClock {
    pub fn start<F>(is_playing: Arc<AtomicBool>, mut on_tick: F)
    where
        F: FnMut() + Send + 'static,
    {
        thread::spawn(move || {
            while Arc::strong_count(&is_playing) > 1 {
                thread::sleep(Duration::from_millis(33)); // 30 fps
                if is_playing.load(Ordering::Relaxed) {
                    on_tick();
                }
            }
        });
    }
}

static TICK_IN_FLIGHT: AtomicBool = AtomicBool::new(false);
static ACTIVE_DOC_ADDR: AtomicUsize = AtomicUsize::new(0);

pub fn start_wasm_media_clock(document: &Document, is_playing: Arc<AtomicBool>) {
    let current_doc_addr = document as *const Document as usize;
    // 1. Guard against duplicate threads on the same Document
    if ACTIVE_DOC_ADDR.swap(current_doc_addr, Ordering::SeqCst) == current_doc_addr {
        return;
    }
    // 2. Reset in-flight guard so a reload (Ctrl+R) never inherits a stuck lock
    TICK_IN_FLIGHT.store(false, Ordering::Release);
    let task_source = document
        .window()
        .as_global_scope()
        .task_manager()
        .dom_manipulation_task_source()
        .to_sendable();
    let trusted_doc = Trusted::new(document);
    thread::Builder::new()
        .name("wasm-media-clock".into())
        .spawn(move || {
            while is_playing.load(Ordering::Relaxed) {
                thread::sleep(Duration::from_millis(33));
                // 3. Exit immediately if the user reloaded (Ctrl+R) or navigated away
                if ACTIVE_DOC_ADDR.load(Ordering::Relaxed) != current_doc_addr {
                    break;
                }
                // 4. Drop tick if the script thread is still processing the previous tick
                if TICK_IN_FLIGHT
                    .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
                    .is_err()
                {
                    continue;
                }
                task_source.queue(task!(wasm_media_tick: move |cx| {
                    let _ = crate::wasm_host::dispatch_wasm_event(
                        cx,
                        0,
                        &"wasm-video".to_string(),
                        &"tick".to_string(),
                        None,
                        None,
                    );
                    TICK_IN_FLIGHT.store(false, Ordering::Release);
                }));
                // 5. If queueing failed (window destroyed/cancelled), release lock and terminate
                // if res.is_err() {
                //     TICK_IN_FLIGHT.store(false, Ordering::Release);
                //     break;
                // }
            }
        })
        .expect("Failed to spawn wasm-media-clock thread");
}

/// Buffer implementation that wraps an in-memory frame decoded by Wasm
struct WasmVideoBuffer {
    data: Arc<Vec<u8>>,
}

impl Buffer for WasmVideoBuffer {
    fn to_vec(&self) -> Option<VideoFrameData> {
        Some(VideoFrameData::Raw(self.data.clone()))
    }
}

static CLOCK_STARTED: AtomicBool = AtomicBool::new(false);

#[allow(unsafe_code)]
impl VideoHost for WasmHostState {
    fn register_stream(&mut self, _element_id: String, _metadata: VideoMetadata) -> bool {
        true
    }

    fn submit_frame(
        &mut self,
        element_id: String,
        width: u32,
        height: u32,
        _format: PixelFormat,
        data: Vec<u8>,
    ) {
        let window = self.global_scope.as_window();
        let document = window.Document();
        let atom_id = Atom::from(element_id);
        let no_gc = unsafe { NoGC::new() };

        if let Some(element) = document.get_element_by_id(&no_gc, &atom_id) {
            if let Some(video) = element.downcast::<HTMLVideoElement>() {
                let buffer = Arc::new(WasmVideoBuffer {
                    data: Arc::new(data),
                });

                if let Some(frame) = VideoFrame::new(width as i32, height as i32, buffer) {
                    video.handle_wasm_video_frame(frame);
                }
            }
        }

        let is_playing = Arc::new(AtomicBool::new(true));
        start_wasm_media_clock(&document, is_playing);
    }

    fn update_time(&mut self, element_id: String, current_time: f64) {
        let cx = unsafe { &mut *self.cx };
        let window = self.global_scope.as_window();
        let document = window.Document();
        let atom_id = Atom::from(element_id);
        let no_gc = unsafe { NoGC::new() };

        if let Some(element) = document.get_element_by_id(&no_gc, &atom_id) {
            if let Some(video) = element.downcast::<HTMLVideoElement>() {
                video.set_current_time_from_wasm(cx, current_time);
            }
        }
    }

    fn end_of_stream(&mut self, _element_id: String) {}
}
