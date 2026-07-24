//! 内嵌 Agent 的解析、校验与本地原子物化。

use crate::{DriverError, Result};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::Mutex;
use tracing::{debug, info, trace, warn};

static MATERIALIZE_LOCK: Mutex<()> = Mutex::const_new(());
static TEMPORARY_SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub(crate) const JAR_NAME: &str = "u2.jar";
pub(crate) const APK_NAME: &str = "app-uiautomator.apk";
pub(crate) const JAR_SIZE: u64 = 3_707_333;
pub(crate) const JAR_SHA256: &str =
    "0b74e83c55f443539a9f76f5ce023a51466b764b1100e4097a897053fdfc0eb6";
pub(crate) const REMOTE_DIR: &str = "/data/local/tmp/android_driver_rs";
pub(crate) const REMOTE_JAR: &str = "/data/local/tmp/android_driver_rs/u2.jar";

/// Agent 文件来源。
#[derive(Clone, Debug, Default)]
pub enum AgentSource {
    /// 使用编译进 crate 的固定资源。
    #[default]
    Embedded,
    /// 从目录读取 `u2.jar` 和可选 APK。
    Directory(PathBuf),
}

/// 当前锁定的 Agent 描述。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentProfile {
    pub jar_version: &'static str,
    pub apk_version: &'static str,
    pub jar_size: u64,
    pub jar_sha256: &'static str,
}

impl Default for AgentProfile {
    fn default() -> Self {
        Self {
            jar_version: "0.4.0",
            apk_version: "2.4.0",
            jar_size: JAR_SIZE,
            jar_sha256: JAR_SHA256,
        }
    }
}

pub(crate) struct MaterializedAgent {
    pub jar: PathBuf,
    pub apk: Option<PathBuf>,
}

pub(crate) async fn materialize(source: &AgentSource) -> Result<MaterializedAgent> {
    debug!(target: "android_driver_rs::agent", ?source, "物化 Agent");
    let files = match source {
        AgentSource::Directory(directory) => MaterializedAgent {
            jar: directory.join(JAR_NAME),
            apk: directory
                .join(APK_NAME)
                .is_file()
                .then(|| directory.join(APK_NAME)),
        },
        AgentSource::Embedded => materialize_embedded().await?,
    };
    verify_jar(&files.jar).await?;
    Ok(files)
}

#[cfg(feature = "embedded-agent")]
async fn materialize_embedded() -> Result<MaterializedAgent> {
    info!(target: "android_driver_rs::agent", "物化内嵌 Agent");
    static JAR: &[u8] = include_bytes!("../assets/u2.jar");
    static APK: &[u8] = include_bytes!("../assets/app-uiautomator.apk");
    let directory = std::env::temp_dir().join("android_driver_rs").join("0.1.0");
    tokio::fs::create_dir_all(&directory).await?;
    let jar = write_atomic(&directory, JAR_NAME, JAR).await?;
    let apk = write_atomic(&directory, APK_NAME, APK).await?;
    Ok(MaterializedAgent {
        jar,
        apk: Some(apk),
    })
}

#[cfg(not(feature = "embedded-agent"))]
async fn materialize_embedded() -> Result<MaterializedAgent> {
    Err(DriverError::AgentNotFound(PathBuf::from(
        "embedded-agent feature",
    )))
}

async fn write_atomic(directory: &Path, name: &str, bytes: &[u8]) -> Result<PathBuf> {
    let _guard = MATERIALIZE_LOCK.lock().await;
    let target = directory.join(name);
    if file_matches(&target, bytes).await {
        trace!(target: "android_driver_rs::agent", path = %target.display(), "复用已物化的 Agent 文件");
        return Ok(target);
    }

    let sequence = TEMPORARY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let temporary = directory.join(format!(".{name}.{}.{}.tmp", std::process::id(), sequence));
    tokio::fs::write(&temporary, bytes).await?;

    if let Err(first_error) = tokio::fs::rename(&temporary, &target).await {
        if file_matches(&target, bytes).await {
            let _ = tokio::fs::remove_file(&temporary).await;
            return Ok(target);
        }
        debug!(
            target: "android_driver_rs::agent",
            path = %target.display(),
            error = %first_error,
            "原子替换 Agent 文件失败，清理旧文件后重试"
        );
        match tokio::fs::remove_file(&target).await {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                let _ = tokio::fs::remove_file(&temporary).await;
                return Err(error.into());
            }
        }
        if let Err(error) = tokio::fs::rename(&temporary, &target).await {
            let _ = tokio::fs::remove_file(&temporary).await;
            return Err(error.into());
        }
    }

    debug!(
        target: "android_driver_rs::agent",
        path = %target.display(),
        size = bytes.len(),
        "Agent 文件物化完成"
    );
    Ok(target)
}

async fn file_matches(path: &Path, expected: &[u8]) -> bool {
    let matches_size = tokio::fs::metadata(path)
        .await
        .map(|value| value.len() == expected.len() as u64)
        .unwrap_or(false);
    matches_size
        && tokio::fs::read(path)
            .await
            .map(|actual| actual == expected)
            .unwrap_or(false)
}

async fn verify_jar(path: &Path) -> Result<()> {
    trace!(target: "android_driver_rs::agent", path = %path.display(), "校验 Agent");
    if !path.is_file() {
        warn!(target: "android_driver_rs::agent", path = %path.display(), "Agent 文件不存在");
        return Err(DriverError::AgentNotFound(path.to_owned()));
    }
    let bytes = tokio::fs::read(path).await?;
    if bytes.len() as u64 != JAR_SIZE {
        warn!(target: "android_driver_rs::agent", expected = JAR_SIZE, actual = bytes.len(), "Agent 大小不匹配");
        return Err(DriverError::AgentVerification(format!(
            "u2.jar 大小应为 {JAR_SIZE} bytes"
        )));
    }
    let digest = format!("{:x}", Sha256::digest(&bytes));
    if digest != JAR_SHA256 {
        warn!(target: "android_driver_rs::agent", "Agent SHA-256 不匹配");
        return Err(DriverError::AgentVerification(
            "u2.jar SHA-256 不匹配".into(),
        ));
    }
    trace!(target: "android_driver_rs::agent", "Agent 验证通过");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[cfg(feature = "embedded-agent")]
    async fn embedded_jar_matches_locked_manifest() {
        let files = materialize(&AgentSource::Embedded).await.unwrap();
        assert_eq!(
            tokio::fs::metadata(files.jar).await.unwrap().len(),
            JAR_SIZE
        );
    }

    #[tokio::test]
    async fn concurrent_atomic_writes_publish_complete_file() {
        let directory = tempfile::tempdir().unwrap();
        let bytes = vec![0x5a; 1024 * 1024];
        let (first, second, third) = tokio::join!(
            write_atomic(directory.path(), "agent.bin", &bytes),
            write_atomic(directory.path(), "agent.bin", &bytes),
            write_atomic(directory.path(), "agent.bin", &bytes),
        );
        first.unwrap();
        second.unwrap();
        third.unwrap();

        assert_eq!(
            tokio::fs::read(directory.path().join("agent.bin"))
                .await
                .unwrap(),
            bytes
        );
    }

    #[tokio::test]
    async fn atomic_write_repairs_same_size_corruption() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("agent.bin");
        tokio::fs::write(&target, b"broken").await.unwrap();

        write_atomic(directory.path(), "agent.bin", b"intact")
            .await
            .unwrap();

        assert_eq!(tokio::fs::read(target).await.unwrap(), b"intact");
    }
}
