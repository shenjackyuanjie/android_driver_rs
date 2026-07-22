//! 在系统设置搜索框验证 Unicode setText，并在结束时返回桌面。

use android_driver_rs::{ActivityName, AndroidDriver, AppIdentifier, MatchPattern, Selector};
use std::time::Duration;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let driver = AndroidDriver::builder().connect().await?;
    let settings = AppIdentifier::new("com.android.settings")?;
    let activity = ActivityName::new("com.android.settings.Settings")?;
    let result = async {
        driver.start_app(&settings, Some(&activity)).await?;
        tokio::time::sleep(Duration::from_secs(1)).await;
        let search = Selector::new().description(MatchPattern::Contains("搜索".into()));
        driver
            .wait_for(&search, Duration::from_secs(5))
            .await?
            .click()
            .await?;
        let edit = driver
            .wait_for(
                &Selector::new().type_name("android.widget.EditText"),
                Duration::from_secs(5),
            )
            .await?;
        edit.click().await?;
        driver.input_text("中文输入测试").await?;
        let actual = edit.text().await?;
        if actual != "中文输入测试" {
            return Err(format!("Unicode 输入结果不匹配：{actual:?}").into());
        }
        edit.clear_text().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    }
    .await;
    let _ = driver.go_home().await;
    let close = driver.close().await;
    result?;
    close?;
    println!("Unicode 输入验证通过，已恢复桌面并关闭 Driver");
    Ok(())
}
