//! ADB CLI 配置、设备发现与命令执行。

use crate::types::{DeviceDescriptor, DeviceSelector, DeviceSerial, DeviceStatus};
use crate::{DriverError, Result};
use std::env;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;
use tokio::process::Child;
use tokio::process::Command;
use tokio::time::timeout;
use tracing::{debug, trace, warn};

/// ADB CLI 配置。
#[derive(Clone, Debug)]
pub struct AdbConfig {
    path: Option<PathBuf>,
    server: Option<(String, u16)>,
    pub command_timeout: Duration,
    pub transfer_timeout: Duration,
    pub agent_timeout: Duration,
}

impl Default for AdbConfig {
    fn default() -> Self {
        Self {
            path: None,
            server: None,
            command_timeout: Duration::from_secs(10),
            transfer_timeout: Duration::from_secs(60),
            agent_timeout: Duration::from_secs(15),
        }
    }
}

impl AdbConfig {
    pub fn with_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.path = Some(path.into());
        self
    }
    pub fn with_server(mut self, host: impl Into<String>, port: u16) -> Self {
        self.server = Some((host.into(), port));
        self
    }
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }
    pub fn server(&self) -> Option<(&str, u16)> {
        self.server
            .as_ref()
            .map(|(host, port)| (host.as_str(), *port))
    }
}

/// ADB 命令的成功输出。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandOutput {
    pub stdout: String,
    pub stderr: String,
    pub status: i32,
}

#[derive(Clone)]
pub(crate) struct AdbRunner {
    inner: Arc<AdbRunnerInner>,
}

struct AdbRunnerInner {
    executable: PathBuf,
    server: Option<(String, u16)>,
    serial: Option<DeviceSerial>,
    config: AdbConfig,
}

impl AdbRunner {
    pub fn new(mut config: AdbConfig) -> Result<Self> {
        let executable = resolve_path(config.path.as_deref())?;
        let server = match config.server.take() {
            Some(value) => Some(validate_server(value)?),
            None => server_from_environment()?,
        };
        Ok(Self {
            inner: Arc::new(AdbRunnerInner {
                executable,
                server,
                serial: None,
                config,
            }),
        })
    }

    pub fn with_serial(&self, serial: DeviceSerial) -> Self {
        Self {
            inner: Arc::new(AdbRunnerInner {
                executable: self.inner.executable.clone(),
                server: self.inner.server.clone(),
                serial: Some(serial),
                config: self.inner.config.clone(),
            }),
        }
    }

    pub async fn discover(&self) -> Result<Vec<DeviceDescriptor>> {
        debug!(target: "android_driver_rs::adb", "发现设备");
        let output = self
            .run_text(["devices", "-l"], self.inner.config.command_timeout)
            .await?;
        Ok(parse_devices(&output.stdout))
    }

    pub async fn select(&self, selector: &DeviceSelector) -> Result<DeviceDescriptor> {
        debug!(target: "android_driver_rs::adb", ?selector, "选择设备");
        let devices = self.discover().await?;
        match selector {
            DeviceSelector::Auto => {
                let mut online = devices
                    .into_iter()
                    .filter(|item| item.status == DeviceStatus::Online);
                let first = online.next().ok_or(DriverError::DeviceNotFound)?;
                if online.next().is_some() {
                    return Err(DriverError::AmbiguousDevice { count: 2 });
                }
                Ok(first)
            }
            DeviceSelector::Serial(serial) => devices
                .into_iter()
                .find(|item| &item.serial == serial && item.status == DeviceStatus::Online)
                .ok_or(DriverError::DeviceOffline),
        }
    }

    pub async fn shell<I, S>(&self, args: I) -> Result<CommandOutput>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let mut values = vec!["shell".into()];
        values.extend(args.into_iter().map(|arg| arg.as_ref().to_os_string()));
        self.run_text(values, self.inner.config.command_timeout)
            .await
    }

    pub async fn run_text<I, S>(&self, args: I, duration: Duration) -> Result<CommandOutput>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let output = self.run_bytes(args, duration).await?;
        Ok(CommandOutput {
            stdout: self.redact(String::from_utf8_lossy(&output.stdout).into_owned()),
            stderr: self.redact(String::from_utf8_lossy(&output.stderr).into_owned()),
            status: output.code.unwrap_or_default(),
        })
    }

    pub async fn run_bytes<I, S>(&self, args: I, duration: Duration) -> Result<RawOutput>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let mut command = Command::new(&self.inner.executable);
        command
            .kill_on_drop(true)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some((host, port)) = &self.inner.server {
            command.arg("-H").arg(host).arg("-P").arg(port.to_string());
        }
        if let Some(serial) = &self.inner.serial {
            command.arg("-s").arg(serial.expose_secret());
        }
        command.args(args);
        trace!(target: "android_driver_rs::adb", command = ?command, "执行 ADB 命令");
        let child = command.spawn().map_err(DriverError::AdbSpawn)?;
        let output = match timeout(duration, child.wait_with_output()).await {
            Ok(Ok(output)) => output,
            Ok(Err(error)) => return Err(DriverError::AdbSpawn(error)),
            Err(_) => {
                warn!(target: "android_driver_rs::adb", ?duration, "ADB 命令超时");
                return Err(DriverError::AdbTimeout { timeout: duration });
            }
        };
        if !output.status.success() {
            let message = self.redact(String::from_utf8_lossy(&output.stderr).trim().to_owned());
            return Err(DriverError::AdbCommand {
                code: output.status.code(),
                message: truncate(if message.is_empty() {
                    "ADB 未返回错误文本".into()
                } else {
                    message
                }),
            });
        }
        Ok(RawOutput {
            stdout: output.stdout,
            stderr: output.stderr,
            code: output.status.code(),
        })
    }

    pub fn transfer_timeout(&self) -> Duration {
        self.inner.config.transfer_timeout
    }
    pub fn agent_timeout(&self) -> Duration {
        self.inner.config.agent_timeout
    }

    /// 启动由 Driver 持有的长期 ADB 子进程，不等待退出。
    pub fn spawn_long_running<I, S>(&self, args: I) -> Result<Child>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        trace!(target: "android_driver_rs::adb", "启动长期子进程");
        let mut command = Command::new(&self.inner.executable);
        command
            .kill_on_drop(true)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some((host, port)) = &self.inner.server {
            command.arg("-H").arg(host).arg("-P").arg(port.to_string());
        }
        if let Some(serial) = &self.inner.serial {
            command.arg("-s").arg(serial.expose_secret());
        }
        command.args(args);
        command.spawn().map_err(DriverError::AdbSpawn)
    }

    fn redact(&self, value: String) -> String {
        self.inner.serial.as_ref().map_or(value.clone(), |serial| {
            value.replace(serial.expose_secret(), "<redacted>")
        })
    }
}

pub(crate) struct RawOutput {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub code: Option<i32>,
}

fn resolve_path(explicit: Option<&Path>) -> Result<PathBuf> {
    if let Some(path) = explicit {
        return canonical(path);
    }
    if let Some(path) = env::var_os("ADB_PATH") {
        return canonical(Path::new(&path));
    }
    let path = env::var_os("PATH").ok_or(DriverError::AdbNotFound)?;
    let names: &[&str] = if cfg!(windows) {
        &["adb.exe", "adb"]
    } else {
        &["adb"]
    };
    for directory in env::split_paths(&path) {
        for name in names {
            let candidate = directory.join(name);
            if candidate.is_file() {
                return canonical(&candidate);
            }
        }
    }
    Err(DriverError::AdbNotFound)
}

fn canonical(path: &Path) -> Result<PathBuf> {
    if !path.is_file() {
        return Err(DriverError::InvalidAdbPath(path.to_owned()));
    }
    path.canonicalize().map_err(DriverError::Io)
}

fn server_from_environment() -> Result<Option<(String, u16)>> {
    match (
        env::var("ADB_SERVER_HOST").ok(),
        env::var("ADB_SERVER_PORT").ok(),
    ) {
        (Some(host), Some(port)) => validate_server((
            host,
            port.parse()
                .map_err(|_| DriverError::InvalidIdentifier("ADB server 端口".into()))?,
        ))
        .map(Some),
        _ => Ok(None),
    }
}

fn validate_server((host, port): (String, u16)) -> Result<(String, u16)> {
    if host.is_empty()
        || port == 0
        || !host
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | ':' | '[' | ']'))
    {
        Err(DriverError::InvalidIdentifier("ADB server 地址".into()))
    } else {
        Ok((host, port))
    }
}

fn truncate(value: String) -> String {
    if value.chars().count() <= 4096 {
        value
    } else {
        format!(
            "{}...[truncated]",
            value.chars().take(4096).collect::<String>()
        )
    }
}

pub(crate) fn parse_devices(output: &str) -> Vec<DeviceDescriptor> {
    output
        .lines()
        .skip_while(|line| !line.starts_with("List of devices"))
        .skip(1)
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let serial = fields.next()?;
            let status_text = fields.next()?;
            let status = match status_text {
                "device" => DeviceStatus::Online,
                "offline" => DeviceStatus::Offline,
                "unauthorized" => DeviceStatus::Unauthorized,
                value => DeviceStatus::Unknown(value.into()),
            };
            Some(DeviceDescriptor {
                serial: DeviceSerial::new(serial),
                status,
                details: fields.map(str::to_owned).collect(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_all_common_device_states_and_redacts() {
        let devices = parse_devices(
            "List of devices attached\nsecret-a device product:x model:y\nsecret-b offline\nsecret-c unauthorized\n",
        );
        assert_eq!(devices.len(), 3);
        assert_eq!(devices[0].status, DeviceStatus::Online);
        assert_eq!(devices[1].status, DeviceStatus::Offline);
        assert_eq!(devices[2].status, DeviceStatus::Unauthorized);
        assert!(!format!("{devices:?}").contains("secret-a"));
    }
}
