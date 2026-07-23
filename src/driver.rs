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
}

impl Default for DriverConfig {
    fn default() -> Self {
        Self {
            rpc_timeout: Duration::from_secs(20),
            max_json_size: 8 * 1024 * 1024,
            wait_interval: Duration::from_millis(500),
        }
    }
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
    debug!(target: "android_driver_rs::driver", local = %local.display(), "部署 Agent JAR");
    adb.shell(["mkdir", "-p", REMOTE_DIR]).await?;
    let current = adb
        .shell(["sha256sum", REMOTE_JAR])
        .await
        .ok()
        .map(|value| value.stdout);
    if current
        .as_deref()
        .is_some_and(|value| value.split_whitespace().next() == Some(agent::JAR_SHA256))
    {
        debug!(target: "android_driver_rs::driver", "Agent JAR 已就绪，跳过部署");
        return Ok(());
    }
    let temporary = format!("{REMOTE_JAR}.{}.tmp", std::process::id());
    adb.run_text(
        [
            OsString::from("push"),
            local.as_os_str().to_os_string(),
            OsString::from(&temporary),
        ],
        adb.transfer_timeout(),
    )
    .await?;
    adb.shell(["chmod", "0644", &temporary]).await?;
    adb.shell(["mv", &temporary, REMOTE_JAR]).await?;
    let digest = adb.shell(["sha256sum", REMOTE_JAR]).await?.stdout;
    if digest.split_whitespace().next() != Some(agent::JAR_SHA256) {
        return Err(DriverError::AgentVerification(
            "设备端 u2.jar SHA-256 不匹配".into(),
        ));
    }
    info!(target: "android_driver_rs::driver", "Agent JAR 部署完成");
    Ok(())
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

    for port in OWNED_AGENT_PORTS {
        if remote_port_in_use(adb, port).await {
            continue;
        }
        let mut owned_agent = match start_agent(adb, port).await {
            Ok(value) => value,
            Err(_) => continue,
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
    Err(DriverError::AgentStartup(
        "9008 不可借用，且 19008..=19017 均无法启动 Agent".into(),
    ))
}

async fn start_agent(adb: &AdbRunner, port: u16) -> Result<OwnedAgent> {
    debug!(target: "android_driver_rs::driver", remote_port = port, "启动自有 Agent");
    let classpath = format!("CLASSPATH={REMOTE_JAR}");
    let mut host_process = adb.spawn_long_running([
        "shell".to_owned(),
        classpath,
        "app_process".to_owned(),
        "/".to_owned(),
        "com.wetest.uia2.Main".to_owned(),
        "-p".to_owned(),
        port.to_string(),
    ])?;
    let deadline = Instant::now() + adb.agent_timeout();
    loop {
        if let Some(pid) = agent_pid(adb, port).await {
            return Ok(OwnedAgent {
                pid,
                port,
                host_process,
            });
        }
        if host_process
            .try_wait()
            .map_err(DriverError::AdbSpawn)?
            .is_some()
        {
            return Err(DriverError::AgentStartup(
                "Agent 子进程在监听端口前退出".into(),
            ));
        }
        if Instant::now() >= deadline {
            let _ = host_process.kill().await;
            return Err(DriverError::AgentStartup("等待 Agent PID 超时".into()));
        }
        sleep(Duration::from_millis(100)).await;
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

async fn agent_pid(adb: &AdbRunner, port: u16) -> Option<u32> {
    let processes = adb.shell(["ps", "-A", "-o", "PID,ARGS"]).await.ok()?;
    processes.stdout.lines().find_map(|line| {
        (line.contains("com.wetest.uia2.Main") && line.contains(&format!("-p {port}")))
            .then(|| line.split_whitespace().next()?.parse().ok())
            .flatten()
    })
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
    let Ok(processes) = adb.shell(["ps", "-A", "-o", "PID,ARGS"]).await else {
        return false;
    };
    for line in processes.stdout.lines() {
        if !line.contains("com.wetest.uia2.Main") || !line.contains(&port.to_string()) {
            continue;
        }
        let Some(pid) = line
            .split_whitespace()
            .next()
            .filter(|value| value.chars().all(|ch| ch.is_ascii_digit()))
        else {
            continue;
        };
        let command = format!("tr '\\0' '\\n' </proc/{pid}/environ");
        if adb.shell(["sh", "-c", &command]).await.is_ok_and(|output| {
            output
                .stdout
                .lines()
                .any(|value| value == format!("CLASSPATH={REMOTE_JAR}"))
        }) {
            return true;
        }
    }
    false
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
}
