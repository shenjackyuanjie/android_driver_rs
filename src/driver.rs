//! 异步 Android Driver、Agent 生命周期与业务能力。

use crate::adb::AdbRunner;
use crate::agent::{self, AgentProfile, AgentSource, REMOTE_DIR, REMOTE_JAR};
use crate::rpc::{RpcClient, ping};
use crate::{
    ActivityName, AdbConfig, AndroidKeyCode, AppIdentifier, DeviceDescriptor, DeviceInfo,
    DeviceSelector, DisplaySize, DriverError, Element, Point, Position, Result, ScreenState,
    ScreenshotMethod, Selector, UiNode, XPathElement,
};
use base64::Engine;
use serde_json::{Value, json};
use std::ffi::OsString;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio::sync::Mutex;
use tokio::time::{Instant, sleep};
use tracing::{debug, info, trace, warn};

const DEFAULT_AGENT_PORT: u16 = 9008;
const OWNED_AGENT_PORTS: std::ops::RangeInclusive<u16> = 19008..=19017;

/// Driver 运行时配置。
#[derive(Clone, Debug)]
pub struct DriverConfig {
    pub rpc_timeout: Duration,
    pub max_json_size: usize,
    pub wait_interval: Duration,
    pub ui_automation_conflict_policy: UiAutomationConflictPolicy,
}

impl Default for DriverConfig {
    fn default() -> Self {
        Self {
            rpc_timeout: Duration::from_secs(20),
            max_json_size: 8 * 1024 * 1024,
            wait_interval: Duration::from_millis(500),
            ui_automation_conflict_policy: UiAutomationConflictPolicy::Fail,
        }
    }
}

/// 设备已有 UiAutomation 服务时的处理策略。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum UiAutomationConflictPolicy {
    /// 不触碰外部自动化进程，返回可诊断的启动错误。
    #[default]
    Fail,
    /// 清理精确识别为 `uiautomator` 的外部进程后再启动 Agent。
    KillStaleProcesses,
}

/// 异步 Driver Builder。
#[derive(Clone, Debug, Default)]
pub struct AndroidDriverBuilder {
    selector: DeviceSelector,
    adb: AdbConfig,
    source: AgentSource,
    config: DriverConfig,
}

impl AndroidDriverBuilder {
    pub fn device(mut self, selector: DeviceSelector) -> Self {
        self.selector = selector;
        self
    }
    pub fn adb_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.adb = self.adb.with_path(path);
        self
    }
    pub fn adb_server(mut self, host: impl Into<String>, port: u16) -> Self {
        self.adb = self.adb.with_server(host, port);
        self
    }
    pub fn adb_config(mut self, config: AdbConfig) -> Self {
        self.adb = config;
        self
    }
    pub fn agent_source(mut self, source: AgentSource) -> Self {
        self.source = source;
        self
    }
    pub fn driver_config(mut self, config: DriverConfig) -> Self {
        self.config = config;
        self
    }
    pub fn ui_automation_conflict_policy(mut self, policy: UiAutomationConflictPolicy) -> Self {
        self.config.ui_automation_conflict_policy = policy;
        self
    }

    pub async fn connect(self) -> Result<AndroidDriver> {
        info!(target: "android_driver_rs::driver", "开始连接设备");
        let discovery = AdbRunner::new(self.adb)?;
        let descriptor = discovery.select(&self.selector).await?;
        let adb = discovery.with_serial(descriptor.serial);
        let files = agent::materialize(&self.source).await?;
        deploy_jar(&adb, &files.jar).await?;
        let session = establish_session(&adb, &self.config).await?;
        info!(target: "android_driver_rs::driver", "设备连接成功");
        Ok(AndroidDriver {
            inner: Arc::new(DriverInner {
                adb,
                source: self.source,
                profile: AgentProfile::default(),
                config: self.config,
                generation: AtomicU64::new(1),
                state: Mutex::new(SessionState {
                    rpc: Some(session.rpc),
                    forwards: vec![session.forward],
                    owned_agent: session.owned_agent,
                    generation: 1,
                    closed: false,
                    active_ime: None,
                }),
                input_apk: files.apk,
            }),
        })
    }
}

/// 一个 Android 设备上的异步 Driver。
#[derive(Clone)]
pub struct AndroidDriver {
    pub(crate) inner: Arc<DriverInner>,
}

impl std::fmt::Debug for AndroidDriver {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AndroidDriver")
            .field("agent", &self.inner.profile)
            .field("generation", &self.generation())
            .finish_non_exhaustive()
    }
}

pub(crate) struct DriverInner {
    adb: AdbRunner,
    source: AgentSource,
    profile: AgentProfile,
    config: DriverConfig,
    generation: AtomicU64,
    state: Mutex<SessionState>,
    input_apk: Option<PathBuf>,
}

struct SessionState {
    rpc: Option<RpcClient>,
    forwards: Vec<OwnedForward>,
    owned_agent: Option<OwnedAgent>,
    generation: u64,
    closed: bool,
    active_ime: Option<String>,
}

#[derive(Clone, Debug)]
struct OwnedForward {
    local_port: u16,
    remote_port: u16,
}
struct OwnedAgent {
    pid: u32,
    port: u16,
    host_process: tokio::process::Child,
}
struct EstablishedSession {
    rpc: RpcClient,
    forward: OwnedForward,
    owned_agent: Option<OwnedAgent>,
}

impl AndroidDriver {
    pub fn builder() -> AndroidDriverBuilder {
        AndroidDriverBuilder::default()
    }
    pub async fn discover_devices(config: AdbConfig) -> Result<Vec<DeviceDescriptor>> {
        AdbRunner::new(config)?.discover().await
    }
    pub fn agent_profile(&self) -> &AgentProfile {
        &self.inner.profile
    }
    pub fn generation(&self) -> u64 {
        self.inner.generation.load(Ordering::Acquire)
    }

    /// 调用 u2 HTTP JSON-RPC。调用不会自动重放。
    pub async fn call_json_rpc(&self, method: &str, params: Value) -> Result<Value> {
        trace!(target: "android_driver_rs::driver", method, "调用 RPC");
        let rpc = {
            let state = self.inner.state.lock().await;
            if state.closed {
                return Err(DriverError::DriverClosed);
            }
            state.rpc.clone().ok_or(DriverError::SessionInvalid)?
        };
        rpc.call(method, params).await
    }

    /// 清理旧会话并重新启动/借用 Agent。generation 成功后递增。
    pub async fn recover(&self) -> Result<()> {
        warn!(target: "android_driver_rs::driver", "开始恢复会话");
        let mut state = self.inner.state.lock().await;
        if state.closed {
            return Err(DriverError::DriverClosed);
        }
        if let Some(rpc) = state.rpc.take() {
            rpc.invalidate();
        }
        restore_ime_locked(&self.inner.adb, &mut state).await?;
        cleanup_resources(&self.inner.adb, &mut state).await?;
        let files = agent::materialize(&self.inner.source).await?;
        deploy_jar(&self.inner.adb, &files.jar).await?;
        let session = establish_session(&self.inner.adb, &self.inner.config).await?;
        state.rpc = Some(session.rpc);
        state.forwards.push(session.forward);
        state.owned_agent = session.owned_agent;
        state.generation = state.generation.saturating_add(1);
        self.inner
            .generation
            .store(state.generation, Ordering::Release);
        info!(target: "android_driver_rs::driver", "会话恢复成功");
        Ok(())
    }

    /// 恢复输入法并精确清理自有 Agent 与 forward。可安全重复调用。
    pub async fn close(&self) -> Result<()> {
        debug!(target: "android_driver_rs::driver", "关闭会话");
        let mut state = self.inner.state.lock().await;
        if state.closed {
            return Ok(());
        }
        if let Some(rpc) = state.rpc.take() {
            rpc.invalidate();
        }
        restore_ime_locked(&self.inner.adb, &mut state).await?;
        cleanup_resources(&self.inner.adb, &mut state).await?;
        state.closed = true;
        Ok(())
    }

    pub async fn display_size(&self) -> Result<DisplaySize> {
        let output = self.inner.adb.shell(["wm", "size"]).await?.stdout;
        let result = parse_display_size(&output);
        trace!(target: "android_driver_rs::driver", ?result, "display_size");
        result.ok_or_else(|| DriverError::Protocol("无法解析 wm size".into()))
    }

    pub async fn device_info(&self) -> Result<DeviceInfo> {
        debug!(target: "android_driver_rs::driver", "收集设备信息");
        let manufacturer = self.property("ro.product.manufacturer").await?;
        let model = self.property("ro.product.model").await?;
        let android_version = self.property("ro.build.version.release").await?;
        let sdk_level = self
            .property("ro.build.version.sdk")
            .await?
            .parse()
            .map_err(|_| DriverError::Protocol("SDK 版本无效".into()))?;
        let cpu_abi = self.property("ro.product.cpu.abi").await?;
        Ok(DeviceInfo {
            manufacturer,
            model,
            android_version,
            sdk_level,
            cpu_abi,
            display_size: self.display_size().await?,
        })
    }

    async fn property(&self, name: &str) -> Result<String> {
        Ok(self
            .inner
            .adb
            .shell(["getprop", name])
            .await?
            .stdout
            .trim()
            .to_owned())
    }

    pub async fn screen_state(&self) -> Result<ScreenState> {
        trace!(target: "android_driver_rs::driver", "获取屏幕状态");
        let output = self.inner.adb.shell(["dumpsys", "power"]).await?.stdout;
        if output.contains("mWakefulness=Awake") || output.contains("Display Power: state=ON") {
            Ok(ScreenState::Awake)
        } else if output.contains("mWakefulness=Asleep")
            || output.contains("Display Power: state=OFF")
        {
            Ok(ScreenState::Asleep)
        } else {
            Ok(ScreenState::Unknown("无法识别 dumpsys power 输出".into()))
        }
    }

    pub async fn screen_on(&self) -> Result<()> {
        debug!(target: "android_driver_rs::driver", "点亮屏幕");
        if self.screen_state().await? != ScreenState::Awake {
            self.press_key(AndroidKeyCode::POWER).await?;
        }
        Ok(())
    }
    pub async fn screen_off(&self) -> Result<()> {
        debug!(target: "android_driver_rs::driver", "熄灭屏幕");
        if self.screen_state().await? == ScreenState::Awake {
            self.press_key(AndroidKeyCode::POWER).await?;
        }
        Ok(())
    }
    pub async fn unlock(&self) -> Result<()> {
        debug!(target: "android_driver_rs::driver", "解锁屏幕");
        self.inner
            .adb
            .shell(["wm", "dismiss-keyguard"])
            .await
            .map(|_| ())
    }

    pub async fn press_key(&self, key: impl Into<AndroidKeyCode>) -> Result<()> {
        let code = key.into().0;
        trace!(target: "android_driver_rs::driver", key_code = code, "按键");
        self.inner
            .adb
            .shell(["input".into(), "keyevent".into(), code.to_string()])
            .await
            .map(|_| ())
    }
    pub async fn go_back(&self) -> Result<()> {
        self.press_key(AndroidKeyCode::BACK).await
    }
    pub async fn go_home(&self) -> Result<()> {
        self.press_key(AndroidKeyCode::HOME).await
    }

    pub async fn click(&self, point: Point) -> Result<()> {
        validate_point(point)?;
        trace!(target: "android_driver_rs::driver", x = point.x, y = point.y, "点击");
        self.inner
            .adb
            .shell([
                "input".into(),
                "tap".into(),
                point.x.to_string(),
                point.y.to_string(),
            ])
            .await
            .map(|_| ())
    }
    pub async fn click_position(&self, position: Position) -> Result<()> {
        self.click(position.resolve(self.display_size().await?)?)
            .await
    }
    pub async fn long_click(&self, point: Point, duration_ms: u32) -> Result<()> {
        trace!(target: "android_driver_rs::driver", x = point.x, y = point.y, duration_ms, "长按");
        self.swipe(point, point, duration_ms).await
    }
    pub async fn swipe(&self, from: Point, to: Point, duration_ms: u32) -> Result<()> {
        validate_point(from)?;
        validate_point(to)?;
        trace!(target: "android_driver_rs::driver", from_x = from.x, from_y = from.y, to_x = to.x, to_y = to.y, duration_ms, "滑动");
        self.inner
            .adb
            .shell([
                "input".into(),
                "swipe".into(),
                from.x.to_string(),
                from.y.to_string(),
                to.x.to_string(),
                to.y.to_string(),
                duration_ms.to_string(),
            ])
            .await
            .map(|_| ())
    }
    pub async fn swipe_positions(
        &self,
        from: Position,
        to: Position,
        duration_ms: u32,
    ) -> Result<()> {
        let display = self.display_size().await?;
        self.swipe(from.resolve(display)?, to.resolve(display)?, duration_ms)
            .await
    }

    pub async fn start_app(
        &self,
        package: &AppIdentifier,
        activity: Option<&ActivityName>,
    ) -> Result<()> {
        info!(target: "android_driver_rs::driver", package = %package.as_str(), activity = ?activity.map(ActivityName::as_str), "启动应用");
        let activity = match activity {
            Some(value) => value.as_str().to_owned(),
            None => self.resolve_activity(package).await?,
        };
        let component = if activity.starts_with(package.as_str()) {
            format!("{}/{activity}", package.as_str())
        } else {
            format!("{}/{}", package.as_str(), activity)
        };
        self.inner
            .adb
            .shell(["am", "start", "-W", "-n", &component])
            .await
            .map(|_| ())
    }

    pub async fn resolve_activity(&self, package: &AppIdentifier) -> Result<String> {
        let output = self
            .inner
            .adb
            .shell([
                "cmd",
                "package",
                "resolve-activity",
                "--brief",
                package.as_str(),
            ])
            .await?
            .stdout;
        output
            .lines()
            .map(str::trim)
            .find(|line| line.contains('/'))
            .and_then(|line| {
                line.split_once('/')
                    .map(|(_, activity)| activity.to_owned())
            })
            .ok_or_else(|| DriverError::Protocol("无法解析启动 Activity".into()))
    }

    pub async fn stop_app(&self, package: &AppIdentifier) -> Result<()> {
        debug!(target: "android_driver_rs::driver", package = %package.as_str(), "停止应用");
        self.inner
            .adb
            .shell(["am", "force-stop", package.as_str()])
            .await
            .map(|_| ())
    }

    pub async fn current_app(&self) -> Result<Option<(AppIdentifier, ActivityName)>> {
        trace!(target: "android_driver_rs::driver", "获取当前前台应用");
        let output = self
            .inner
            .adb
            .shell(["dumpsys", "window", "windows"])
            .await?
            .stdout;
        let regex = regex::Regex::new(
            r"(?:mCurrentFocus|mFocusedApp).*? ([A-Za-z0-9_.$]+)/([A-Za-z0-9_.$]+)",
        )
        .map_err(|error| DriverError::Protocol(error.to_string()))?;
        let Some(captures) = regex.captures(&output) else {
            return Ok(None);
        };
        Ok(Some((
            AppIdentifier::new(captures[1].to_owned())?,
            ActivityName::new(captures[2].to_owned())?,
        )))
    }

    /// 对当前获得焦点的控件输入 Unicode 文本，并在所有正常路径恢复原输入法。
    pub async fn input_text(&self, text: &str) -> Result<()> {
        #[cfg(not(feature = "input-method"))]
        return Err(DriverError::InputMethod(
            "未启用 input-method feature".into(),
        ));
        #[cfg(feature = "input-method")]
        {
            let focused = Selector::new().focused(true).value(0);
            if self
                .call_json_rpc("setText", json!([focused, text]))
                .await
                .is_ok_and(|value| value.as_bool() != Some(false))
            {
                return Ok(());
            }
            self.ensure_fast_input_ime().await?;
            let original = self
                .inner
                .adb
                .shell(["settings", "get", "secure", "default_input_method"])
                .await?
                .stdout
                .trim()
                .to_owned();
            {
                let mut state = self.inner.state.lock().await;
                state.active_ime = Some(original.clone());
            }
            let guard = ImeGuard {
                adb: self.inner.adb.clone(),
                original: Some(original),
                state: self.inner.clone(),
            };
            self.inner
                .adb
                .shell(["ime", "enable", "com.github.uiautomator/.AdbKeyboard"])
                .await?;
            self.inner
                .adb
                .shell(["ime", "set", "com.github.uiautomator/.AdbKeyboard"])
                .await?;
            let encoded = base64::engine::general_purpose::STANDARD.encode(text.as_bytes());
            let output = self
                .inner
                .adb
                .shell([
                    "am",
                    "broadcast",
                    "-a",
                    "ADB_KEYBOARD_INPUT_TEXT",
                    "--es",
                    "text",
                    &encoded,
                ])
                .await?
                .stdout;
            if !output.contains("result=-1") {
                return Err(DriverError::InputMethod("辅助输入法广播未成功".into()));
            }
            guard.restore().await
        }
    }

    #[cfg(feature = "input-method")]
    async fn ensure_fast_input_ime(&self) -> Result<()> {
        if self
            .inner
            .adb
            .shell(["pm", "path", "com.github.uiautomator"])
            .await
            .is_ok_and(|value| !value.stdout.trim().is_empty())
        {
            return Ok(());
        }
        let apk = self.inner.input_apk.as_ref().ok_or_else(|| {
            DriverError::InputMethod("AgentSource 目录缺少 app-uiautomator.apk".into())
        })?;
        self.inner
            .adb
            .run_text(
                [
                    OsString::from("install"),
                    OsString::from("-r"),
                    OsString::from("-t"),
                    apk.as_os_str().to_os_string(),
                ],
                self.inner.adb.transfer_timeout(),
            )
            .await
            .map(|_| ())
            .map_err(|error| DriverError::InputMethod(error.to_string()))
    }

    pub async fn screenshot(&self) -> Result<Vec<u8>> {
        debug!(target: "android_driver_rs::driver", "截取屏幕");
        self.screenshot_with_method(ScreenshotMethod::Auto).await
    }
    pub async fn screenshot_with_method(&self, method: ScreenshotMethod) -> Result<Vec<u8>> {
        trace!(target: "android_driver_rs::driver", ?method, "截图（指定方式）");
        match method {
            ScreenshotMethod::AdbScreencap => self.adb_screenshot().await,
            ScreenshotMethod::U2 => self.u2_screenshot().await,
            ScreenshotMethod::Auto => match self.adb_screenshot().await {
                Ok(bytes) => Ok(bytes),
                Err(error) => {
                    debug!(target: "android_driver_rs::driver", %error, "ADB 截图失败，回退 u2");
                    self.u2_screenshot().await
                }
            },
        }
    }
    async fn adb_screenshot(&self) -> Result<Vec<u8>> {
        let output = self
            .inner
            .adb
            .run_bytes(
                ["exec-out", "screencap", "-p"],
                self.inner.adb.transfer_timeout(),
            )
            .await?;
        validate_image(output.stdout)
    }
    async fn u2_screenshot(&self) -> Result<Vec<u8>> {
        let value = self.call_json_rpc("takeScreenshot", json!([1, 80])).await?;
        let encoded = value
            .as_str()
            .or_else(|| value.get("data").and_then(Value::as_str))
            .ok_or_else(|| DriverError::InvalidScreenshot("RPC 未返回 base64 字符串".into()))?;
        let encoded = encoded
            .trim_start_matches("data:image/png;base64,")
            .trim_start_matches("data:image/jpeg;base64,");
        let compact = encoded
            .bytes()
            .filter(|value| !value.is_ascii_whitespace())
            .collect::<Vec<_>>();
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(compact)
            .map_err(|error| DriverError::InvalidScreenshot(format!("base64 解码失败：{error}")))?;
        validate_image(bytes)
    }
    pub async fn screenshot_to(&self, path: impl AsRef<Path>) -> Result<()> {
        tokio::fs::write(path, self.screenshot().await?)
            .await
            .map_err(DriverError::Io)
    }

    pub async fn ui_tree_xml(&self) -> Result<String> {
        trace!(target: "android_driver_rs::driver", "获取 UI 树 XML");
        let value = self
            .call_json_rpc("dumpWindowHierarchy", json!([false, 50]))
            .await?;
        value
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| DriverError::Protocol("UI 树响应不是 XML 字符串".into()))
    }
    pub async fn ui_tree(&self) -> Result<UiNode> {
        UiNode::parse(&self.ui_tree_xml().await?)
    }

    pub async fn find(&self, selector: &Selector) -> Result<Option<Element>> {
        trace!(target: "android_driver_rs::driver", ?selector, "查找元素");
        let exists = self
            .call_json_rpc("exist", json!([selector.value(0)]))
            .await?
            .as_bool()
            .unwrap_or(false);
        Ok(exists.then(|| Element {
            driver: self.clone(),
            selector: selector.clone(),
            index: 0,
            generation: self.generation(),
        }))
    }
    pub async fn find_all(&self, selector: &Selector) -> Result<Vec<Element>> {
        let count = self.count(selector).await?;
        Ok((0..count)
            .map(|index| Element {
                driver: self.clone(),
                selector: selector.clone(),
                index,
                generation: self.generation(),
            })
            .collect())
    }
    pub async fn exists(&self, selector: &Selector) -> Result<bool> {
        Ok(self.find(selector).await?.is_some())
    }
    pub async fn count(&self, selector: &Selector) -> Result<usize> {
        self.call_json_rpc("count", json!([selector.value(0)]))
            .await?
            .as_u64()
            .and_then(|value| usize::try_from(value).ok())
            .ok_or_else(|| DriverError::Protocol("Selector count 响应无效".into()))
    }
    pub async fn click_if_exists(&self, selector: &Selector) -> Result<bool> {
        if let Some(element) = self.find(selector).await? {
            element.click().await?;
            Ok(true)
        } else {
            Ok(false)
        }
    }
    pub async fn wait_for(&self, selector: &Selector, timeout: Duration) -> Result<Element> {
        trace!(target: "android_driver_rs::driver", ?selector, ?timeout, "等待元素");
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(element) = self.find(selector).await? {
                return Ok(element);
            }
            if Instant::now() >= deadline {
                return Err(DriverError::ElementNotFound);
            }
            sleep(self.inner.config.wait_interval).await;
        }
    }
    pub async fn wait_until_gone(&self, selector: &Selector, timeout: Duration) -> Result<bool> {
        let deadline = Instant::now() + timeout;
        loop {
            if !self.exists(selector).await? {
                return Ok(true);
            }
            if Instant::now() >= deadline {
                return Ok(false);
            }
            sleep(self.inner.config.wait_interval).await;
        }
    }
    pub async fn wait_until<F, Fut>(&self, timeout: Duration, mut condition: F) -> Result<bool>
    where
        F: FnMut() -> Fut,
        Fut: Future<Output = Result<bool>>,
    {
        trace!(target: "android_driver_rs::driver", ?timeout, "等待条件");
        let deadline = Instant::now() + timeout;
        loop {
            if condition().await? {
                return Ok(true);
            }
            if Instant::now() >= deadline {
                return Ok(false);
            }
            sleep(self.inner.config.wait_interval).await;
        }
    }

    pub async fn xpath_all(&self, expression: &str) -> Result<Vec<XPathElement>> {
        crate::xpath::evaluate(self.clone(), &self.ui_tree().await?, expression)
    }
    pub async fn xpath_optional(&self, expression: &str) -> Result<Option<XPathElement>> {
        Ok(self.xpath_all(expression).await?.into_iter().next())
    }
    pub async fn xpath(&self, expression: &str) -> Result<XPathElement> {
        self.xpath_optional(expression)
            .await?
            .ok_or(DriverError::XPathNotFound)
    }
    pub async fn xpath_exists(&self, expression: &str) -> Result<bool> {
        Ok(self.xpath_optional(expression).await?.is_some())
    }
    pub async fn wait_for_xpath(
        &self,
        expression: &str,
        timeout: Duration,
    ) -> Result<XPathElement> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(element) = self.xpath_optional(expression).await? {
                return Ok(element);
            }
            if Instant::now() >= deadline {
                return Err(DriverError::XPathNotFound);
            }
            sleep(self.inner.config.wait_interval).await;
        }
    }
}

async fn deploy_jar(adb: &AdbRunner, local: &Path) -> Result<()> {
    debug!(
        target: "android_driver_rs::driver",
        local = %local.display(),
        expected_size = agent::JAR_SIZE,
        expected_sha256 = agent::JAR_SHA256,
        "部署 Agent JAR"
    );
    adb.shell(["mkdir", "-p", REMOTE_DIR]).await?;
    match inspect_remote_file(adb, REMOTE_JAR, "部署前").await {
        Ok(current) if remote_file_matches(&current) => {
            debug!(target: "android_driver_rs::driver", "Agent JAR 已就绪，跳过部署");
            return Ok(());
        }
        Ok(_) => {
            debug!(target: "android_driver_rs::driver", "设备端现有 Agent JAR 不匹配，重新部署");
        }
        Err(error) => {
            debug!(target: "android_driver_rs::driver", error = %error, "无法检查设备端现有 Agent JAR，继续部署");
        }
    }

    let temporary = format!("{REMOTE_JAR}.{}.tmp", std::process::id());
    let push = adb
        .run_text(
            [
                OsString::from("push"),
                local.as_os_str().to_os_string(),
                OsString::from(&temporary),
            ],
            adb.transfer_timeout(),
        )
        .await?;
    debug!(
        target: "android_driver_rs::driver",
        remote = temporary,
        stdout = ?push.stdout.trim(),
        stderr = ?push.stderr.trim(),
        "Agent JAR 推送完成"
    );

    adb.shell(["chmod", "0644", &temporary]).await?;
    let pushed = inspect_remote_file(adb, &temporary, "push 后").await?;
    verify_remote_digest(&pushed, "临时")?;

    adb.shell(["mv", &temporary, REMOTE_JAR]).await?;
    let published = inspect_remote_file(adb, REMOTE_JAR, "mv 后").await?;
    verify_remote_digest(&published, "正式")?;
    info!(target: "android_driver_rs::driver", "Agent JAR 部署完成");
    Ok(())
}

struct RemoteFileInfo {
    digest: Option<String>,
    size: Option<u64>,
    exists: bool,
}

async fn inspect_remote_file(
    adb: &AdbRunner,
    remote: &str,
    stage: &'static str,
) -> Result<RemoteFileInfo> {
    let (digest, sha256_stdout, sha256_stderr) = match adb.shell(["sha256sum", remote]).await {
        Ok(output) => (
            parse_sha256_output(&output.stdout).map(str::to_owned),
            output.stdout,
            output.stderr,
        ),
        Err(error) => {
            debug!(
                target: "android_driver_rs::driver",
                stage,
                remote,
                error = %error,
                "设备端不支持 sha256sum，降级使用文件大小校验"
            );
            (None, String::new(), String::new())
        }
    };
    let size = match adb.shell(["stat", "-c", "%s", remote]).await {
        Ok(output) => output
            .stdout
            .split_whitespace()
            .next()
            .and_then(|value| value.parse().ok()),
        Err(error) => {
            debug!(
                target: "android_driver_rs::driver",
                stage,
                remote,
                error = %error,
                "无法获取设备端 Agent 文件大小"
            );
            match adb.shell(["wc", "-c", remote]).await {
                Ok(output) => output
                    .stdout
                    .split_whitespace()
                    .next()
                    .and_then(|value| value.parse().ok()),
                Err(_) => None,
            }
        }
    };
    let exists =
        digest.is_some() || size.is_some() || adb.shell(["test", "-f", remote]).await.is_ok();
    debug!(
        target: "android_driver_rs::driver",
        stage,
        remote,
        expected_sha256 = agent::JAR_SHA256,
        actual_sha256 = digest.as_deref().unwrap_or("<无法解析>"),
        size = ?size,
        sha256_stdout = ?sha256_stdout,
        sha256_stderr = ?sha256_stderr,
        exists,
        "设备端 Agent 文件信息"
    );
    Ok(RemoteFileInfo {
        digest,
        size,
        exists,
    })
}

fn parse_sha256_output(output: &str) -> Option<&str> {
    output
        .split_whitespace()
        .find(|value| value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()))
}

fn remote_digest_matches(info: &RemoteFileInfo) -> bool {
    info.digest
        .as_deref()
        .is_some_and(|value| value.eq_ignore_ascii_case(agent::JAR_SHA256))
}

fn remote_file_matches(info: &RemoteFileInfo) -> bool {
    remote_digest_matches(info)
        || (info.exists && info.digest.is_none() && info.size == Some(agent::JAR_SIZE))
}

fn verify_remote_digest(info: &RemoteFileInfo, kind: &str) -> Result<()> {
    if remote_digest_matches(info) {
        return Ok(());
    }
    if remote_file_matches(info) {
        warn!(
            target: "android_driver_rs::driver",
            kind,
            expected_size = agent::JAR_SIZE,
            "设备端不支持 SHA-256，已降级为文件大小校验"
        );
        return Ok(());
    }
    let actual = info.digest.as_deref().unwrap_or("<无法解析>");
    warn!(
        target: "android_driver_rs::driver",
        kind,
        expected_sha256 = agent::JAR_SHA256,
        actual_sha256 = actual,
        size = ?info.size,
        "设备端 Agent JAR 校验失败"
    );
    Err(DriverError::AgentVerification(format!(
        "设备端{kind} u2.jar 校验失败（expected_sha256={}, actual={actual}, size={:?}, exists={}）",
        agent::JAR_SHA256,
        info.size,
        info.exists
    )))
}

async fn establish_session(adb: &AdbRunner, config: &DriverConfig) -> Result<EstablishedSession> {
    info!(target: "android_driver_rs::driver", "建立 RPC 会话");
    let compatible_process = compatible_agent_process(adb, DEFAULT_AGENT_PORT).await;
    let borrowed_forward = create_forward(adb, DEFAULT_AGENT_PORT).await?;
    if compatible_process && ping(borrowed_forward.local_port, adb.agent_timeout()).await {
        return Ok(EstablishedSession {
            rpc: RpcClient::new(
                borrowed_forward.local_port,
                config.rpc_timeout,
                config.max_json_size,
            ),
            forward: borrowed_forward,
            owned_agent: None,
        });
    }
    remove_forward(adb, &borrowed_forward).await?;
    resolve_uiautomation_conflict(adb, config.ui_automation_conflict_policy).await?;

    let mut startup_errors = Vec::new();
    for port in OWNED_AGENT_PORTS {
        if remote_port_in_use(adb, port).await {
            continue;
        }
        let mut owned_agent = match start_agent(adb, port).await {
            Ok(value) => value,
            Err(error) => {
                warn!(
                    target: "android_driver_rs::driver",
                    remote_port = port,
                    error = %error,
                    "Agent 启动尝试失败"
                );
                if is_uiautomation_conflict_message(&error.to_string()) {
                    return Err(error);
                }
                startup_errors.push(format!("tcp:{port}: {error}"));
                continue;
            }
        };
        let forward = match create_forward(adb, port).await {
            Ok(value) => value,
            Err(error) => {
                let _ = stop_owned_agent(adb, &mut owned_agent).await;
                return Err(error);
            }
        };
        let deadline = Instant::now() + adb.agent_timeout();
        loop {
            if ping(forward.local_port, Duration::from_millis(500)).await {
                return Ok(EstablishedSession {
                    rpc: RpcClient::new(
                        forward.local_port,
                        config.rpc_timeout,
                        config.max_json_size,
                    ),
                    forward,
                    owned_agent: Some(owned_agent),
                });
            }
            if Instant::now() >= deadline {
                break;
            }
            sleep(Duration::from_millis(200)).await;
        }
        let _ = remove_forward(adb, &forward).await;
        let _ = stop_owned_agent(adb, &mut owned_agent).await;
    }
    let detail = if startup_errors.is_empty() {
        "候选端口均已被占用".to_owned()
    } else {
        startup_errors.join("；")
    };
    Err(DriverError::AgentStartup(format!(
        "9008 不可借用，且 19008..=19017 均无法启动 Agent：{detail}"
    )))
}

async fn start_agent(adb: &AdbRunner, port: u16) -> Result<OwnedAgent> {
    debug!(target: "android_driver_rs::driver", remote_port = port, "启动自有 Agent");
    let log = agent_log_path(port);
    let command =
        format!("CLASSPATH={REMOTE_JAR} app_process / com.wetest.uia2.Main -p {port} > {log} 2>&1");
    let mut host_process = adb.spawn_long_running([
        "shell".to_owned(),
        "sh".to_owned(),
        "-c".to_owned(),
        command,
    ])?;
    let deadline = Instant::now() + adb.agent_timeout();
    loop {
        if let Some(pid) = agent_pid(adb, port).await {
            let _ = adb.shell(["rm", "-f", &log]).await;
            return Ok(OwnedAgent {
                pid,
                port,
                host_process,
            });
        }
        if let Some(status) = host_process.try_wait().map_err(DriverError::AdbSpawn)? {
            return Err(agent_startup_failure(
                adb,
                port,
                &log,
                format!("Agent 子进程在监听端口前退出（status={status:?}）"),
            )
            .await);
        }
        if Instant::now() >= deadline {
            let _ = host_process.kill().await;
            return Err(agent_startup_failure(adb, port, &log, "等待 Agent PID 超时").await);
        }
        sleep(Duration::from_millis(100)).await;
    }
}

fn agent_log_path(port: u16) -> String {
    format!("{REMOTE_DIR}/agent-{port}.log")
}

async fn agent_startup_failure(
    adb: &AdbRunner,
    port: u16,
    log: &str,
    reason: impl Into<String>,
) -> DriverError {
    let diagnostic = adb
        .shell(["cat", log])
        .await
        .map(|output| output.stdout)
        .unwrap_or_default();
    let _ = adb.shell(["rm", "-f", log]).await;
    let diagnostic = truncate_agent_diagnostic(&diagnostic);
    let reason = reason.into();
    let message = if is_uiautomation_conflict_message(&diagnostic) {
        format!(
            "UiAutomationService already registered：设备上的 UiAutomation 被其他进程占用（port={port}）；{diagnostic}"
        )
    } else if diagnostic.is_empty() {
        format!("{reason}（port={port}；设备端没有返回启动输出）")
    } else {
        format!("{reason}（port={port}；设备端输出：{diagnostic}）")
    };
    DriverError::AgentStartup(message)
}

fn truncate_agent_diagnostic(value: &str) -> String {
    let value = value.trim();
    if value.is_empty() {
        return String::new();
    }
    const LIMIT: usize = 8192;
    if value.chars().count() <= LIMIT {
        value.to_owned()
    } else {
        format!(
            "{}...[truncated]",
            value.chars().take(LIMIT).collect::<String>()
        )
    }
}

fn is_uiautomation_conflict_message(value: &str) -> bool {
    let normalized = value.to_ascii_lowercase();
    normalized.contains("uiautomationservice")
        && (normalized.contains("already registered")
            || normalized.contains("already_registered")
            || normalized.contains("已被"))
}

async fn resolve_uiautomation_conflict(
    adb: &AdbRunner,
    policy: UiAutomationConflictPolicy,
) -> Result<()> {
    let pids = uiautomator_pids(adb).await;
    if pids.is_empty() {
        return Ok(());
    }
    let list = pids
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(", ");
    match policy {
        UiAutomationConflictPolicy::Fail => Err(DriverError::AgentStartup(format!(
            "设备上的 UiAutomation 已被外部 uiautomator 进程占用（PID: {list}）；\
             请停止该进程后重试，或显式启用 KillStaleProcesses 策略"
        ))),
        UiAutomationConflictPolicy::KillStaleProcesses => {
            for pid in &pids {
                let pid = pid.to_string();
                adb.shell(["kill", &pid]).await.map_err(|error| {
                    DriverError::AgentStartup(format!(
                        "无法清理 uiautomator 进程 PID {pid}：{error}"
                    ))
                })?;
            }
            let deadline = Instant::now() + Duration::from_secs(2);
            loop {
                if uiautomator_pids(adb).await.is_empty() {
                    return Ok(());
                }
                if Instant::now() >= deadline {
                    return Err(DriverError::AgentStartup(format!(
                        "已请求清理 uiautomator 进程，但 PID {list} 仍在运行"
                    )));
                }
                sleep(Duration::from_millis(100)).await;
            }
        }
    }
}

async fn stop_owned_agent(adb: &AdbRunner, agent: &mut OwnedAgent) -> Result<()> {
    debug!(target: "android_driver_rs::driver", remote_port = agent.port, "停止自有 Agent");
    let pid = agent.pid.to_string();
    let result = adb
        .shell(["kill", &pid])
        .await
        .or_else(|error| match error {
            DriverError::AdbCommand { .. } => Ok(crate::CommandOutput {
                stdout: String::new(),
                stderr: String::new(),
                status: 0,
            }),
            value => Err(value),
        })
        .map(|_| ());
    let _ = agent.host_process.kill().await;
    result
}

async fn uiautomator_pids(adb: &AdbRunner) -> Vec<u32> {
    for output in [
        adb.shell(["ps", "-A", "-o", "PID,ARGS"]).await.ok(),
        adb.shell(["ps", "-A"]).await.ok(),
        adb.shell(["ps"]).await.ok(),
    ]
    .into_iter()
    .flatten()
    {
        let pids = output
            .stdout
            .lines()
            .filter_map(parse_uiautomator_pid_line)
            .collect::<Vec<_>>();
        if !pids.is_empty() {
            return pids;
        }
    }

    let proc_listing = r#"for path in /proc/[0-9]*/cmdline; do pid=${path#/proc/}; pid=${pid%/cmdline}; cmdline=$(tr '\0' ' ' < "$path" 2>/dev/null); case "$cmdline" in uiautomator|uiautomator\ *|*/uiautomator|*/uiautomator\ *) echo "$pid $cmdline";; esac; done"#;
    adb.shell(["sh", "-c", proc_listing])
        .await
        .map(|output| {
            output
                .stdout
                .lines()
                .filter_map(parse_uiautomator_pid_line)
                .collect()
        })
        .unwrap_or_default()
}

fn parse_uiautomator_pid_line(line: &str) -> Option<u32> {
    let fields = line.split_whitespace().collect::<Vec<_>>();
    let pid_index = fields
        .iter()
        .position(|field| field.parse::<u32>().is_ok())?;
    let command = &fields[pid_index + 1..];
    if command
        .iter()
        .any(|field| field.contains("com.wetest.uia2.Main"))
    {
        return None;
    }
    command
        .iter()
        .any(|field| {
            *field == "uiautomator"
                || field.ends_with("/uiautomator")
                || field.starts_with("uiautomator:")
        })
        .then(|| fields[pid_index].parse().ok())
        .flatten()
}

async fn agent_pid(adb: &AdbRunner, port: u16) -> Option<u32> {
    let port = port.to_string();
    if let Ok(output) = adb.shell(["ps", "-A", "-o", "PID,ARGS"]).await
        && let Some(pid) = output
            .stdout
            .lines()
            .find_map(|line| parse_agent_pid_line(line, &port))
    {
        return Some(pid);
    }
    if let Ok(output) = adb.shell(["ps", "-A"]).await
        && let Some(pid) = output
            .stdout
            .lines()
            .find_map(|line| parse_agent_pid_line(line, &port))
    {
        return Some(pid);
    }
    if let Ok(output) = adb.shell(["ps"]).await
        && let Some(pid) = output
            .stdout
            .lines()
            .find_map(|line| parse_agent_pid_line(line, &port))
    {
        return Some(pid);
    }

    let proc_listing = r#"for path in /proc/[0-9]*/cmdline; do pid=${path#/proc/}; pid=${pid%/cmdline}; cmdline=$(tr '\0' ' ' < "$path" 2>/dev/null); case "$cmdline" in *com.wetest.uia2.Main*) echo "$pid $cmdline";; esac; done"#;
    adb.shell(["sh", "-c", proc_listing])
        .await
        .ok()?
        .stdout
        .lines()
        .find_map(|line| parse_agent_pid_line(line, &port))
}

fn parse_agent_pid_line(line: &str, port: &str) -> Option<u32> {
    let fields = line.split_whitespace().collect::<Vec<_>>();
    let command_start = fields
        .iter()
        .position(|field| field.contains("com.wetest.uia2.Main"))?;
    let command = &fields[command_start..];
    let has_port = command
        .windows(2)
        .any(|pair| pair[0] == "-p" && pair[1] == port)
        || command.contains(&port);
    if !has_port {
        return None;
    }
    fields[..command_start]
        .iter()
        .find_map(|field| field.parse::<u32>().ok())
}

async fn remote_port_in_use(adb: &AdbRunner, port: u16) -> bool {
    adb.shell(["cat", "/proc/net/tcp", "/proc/net/tcp6"])
        .await
        .map(|value| {
            value
                .stdout
                .lines()
                .filter_map(|line| line.split_whitespace().nth(1))
                .filter_map(|address| address.rsplit_once(':').map(|(_, port)| port))
                .filter_map(|value| u16::from_str_radix(value, 16).ok())
                .any(|value| value == port)
        })
        .unwrap_or(false)
}

async fn compatible_agent_process(adb: &AdbRunner, port: u16) -> bool {
    let Some(pid) = agent_pid(adb, port).await else {
        return false;
    };
    let command = format!("tr '\\0' '\\n' </proc/{pid}/environ");
    match adb.shell(["sh", "-c", &command]).await {
        Ok(output) => output.stdout.lines().any(classpath_matches),
        Err(error) => {
            warn!(
                target: "android_driver_rs::driver",
                pid,
                port,
                error = %error,
                "无法读取 Agent 进程环境，按目标命令行尝试复用"
            );
            true
        }
    }
}

fn classpath_matches(value: &str) -> bool {
    value
        .strip_prefix("CLASSPATH=")
        .is_some_and(|classpath| classpath.split(':').any(|entry| entry == REMOTE_JAR))
}

async fn create_forward(adb: &AdbRunner, remote_port: u16) -> Result<OwnedForward> {
    trace!(target: "android_driver_rs::driver", remote_port, "创建端口转发");
    let remote = format!("tcp:{remote_port}");
    let output = adb
        .run_text(["forward", "tcp:0", &remote], adb.agent_timeout())
        .await?;
    let local_port = output
        .stdout
        .trim()
        .parse()
        .map_err(|_| DriverError::Forward("ADB 未返回动态本地端口".into()))?;
    Ok(OwnedForward {
        local_port,
        remote_port,
    })
}

async fn remove_forward(adb: &AdbRunner, forward: &OwnedForward) -> Result<()> {
    trace!(target: "android_driver_rs::driver", local_port = forward.local_port, remote_port = forward.remote_port, "移除端口转发");
    let local = format!("tcp:{}", forward.local_port);
    adb.run_text(["forward", "--remove", &local], adb.agent_timeout())
        .await?;
    for _ in 0..3 {
        let output = adb
            .run_text(["forward", "--list"], adb.agent_timeout())
            .await?;
        if !output
            .stdout
            .lines()
            .any(|line| line.split_whitespace().any(|value| value == local))
        {
            return Ok(());
        }
        sleep(Duration::from_millis(100)).await;
    }
    Err(DriverError::Forward(format!(
        "tcp:{} -> tcp:{} 删除后仍存在",
        forward.local_port, forward.remote_port
    )))
}

async fn cleanup_resources(adb: &AdbRunner, state: &mut SessionState) -> Result<()> {
    debug!(target: "android_driver_rs::driver", "清理资源");
    if let Some(agent) = state.owned_agent.as_mut() {
        stop_owned_agent(adb, agent).await?;
        state.owned_agent = None;
    }
    let index = 0;
    while index < state.forwards.len() {
        let forward = state.forwards[index].clone();
        match remove_forward(adb, &forward).await {
            Ok(()) => {
                state.forwards.remove(index);
            }
            Err(source) => {
                return Err(DriverError::ForwardCleanup {
                    local_port: forward.local_port,
                    source: Box::new(source),
                });
            }
        }
    }
    Ok(())
}

async fn restore_ime_locked(adb: &AdbRunner, state: &mut SessionState) -> Result<()> {
    if let Some(original) = state.active_ime.as_deref() {
        adb.shell(["ime", "set", original]).await?;
        state.active_ime = None;
    }
    Ok(())
}

#[cfg(feature = "input-method")]
struct ImeGuard {
    adb: AdbRunner,
    original: Option<String>,
    state: Arc<DriverInner>,
}

#[cfg(feature = "input-method")]
impl ImeGuard {
    async fn restore(mut self) -> Result<()> {
        if let Some(original) = self.original.as_deref() {
            self.adb.shell(["ime", "set", original]).await?;
            self.state.state.lock().await.active_ime = None;
            self.original = None;
        }
        Ok(())
    }
}

#[cfg(feature = "input-method")]
impl Drop for ImeGuard {
    fn drop(&mut self) {
        let Some(original) = self.original.take() else {
            return;
        };
        let adb = self.adb.clone();
        let state = self.state.clone();
        spawn_cleanup(async move {
            let _ = adb.shell(["ime", "set", &original]).await;
            state.state.lock().await.active_ime = None;
        });
    }
}

fn spawn_cleanup(future: impl Future<Output = ()> + Send + 'static) {
    debug!(target: "android_driver_rs::driver", "生成后台清理任务");
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        handle.spawn(future);
    } else {
        let _ = std::thread::Builder::new()
            .name("android-driver-cleanup".into())
            .spawn(move || {
                if let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    runtime.block_on(future);
                }
            });
    }
}

impl Drop for DriverInner {
    fn drop(&mut self) {
        let Ok(mut state) = self.state.try_lock() else {
            warn!(target: "android_driver_rs::driver", "Driver 释放时会话仍被占用，无法兜底清理");
            return;
        };
        if state.closed {
            return;
        }
        if let Some(rpc) = state.rpc.take() {
            rpc.invalidate();
        }
        let mut detached = SessionState {
            rpc: None,
            forwards: std::mem::take(&mut state.forwards),
            owned_agent: state.owned_agent.take(),
            generation: state.generation,
            closed: false,
            active_ime: state.active_ime.take(),
        };
        let adb = self.adb.clone();
        spawn_cleanup(async move {
            let _ = restore_ime_locked(&adb, &mut detached).await;
            let _ = cleanup_resources(&adb, &mut detached).await;
        });
    }
}

fn parse_display_size(value: &str) -> Option<DisplaySize> {
    let regex = regex::Regex::new(r"(?m)(?:Override|Physical) size:\s*(\d+)x(\d+)").ok()?;
    let captures = regex.captures_iter(value).last()?;
    Some(DisplaySize {
        width: captures[1].parse().ok()?,
        height: captures[2].parse().ok()?,
    })
}

fn validate_point(point: Point) -> Result<()> {
    if point.x >= 0 && point.y >= 0 {
        Ok(())
    } else {
        Err(DriverError::InvalidCoordinate("绝对坐标不能为负数".into()))
    }
}

fn validate_image(bytes: Vec<u8>) -> Result<Vec<u8>> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") || bytes.starts_with(b"\xff\xd8\xff") {
        Ok(bytes)
    } else {
        let magic = bytes
            .iter()
            .take(8)
            .map(|value| format!("{value:02x}"))
            .collect::<String>();
        Err(DriverError::InvalidScreenshot(format!(
            "未知图像魔数 {magic}"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefers_override_display_size() {
        assert_eq!(
            parse_display_size("Physical size: 1080x2400\nOverride size: 720x1600\n"),
            Some(DisplaySize {
                width: 720,
                height: 1600
            })
        );
    }

    #[test]
    fn validates_png_signature() {
        assert!(validate_image(b"\x89PNG\r\n\x1a\nrest".to_vec()).is_ok());
        assert!(validate_image(b"\xff\xd8\xffrest".to_vec()).is_ok());
        assert!(validate_image(b"bad".to_vec()).is_err());
    }

    #[test]
    fn parses_common_sha256_output_formats() {
        let gnu = format!("{}  /data/local/tmp/u2.jar\n", agent::JAR_SHA256);
        assert_eq!(parse_sha256_output(&gnu), Some(agent::JAR_SHA256));

        let uppercase = agent::JAR_SHA256.to_ascii_uppercase();
        let bsd = format!("SHA256 (/data/local/tmp/u2.jar) = {uppercase}\r\n");
        assert_eq!(parse_sha256_output(&bsd), Some(uppercase.as_str()));
        assert!(remote_digest_matches(&RemoteFileInfo {
            digest: Some(uppercase),
            size: Some(agent::JAR_SIZE),
            exists: true,
        }));
        assert!(remote_file_matches(&RemoteFileInfo {
            digest: None,
            size: Some(agent::JAR_SIZE),
            exists: true,
        }));
        assert!(!remote_file_matches(&RemoteFileInfo {
            digest: None,
            size: Some(agent::JAR_SIZE - 1),
            exists: true,
        }));
    }

    #[test]
    fn parses_legacy_ps_rows_and_proc_rows() {
        assert_eq!(
            parse_agent_pid_line(
                "u0_a123 4321 88 123456 45678 ffffffff 00000000 S com.wetest.uia2.Main -p 19008",
                "19008"
            ),
            Some(4321)
        );
        assert_eq!(
            parse_agent_pid_line("4321 com.wetest.uia2.Main -p 19008", "19008"),
            Some(4321)
        );
        assert_eq!(
            parse_agent_pid_line("4321 com.wetest.uia2.Main -p 9008", "19008"),
            None
        );
    }

    #[test]
    fn accepts_classpath_variants() {
        assert!(classpath_matches(&format!("CLASSPATH={REMOTE_JAR}")));
        assert!(classpath_matches(&format!(
            "CLASSPATH=/system/framework/foo.jar:{REMOTE_JAR}:"
        )));
        assert!(!classpath_matches("PATH=/system/bin"));
    }
}
