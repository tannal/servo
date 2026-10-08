/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use js::context::NoGC;
use script_bindings::refcounted::Trusted;
use std::sync::Arc;
use style::Atom;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

// Use the existing servo_media dependency from Cargo.toml
use servo_media::player::video::{Buffer, VideoFrame, VideoFrameData};

use crate::dom::bindings::codegen::Bindings::WindowBinding::WindowMethods;
use crate::dom::bindings::inheritance::Castable;
use crate::dom::htmlvideoelement::HTMLVideoElement;
use crate::dom::Document;
use crate::dom::eventtarget::EventTarget;

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

pub fn start_wasm_media_clock(document: &Document, is_playing: Arc<AtomicBool>) {
    let task_source = document
        .window()
        .as_global_scope()
        .task_manager()
        .dom_manipulation_task_source()
        .to_sendable();
    let trusted_doc = Trusted::new(document);
    thread::spawn(move || {
        while is_playing.load(Ordering::Relaxed) {
            thread::sleep(Duration::from_millis(33)); // 30 fps
            let trusted_doc = trusted_doc.clone();
            task_source.queue(task!(wasm_media_tick: move |cx| {
                let doc = trusted_doc.root();
                // Call the Wasm event dispatcher directly (bypassing DOM event listener table)
                let _ = crate::wasm_host::dispatch_wasm_event(
                    cx,
                    0, // app_index
                    &"wasm-video".to_string(),
                    &"tick".to_string(),
                    None,
                    None,
                );
            }));
        }
    });
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

        if !CLOCK_STARTED.swap(true, Ordering::Relaxed) {
            let is_playing = Arc::new(AtomicBool::new(true));
            start_wasm_media_clock(&document, is_playing);
        }
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
