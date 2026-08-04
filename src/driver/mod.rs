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
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Child;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tokio::time::{Instant, sleep};
use tracing::{debug, info, trace, warn};

const DEFAULT_AGENT_PORT: u16 = 9008;
const START_APP_TIMEOUT: Duration = Duration::from_secs(30);
const RESOURCE_CLEANUP_ATTEMPTS: usize = 3;
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
    capture: AgentCapture,
}
struct EstablishedSession {
    rpc: RpcClient,
    forward: OwnedForward,
    owned_agent: Option<OwnedAgent>,
}

struct ForwardGuard {
    adb: AdbRunner,
    forward: Option<OwnedForward>,
}

impl ForwardGuard {
    fn new(adb: &AdbRunner, forward: OwnedForward) -> Self {
        Self {
            adb: adb.clone(),
            forward: Some(forward),
        }
    }

    fn into_inner(mut self) -> OwnedForward {
        self.forward.take().expect("forward guard 已持有资源")
    }

    async fn cleanup(mut self) -> Result<()> {
        let Some(forward) = self.forward.as_ref() else {
            return Ok(());
        };
        let result = remove_forward_with_retries(&self.adb, forward).await;
        self.forward = None;
        result
    }
}

impl Drop for ForwardGuard {
    fn drop(&mut self) {
        let Some(forward) = self.forward.take() else {
            return;
        };
        let adb = self.adb.clone();
        spawn_cleanup(async move {
            let _ = remove_forward_with_retries(&adb, &forward).await;
        });
    }
}

struct OwnedAgentGuard {
    adb: AdbRunner,
    agent: Option<OwnedAgent>,
}

impl OwnedAgentGuard {
    fn new(adb: &AdbRunner, agent: OwnedAgent) -> Self {
        Self {
            adb: adb.clone(),
            agent: Some(agent),
        }
    }

    fn into_inner(mut self) -> OwnedAgent {
        self.agent.take().expect("agent guard 已持有资源")
    }

    async fn cleanup(mut self) -> Result<()> {
        let Some(agent) = self.agent.as_mut() else {
            return Ok(());
        };
        let result = stop_owned_agent_with_retries(&self.adb, agent).await;
        self.agent = None;
        result
    }
}

impl Drop for OwnedAgentGuard {
    fn drop(&mut self) {
        let Some(mut agent) = self.agent.take() else {
            return;
        };
        let adb = self.adb.clone();
        spawn_cleanup(async move {
            let _ = stop_owned_agent_with_retries(&adb, &mut agent).await;
        });
    }
}

struct StartingAgentGuard {
    adb: AdbRunner,
    port: u16,
    armed: bool,
}

impl StartingAgentGuard {
    fn new(adb: &AdbRunner, port: u16) -> Self {
        Self {
            adb: adb.clone(),
            port,
            armed: true,
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }

    async fn cleanup(mut self) {
        if self.armed
            && let Some(pid) = agent_pid(&self.adb, self.port).await
        {
            let _ = self.adb.shell(["kill", &pid.to_string()]).await;
        }
        self.armed = false;
    }
}

impl Drop for StartingAgentGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let adb = self.adb.clone();
        let port = self.port;
        spawn_cleanup(async move {
            if let Some(pid) = agent_pid(&adb, port).await {
                let _ = adb.shell(["kill", &pid.to_string()]).await;
            }
        });
    }
}

mod app;
mod device;
mod files;
mod input;
mod query;
mod session;

use session::{
    AgentCapture, agent_pid, cleanup_resources, deploy_jar, establish_session,
    remove_forward_with_retries, restore_ime_locked, spawn_cleanup, stop_owned_agent_with_retries,
};

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
        let mut cleanup_error = restore_ime_locked(&self.inner.adb, &mut state).await.err();
        if let Err(error) = cleanup_resources(&self.inner.adb, &mut state).await
            && cleanup_error.is_none()
        {
            cleanup_error = Some(error);
        }
        state.closed = true;
        match cleanup_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
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
