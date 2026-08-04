use super::*;

impl AndroidDriver {
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

    /// 将媒体流音量设置为零。
    pub async fn mute_media(&self) -> Result<()> {
        debug!(target: "android_driver_rs::driver", "静音媒体流");
        let sdk_level = self
            .property("ro.build.version.sdk")
            .await?
            .parse::<u32>()
            .map_err(|_| DriverError::Protocol("SDK 版本无效".into()))?;
        if sdk_level > 23
            && self
                .inner
                .adb
                .shell([
                    "cmd",
                    "media_session",
                    "volume",
                    "--stream",
                    "3",
                    "--set",
                    "0",
                ])
                .await
                .is_ok()
        {
            return Ok(());
        }

        let output = self
            .inner
            .adb
            .shell([
                "service",
                "call",
                "audio",
                "3",
                "i32",
                "3",
                "i32",
                "0",
                "i32",
                "0",
                "s16",
                "com.android.shell",
            ])
            .await?;
        let diagnostic = format!("{}\n{}", output.stdout, output.stderr);
        if diagnostic.to_ascii_lowercase().contains("exception") {
            return Err(DriverError::AdbCommand {
                code: Some(output.status),
                message: diagnostic.trim().to_owned(),
            });
        }
        Ok(())
    }
}
