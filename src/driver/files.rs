use super::*;

impl AndroidDriver {
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
}
