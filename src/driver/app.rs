use super::*;

impl AndroidDriver {
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
            .shell_with_timeout(["am", "start", "-W", "-n", &component], START_APP_TIMEOUT)
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
}
