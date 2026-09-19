#![no_main]

use dpi::PhysicalSize;
use libfuzzer_sys::fuzz_target;
use servo::resources::{Resource, ResourceReaderMethods};
use servo::{
    EventLoopWaker, LoadStatus, RenderingContext, Servo, ServoBuilder, SoftwareRenderingContext,
    WebView, WebViewBuilder, WebViewDelegate, submit_resource_reader,
};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use url::Url;

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

struct FuzzWebViewDelegate {
    frame_ready: Arc<AtomicBool>,
}

impl WebViewDelegate for FuzzWebViewDelegate {
    fn notify_new_frame_ready(&self, _: WebView) {
        self.frame_ready.store(true, Ordering::Relaxed);
    }
}

pub struct ServoFuzzerHelper {
    servo: Servo,
    rendering_context: Rc<SoftwareRenderingContext>,
    waker_triggered: Arc<AtomicBool>,
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
        let _ = rendering_context.make_current();

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

        let waker_triggered = Arc::new(AtomicBool::new(false));

        let servo = ServoBuilder::default()
            .event_loop_waker(Box::new(EventLoopWakerImpl(waker_triggered.clone())))
            .build();

        Self {
            servo,
            rendering_context,
            waker_triggered,
        }
    }

    pub fn process_html(&mut self, html_content: &str) {
        let frame_ready = Arc::new(AtomicBool::new(false));
        let delegate = Rc::new(FuzzWebViewDelegate {
            frame_ready: frame_ready.clone(),
        });

        let webview = WebViewBuilder::new(&self.servo, self.rendering_context.clone())
            .delegate(delegate)
            .build();

        self.waker_triggered.store(false, Ordering::Relaxed);

        let data_url_str = format!("data:text/html;charset=utf-8,{}", html_content);
        if let Ok(url) = Url::parse(&data_url_str) {
            webview.load(url);

            let mut idle_spins = 0;
            let max_idle_spins = 50;
            let mut painted = false;

            loop {
                self.servo.spin_event_loop();
                let is_complete = webview.load_status() == LoadStatus::Complete;

                if is_complete && !painted {
                    webview.paint();
                    painted = true;
                }

                if painted && frame_ready.load(Ordering::Relaxed) {
                    break;
                }

                if self.waker_triggered.swap(false, Ordering::Relaxed) {
                    idle_spins = 0;
                } else {
                    if painted {
                        idle_spins += 1;
                    }
                }

                if idle_spins >= max_idle_spins {
                    break;
                }
            }
        }

        webview.close();
        drop(webview);

        for _ in 0..5 {
            self.servo.spin_event_loop();
        }
    }
}

thread_local! {
    static SERVO_INSTANCE: std::cell::RefCell<ServoFuzzerHelper> = std::cell::RefCell::new(ServoFuzzerHelper::new());
}

fuzz_target!(|data: &[u8]| {
    if let Ok(input_str) = std::str::from_utf8(data) {
        SERVO_INSTANCE.with(|helper_cell| {
            helper_cell.borrow_mut().process_html(input_str);
        });
    }
});
