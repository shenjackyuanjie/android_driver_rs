//! 真机 smoke：不会输出设备序列号，也不会改变业务应用状态。

use android_driver_rs::{AdbConfig, AndroidDriver, DeviceStatus, ScreenshotMethod};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cycles = std::env::args()
        .nth(1)
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(1);
    let devices = AndroidDriver::discover_devices(AdbConfig::default()).await?;
    let online = devices
        .iter()
        .filter(|device| device.status == DeviceStatus::Online)
        .count();
    if online != 1 {
        return Err(format!("smoke 要求恰好一台在线设备，当前为 {online} 台").into());
    }
    for cycle in 1..=cycles {
        let driver = AndroidDriver::builder().connect().await?;
        let display = driver.display_size().await?;
        let tree = driver.ui_tree().await?;
        let xpath_count = driver.xpath_all("//node").await?.len();
        let screenshot = driver
            .screenshot_with_method(ScreenshotMethod::AdbScreencap)
            .await?;
        driver.recover().await?;
        driver.close().await?;
        println!(
            "周期 {cycle}/{cycles}：{}x{}，UI 根节点 {} 个子节点，XPath {xpath_count} 个节点，截图 {} bytes",
            display.width,
            display.height,
            tree.children.len(),
            screenshot.len()
        );
    }
    Ok(())
}
