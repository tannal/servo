#![no_main]

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use dpi::PhysicalSize;
use libfuzzer_sys::fuzz_target;
use servo::{
    EventLoopWaker, JSValue, JavaScriptEvaluationError, LoadStatus, RenderingContext, Servo,
    ServoBuilder, SoftwareRenderingContext, WebViewBuilder,
};
use url::Url;
use std::path::PathBuf;

use servo::{submit_resource_reader, resources::{
    Resource, ResourceReaderMethods,
}};

struct FuzzResourceReader;

impl ResourceReaderMethods for FuzzResourceReader {
    fn read(&self, _res: Resource) -> Vec<u8> {
        vec![]
    }

    fn sandbox_access_files(&self) -> Vec<PathBuf> {
        vec![]
    }

    fn sandbox_access_files_dirs(&self) -> Vec<PathBuf> {
        vec![]
    }
}

static FUZZ_RESOURCE_READER: FuzzResourceReader = FuzzResourceReader;
submit_resource_reader!(&FUZZ_RESOURCE_READER);

pub struct ServoFuzzerHelper {
    servo: Servo,
    rendering_context: Rc<SoftwareRenderingContext>,
}

impl Drop for ServoFuzzerHelper {
    fn drop(&mut self) {
        self.servo.spin_event_loop();
        std::thread::sleep(Duration::from_millis(1));
    }
}

impl ServoFuzzerHelper {
    pub fn new() -> Self {
        let rendering_context = Rc::new(
            SoftwareRenderingContext::new(PhysicalSize {
                width: 800,
                height: 600,
            })
            .expect("Could not create SoftwareRenderingContext"),
        );
        assert!(rendering_context.make_current().is_ok());

        #[derive(Clone)]
        struct EventLoopWakerImpl(Arc<AtomicBool>);
        impl EventLoopWaker for EventLoopWakerImpl {
            fn clone_box(&self) -> Box<dyn EventLoopWaker> {
                Box::new(self.clone())
            }

            fn wake(&self) {
                self.0.store(true, Ordering::Relaxed);
            }
        }

        let user_event_triggered = Arc::new(AtomicBool::new(false));
        let servo = ServoBuilder::default()
            .event_loop_waker(Box::new(EventLoopWakerImpl(user_event_triggered)))
            .build();

        Self {
            servo,
            rendering_context,
        }
    }

    pub fn servo(&self) -> &Servo {
        &self.servo
    }

    pub fn spin<F>(&self, callback: F)
    where
        F: Fn() -> bool + 'static,
    {
        while callback() {
            std::thread::sleep(Duration::from_millis(1));
            self.servo.spin_event_loop();
        }
    }

    pub fn load_html(&self, html_content: &str) {
        let webview = WebViewBuilder::new(&self.servo, self.rendering_context.clone()).build();

        let data_url_str = format!("data:text/html;charset=utf-8,{}", html_content);

        let url = match Url::parse(&data_url_str) {
            Ok(u) => u,
            Err(_) => return,
        };

        webview.load(url);

        let load_webview = webview.clone();
        self.spin(move || load_webview.load_status() != LoadStatus::Complete);
    }

    pub fn eval_js(&self, script: &str) -> Result<JSValue, JavaScriptEvaluationError> {
        let webview = WebViewBuilder::new(&self.servo, self.rendering_context.clone()).build();
        
        self.load_html("<html><body></body></html>");

        let saved_result = Rc::new(RefCell::new(None));
        let callback_result = saved_result.clone();
        webview.evaluate_javascript(script.to_string(), move |result| {
            *callback_result.borrow_mut() = Some(result);
        });

        let spin_result = saved_result.clone();
        self.spin(move || spin_result.borrow().is_none());

        let result = saved_result
            .borrow()
            .clone()
            .expect("Should have waited until value available");

        result
    }
}

thread_local! {
    static SERVO_INSTANCE: OnceLock<ServoFuzzerHelper> = OnceLock::new();
}

fuzz_target!(|data: &[u8]| {
    let input_str = match std::str::from_utf8(data) {
        Ok(s) => s,
        Err(_) => return,
    };

    SERVO_INSTANCE.with(|cell| {
        let helper = cell.get_or_init(|| ServoFuzzerHelper::new());

        helper.load_html(input_str);

        // let _ = helper.eval_js(input_str);
    });
});