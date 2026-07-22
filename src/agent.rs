//! 内嵌 Agent 的解析、校验与本地原子物化。

use crate::{DriverError, Result};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

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
    let target = directory.join(name);
    if tokio::fs::metadata(&target)
        .await
        .map(|value| value.len() == bytes.len() as u64)
        .unwrap_or(false)
    {
        return Ok(target);
    }
    let temporary = directory.join(format!(".{name}.{}.tmp", std::process::id()));
    tokio::fs::write(&temporary, bytes).await?;
    if tokio::fs::rename(&temporary, &target).await.is_err() {
        let _ = tokio::fs::remove_file(&target).await;
        tokio::fs::rename(&temporary, &target).await?;
    }
    Ok(target)
}

async fn verify_jar(path: &Path) -> Result<()> {
    if !path.is_file() {
        return Err(DriverError::AgentNotFound(path.to_owned()));
    }
    let bytes = tokio::fs::read(path).await?;
    if bytes.len() as u64 != JAR_SIZE {
        return Err(DriverError::AgentVerification(format!(
            "u2.jar 大小应为 {JAR_SIZE} bytes"
        )));
    }
    let digest = format!("{:x}", Sha256::digest(&bytes));
    if digest != JAR_SHA256 {
        return Err(DriverError::AgentVerification(
            "u2.jar SHA-256 不匹配".into(),
        ));
    }
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
}
