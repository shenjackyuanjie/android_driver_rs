#[cfg(feature = "input-method")]
use super::session::ImeGuard;
use super::*;

impl AndroidDriver {
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
}
