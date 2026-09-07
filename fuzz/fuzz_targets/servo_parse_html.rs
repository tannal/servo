#![no_main]

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

use dpi::PhysicalSize;
use libfuzzer_sys::fuzz_target;
use servo::resources::{Resource, ResourceReaderMethods};
use servo::{
    EventLoopWaker, LoadStatus, RenderingContext, Servo, ServoBuilder,
    SoftwareRenderingContext, WebView, WebViewBuilder, submit_resource_reader,
};
use url::Url;

struct FuzzResourceReader;

impl ResourceReaderMethods for FuzzResourceReader {
    fn read(&self, _res: Resource) -> Vec<u8> { vec![] }
    fn sandbox_access_files(&self) -> Vec<PathBuf> { vec![] }
    fn sandbox_access_files_dirs(&self) -> Vec<PathBuf> { vec![] }
}

static FUZZ_RESOURCE_READER: FuzzResourceReader = FuzzResourceReader;
submit_resource_reader!(&FUZZ_RESOURCE_READER);

pub struct ServoFuzzerHelper {
    servo: Servo,
    webview: WebView,
}

impl ServoFuzzerHelper {
    pub fn new() -> Self {
        let rendering_context = Rc::new(
            SoftwareRenderingContext::new(PhysicalSize { width: 100, height: 100 })
                .expect("Could not create SoftwareRenderingContext"),
        );
        assert!(rendering_context.make_current().is_ok());

        #[derive(Clone)]
        struct EventLoopWakerImpl(Arc<AtomicBool>);
        impl EventLoopWaker for EventLoopWakerImpl {
            fn clone_box(&self) -> Box<dyn EventLoopWaker> { Box::new(self.clone()) }
            fn wake(&self) { self.0.store(true, Ordering::Relaxed); }
        }

        let user_event_triggered = Arc::new(AtomicBool::new(false));
        let servo = ServoBuilder::default()
            .event_loop_waker(Box::new(EventLoopWakerImpl(user_event_triggered)))
            .build();

        let webview = WebViewBuilder::new(&servo, rendering_context).build();

        Self { servo, webview }
    }

    pub fn load_html(&self, html_content: &str) {
        if html_content.len() > 8192 {
            return;
        }

        let data_url_str = format!("data:text/html;charset=utf-8,{}", html_content);
        let url = match Url::parse(&data_url_str) {
            Ok(u) => u,
            Err(_) => return,
        };

        self.webview.load(url);

        let mut max_spins = 1000;
        while self.webview.load_status() != LoadStatus::Complete && max_spins > 0 {
            self.servo.spin_event_loop();
            max_spins -= 1;
        }
    }
}

thread_local! {
    static SERVO_INSTANCE: ServoFuzzerHelper = ServoFuzzerHelper::new();
}

fuzz_target!(|data: &[u8]| {
    let input_str = match std::str::from_utf8(data) {
        Ok(s) => s,
        Err(_) => return,
    };

    SERVO_INSTANCE.with(|helper| {
        helper.load_html(input_str);
    });
});