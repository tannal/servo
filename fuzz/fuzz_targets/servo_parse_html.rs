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

// 1. 导入 embedder_traits 中关于 Resource 的模块
use servo::{submit_resource_reader, resources::{
    Resource, ResourceReaderMethods,
}};

// 2. 实现一个 Mock 的 ResourceReader 供 Fuzz 使用，防止报 No resource reader registered
struct FuzzResourceReader;

impl ResourceReaderMethods for FuzzResourceReader {
    fn read(&self, _res: Resource) -> Vec<u8> {
        // 返回空数据或默认内容，阻止崩溃
        vec![]
    }

    fn sandbox_access_files(&self) -> Vec<PathBuf> {
        vec![]
    }

    fn sandbox_access_files_dirs(&self) -> Vec<PathBuf> {
        vec![]
    }
}

// 3. 静态实例化并在全局作用域（函数体之外）调用 submit_resource_reader!
static FUZZ_RESOURCE_READER: FuzzResourceReader = FuzzResourceReader;
submit_resource_reader!(&FUZZ_RESOURCE_READER);

/// 封装符合 Servo 标准生命周期的 Helper
pub struct ServoFuzzerHelper {
    servo: Servo,
    rendering_context: Rc<SoftwareRenderingContext>,
}

impl Drop for ServoFuzzerHelper {
    fn drop(&mut self) {
        // 驱动事件循环清理剩余任务，依赖 Servo 自带的 Drop 完成析构
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

    /// 驱动事件循环直到 callback 条件满足
    pub fn spin<F>(&self, callback: F)
    where
        F: Fn() -> bool + 'static,
    {
        while callback() {
            std::thread::sleep(Duration::from_millis(1));
            // spin_event_loop 返回 ()，直接驱动一帧事件
            self.servo.spin_event_loop();
        }
    }

    /// 通过 Data URL (data:text/html,...) 加载并解析给定的 HTML 字符串
    pub fn load_html(&self, html_content: &str) {
        // 使用 .clone() 增加 Rc 引用计数，防止 move 出 self.rendering_context
        let webview = WebViewBuilder::new(&self.servo, self.rendering_context.clone()).build();

        // 将输入的 HTML 内容构建为标准的 Data URL
        let data_url_str = format!("data:text/html;charset=utf-8,{}", html_content);

        let url = match Url::parse(&data_url_str) {
            Ok(u) => u,
            Err(_) => return, // 如果生成了非法 URL 则跳过
        };

        // 加载 URL
        webview.load(url);

        // 驱动事件循环直到页面加载与 DOM 解析完成
        let load_webview = webview.clone();
        self.spin(move || load_webview.load_status() != LoadStatus::Complete);
    }

    /// 执行给定 JavaScript 代码并同步等待评估结果
    pub fn eval_js(&self, script: &str) -> Result<JSValue, JavaScriptEvaluationError> {
        let webview = WebViewBuilder::new(&self.servo, self.rendering_context.clone()).build();
        
        // 使用 load_html 初始化 context
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

// 保证 Servo 在当前线程只被初始化一次，跨 iteration 复用进程内实例
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

        // 1. 测试 HTML / DOM 解析器
        helper.load_html(input_str);

        // 2. 测试 JS 脚本执行器（取消注释可开启 JS Fuzz）
        // let _ = helper.eval_js(input_str);
    });
});