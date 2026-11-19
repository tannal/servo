#![no_main]

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use dpi::PhysicalSize;
use libfuzzer_sys::fuzz_target;
use servo::{
    EventLoopWaker, LoadStatus, Preferences, RenderingContext, Servo, ServoBuilder,
    SoftwareRenderingContext, WebView, WebViewBuilder, WebViewDelegate,
};
// 链接 servo-default-resources 以自动注册内置 DefaultResourceReader（适配最新 ResourceBytes，包含默认 UA 样式表等资源）
use servo_default_resources as _;
use url::Url;

const MAX_INPUT_LEN: usize = 16 * 1024;
const DEFAULT_LOAD_TIMEOUT_MS: u64 = 1000;
const NORMAL_DRAIN_TIMEOUT: Duration = Duration::from_millis(20);
const TIMEOUT_DRAIN_TIMEOUT: Duration = Duration::from_millis(80);
const HISTORY_LEN: usize = 4;

static INPUT_SEQ: AtomicU64 = AtomicU64::new(0);
static INPUT_HISTORY: OnceLock<Mutex<VecDeque<(u64, Vec<u8>)>>> = OnceLock::new();
static PANIC_HOOK_INSTALLED: OnceLock<()> = OnceLock::new();

fn history_store() -> &'static Mutex<VecDeque<(u64, Vec<u8>)>> {
    INPUT_HISTORY.get_or_init(|| Mutex::new(VecDeque::with_capacity(HISTORY_LEN)))
}

fn record_input(data: &[u8]) -> u64 {
    let seq = INPUT_SEQ.fetch_add(1, Ordering::Relaxed) + 1;
    if let Ok(mut guard) = history_store().lock() {
        if guard.len() >= HISTORY_LEN {
            guard.pop_front();
        }
        guard.push_back((seq, data.to_vec()));
    }
    seq
}

fn dump_recent_inputs() {
    let Ok(guard) = history_store().try_lock() else {
        eprintln!("[servo_fuzz] Could not lock input history during panic");
        return;
    };
    if guard.is_empty() {
        return;
    }

    let dir = std::env::var_os("SERVO_FUZZ_HISTORY_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::temp_dir().join(format!("servo_fuzz_history_{}", std::process::id()))
        });

    if let Err(err) = std::fs::create_dir_all(&dir) {
        eprintln!("[servo_fuzz] Failed to create history dir {:?}: {err}", dir);
        return;
    }

    eprintln!(
        "[servo_fuzz] Dumping last {} inputs to {:?} (in case of async thread misattribution):",
        guard.len(),
        dir
    );
    for (seq, bytes) in guard.iter() {
        let path = dir.join(format!("input_{seq:08}.html"));
        match std::fs::write(&path, bytes) {
            Ok(()) => eprintln!("  - seq={seq} len={} -> {}", bytes.len(), path.display()),
            Err(err) => eprintln!("  - seq={seq} write failed: {err}"),
        }
    }
}

fn install_panic_hook() {
    PANIC_HOOK_INSTALLED.get_or_init(|| {
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            // 任意线程（含 WRRenderBackend / Script / Layout）panic 时导出最近 N 个输入；
            // 注意不在这里强制 abort，避免误杀 Servo 内部合法的 catch_unwind（如 line_breaker / resvg）
            dump_recent_inputs();
            prev(info);
        }));
    });
}

#[derive(Clone)]
struct FuzzWaker(Arc<AtomicBool>);

impl EventLoopWaker for FuzzWaker {
    fn clone_box(&self) -> Box<dyn EventLoopWaker> {
        Box::new(self.clone())
    }
    fn wake(&self) {
        self.0.store(true, Ordering::Release);
    }
}

/// 每个 WebView 实例独享一个 delegate，绝不跨迭代复用，
/// 因此不会收到上一轮 WebView 的残留加载/帧事件。
struct FuzzWebViewDelegate {
    completed: Cell<bool>,
    frame_ready: Cell<bool>,
    closed: Cell<bool>,
    enable_paint: bool,
}

impl FuzzWebViewDelegate {
    fn new(enable_paint: bool) -> Self {
        Self {
            completed: Cell::new(false),
            frame_ready: Cell::new(false),
            closed: Cell::new(false),
            enable_paint,
        }
    }

    fn is_done(&self) -> bool {
        self.closed.get() || (self.completed.get() && self.frame_ready.get())
    }
}

impl WebViewDelegate for FuzzWebViewDelegate {
    fn notify_load_status_changed(&self, _webview: WebView, status: LoadStatus) {
        match status {
            LoadStatus::Started => {
                // 若页面通过 JS / meta refresh 触发二次跳转，重置完成标记
                self.completed.set(false);
                self.frame_ready.set(false);
            },
            LoadStatus::HeadParsed => {},
            LoadStatus::Complete | LoadStatus::Stopped => {
                self.completed.set(true);
            },
        }
    }

    fn notify_new_frame_ready(&self, webview: WebView) {
        self.frame_ready.set(true);
        // 默认跳过慢速 CPU 软件光栅化，仅覆盖 Parse -> DOM -> Stylo -> Layout -> DisplayList -> WR Scene；
        // 如需同时测 WebRender 软件绘制，可设置环境变量 SERVO_FUZZ_PAINT=1
        if self.enable_paint {
            webview.paint();
        }
    }

    fn notify_closed(&self, _webview: WebView) {
        self.closed.set(true);
    }

    fn notify_crashed(&self, _webview: WebView, reason: String, backtrace: Option<String>) {
        // 把 Script/Layout 线程被 Constellation 捕获的 panic 显式抛给 libFuzzer
        panic!(
            "Servo WebView crashed: {reason}\n{}",
            backtrace.unwrap_or_default()
        );
    }
}

pub struct ServoFuzzerHelper {
    servo: Servo,
    // 统一存为 Rc<dyn RenderingContext>，确保 Paint::register_rendering_context 通过 Rc::ptr_eq 复用同一个 Painter
    rendering_context: Rc<dyn RenderingContext>,
    woken: Arc<AtomicBool>,
    enable_paint: bool,
    load_timeout: Duration,
}

impl ServoFuzzerHelper {
    pub fn new() -> Self {
        install_panic_hook();

        let rendering_context: Rc<dyn RenderingContext> = Rc::new(
            SoftwareRenderingContext::new(PhysicalSize {
                width: 800,
                height: 600,
            })
            .expect("Could not create SoftwareRenderingContext"),
        );
        assert!(rendering_context.make_current().is_ok());

        let woken = Arc::new(AtomicBool::new(false));

        // 固定单布局线程，消除 Rayon 多线程调度带来的覆盖率噪声；
        // 清空代理配置，避免宿主代理环境变量干扰
        let mut preferences = Preferences::default();
        preferences.layout_threads = 1;
        preferences.network_http_proxy_uri = String::new();
        preferences.network_https_proxy_uri = String::new();

        let servo = ServoBuilder::default()
            .preferences(preferences)
            .event_loop_waker(Box::new(FuzzWaker(woken.clone())))
            .build();

        let enable_paint = std::env::var_os("SERVO_FUZZ_PAINT").is_some();
        let timeout_ms = std::env::var("SERVO_FUZZ_TIMEOUT_MS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(DEFAULT_LOAD_TIMEOUT_MS);

        Self {
            servo,
            rendering_context,
            woken,
            enable_paint,
            load_timeout: Duration::from_millis(timeout_ms),
        }
    }

    fn drain_event_loop(&self, max_duration: Duration) {
        let deadline = Instant::now() + max_duration;
        let mut idle_rounds = 0u8;
        while Instant::now() < deadline {
            self.servo.spin_event_loop();
            if self.woken.swap(false, Ordering::AcqRel) {
                idle_rounds = 0;
            } else {
                idle_rounds += 1;
                if idle_rounds >= 3 {
                    break;
                }
                std::thread::sleep(Duration::from_micros(200));
            }
        }
    }

    pub fn load_html(&self, data: &[u8]) {
        if data.is_empty() || data.len() > MAX_INPUT_LEN {
            return;
        }

        // 用 base64 编码 data URL，支持任意字节且避免 '#', '%', '\0' 截断 HTML/CSS
        let url_str = format!("data:text/html;charset=utf-8;base64,{}", base64_encode(data));
        let Ok(url) = Url::parse(&url_str) else {
            return;
        };

        let _seq = record_input(data);

        // 每次迭代使用独立 WebView + 直接通过 .url(url) 加载：
        // 1. 避免 DOM/CSS/Layout/滚动状态跨迭代残留
        // 2. 跳过默认的 about:blank 初始导航，彻底消除 about:blank 与 data URL 的完成事件竞态
        let delegate = Rc::new(FuzzWebViewDelegate::new(self.enable_paint));
        self.woken.store(false, Ordering::Release);

        let webview = WebViewBuilder::new(&self.servo, self.rendering_context.clone())
            .delegate(delegate.clone())
            .url(url)
            .build();

        let deadline = Instant::now() + self.load_timeout;
        while !delegate.is_done() && Instant::now() < deadline {
            self.servo.spin_event_loop();
            if !self.woken.swap(false, Ordering::AcqRel) {
                std::thread::sleep(Duration::from_micros(200));
            }
        }

        let timed_out = !delegate.is_done();

        // Drop 触发 WebViewInner::drop -> 发送 CloseWebView 并从 Paint/WebRender 移除该视图
        drop(webview);

        // 排空关闭/清理事件，尽量防止上一轮后台任务拖入下一次迭代
        self.drain_event_loop(if timed_out {
            TIMEOUT_DRAIN_TIMEOUT
        } else {
            NORMAL_DRAIN_TIMEOUT
        });
    }
}

fn base64_encode(input: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity((input.len() + 2) / 3 * 4);
    for chunk in input.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(TABLE[((n >> 18) & 0x3f) as usize] as char);
        out.push(TABLE[((n >> 12) & 0x3f) as usize] as char);
        out.push(if chunk.len() > 1 {
            TABLE[((n >> 6) & 0x3f) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[(n & 0x3f) as usize] as char
        } else {
            '='
        });
    }
    out
}

thread_local! {
    // Servo 内部 opts::initialize_options 使用全局 OnceLock，同进程只能初始化一次，
    // 因此进程内复用单例 Servo，靠「每轮独立 WebView + drop 排空 + libFuzzer -fork=N」做隔离。
    static SERVO_INSTANCE: RefCell<ServoFuzzerHelper> = RefCell::new(ServoFuzzerHelper::new());
}

fuzz_target!(|data: &[u8]| {
    SERVO_INSTANCE.with(|helper| {
        helper.borrow().load_html(data);
    });
});
