//! Android 原生 UI 自动化驱动。
//!
//! 本 crate 通过 ADB CLI 部署并启动固定版本的 `u2.jar`，再使用 HTTP
//! JSON-RPC 2.0 操作设备。所有设备序列号的格式化输出均会脱敏。

mod adb;
mod agent;
mod driver;
mod error;
mod rpc;
mod selector;
mod types;
mod ui;
mod xpath;

#[cfg(feature = "blocking")]
pub mod blocking;

pub use adb::{AdbConfig, CommandOutput};
pub use agent::{AgentProfile, AgentSource};
pub use driver::{AndroidDriver, AndroidDriverBuilder, DriverConfig};
pub use error::{DriverError, Result};
pub use selector::{Element, MatchPattern, Selector};
pub use types::{
    ActivityName, AndroidKeyCode, AppIdentifier, Bounds, DeviceDescriptor, DeviceInfo,
    DeviceSelector, DeviceSerial, DeviceStatus, DisplaySize, NormalizedPoint, Point, Position,
    ScreenState, ScreenshotMethod,
};
pub use ui::UiNode;
pub use xpath::XPathElement;
