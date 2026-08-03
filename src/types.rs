use crate::{DriverError, Result};
use serde::{Deserialize, Serialize};
use std::fmt;

/// 设备序列号。`Debug` 和 `Display` 始终脱敏。
///
/// 这里故意不使用 `secrecy` 之类的包装类型。本类型的安全边界完全来自下方
/// 手写的 `Debug`/`Display` 实现：只要不调用 [`DeviceSerial::expose_secret`]，
/// 序列号就不可能被格式化进日志或错误消息。包装类型提供的是同一道保护，
/// 同样拦不住“调用方主动 expose 后拉进字符串”这种用法（参见 `adb.rs` 的
/// `redact`，无论否都需要手写），因此引入额外依赖并无实质收益。
///
/// 不变式由 `serial_never_formats_plaintext` 测试锅守。
#[derive(Clone, Eq)]
pub struct DeviceSerial(String);

impl DeviceSerial {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }
    /// 显式取得原始序列号。调用方不得记录返回值。
    ///
    /// 这是本类型唯一的泄露口。传给 ADB 命令行参数是预期用法；写进任何会被
    /// 展示或持久化的文本前，必须先经过 `AdbRunner::redact`。
    pub fn expose_secret(&self) -> &str {
        &self.0
    }
}

impl PartialEq for DeviceSerial {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}

impl fmt::Debug for DeviceSerial {
    /// 恒输出占位符。不要改成输出真实值（包括“只显示后四位”之类的妥协），
    /// 否则包含 `DeviceSerial` 的 `DeviceDescriptor` 等结构体一旦被 `{:?}`
    /// 打印就会泄露。
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("DeviceSerial(<redacted>)")
    }
}

impl fmt::Display for DeviceSerial {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

#[derive(Clone, Debug, Default)]
pub enum DeviceSelector {
    #[default]
    Auto,
    Serial(DeviceSerial),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DeviceStatus {
    Online,
    Offline,
    Unauthorized,
    Unknown(String),
}

#[derive(Clone, Debug)]
pub struct DeviceDescriptor {
    pub serial: DeviceSerial,
    pub status: DeviceStatus,
    pub details: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Point {
    pub x: i32,
    pub y: i32,
}

impl Point {
    pub const fn new(x: i32, y: i32) -> Self {
        Self { x, y }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NormalizedPoint {
    pub x: f64,
    pub y: f64,
}

impl NormalizedPoint {
    pub fn new(x: f64, y: f64) -> Result<Self> {
        if x.is_finite() && y.is_finite() && (0.0..=1.0).contains(&x) && (0.0..=1.0).contains(&y) {
            Ok(Self { x, y })
        } else {
            Err(DriverError::InvalidCoordinate(
                "归一化坐标必须位于 0 到 1".into(),
            ))
        }
    }

    pub fn resolve(self, display: DisplaySize) -> Result<Point> {
        let x = display
            .width
            .checked_sub(1)
            .ok_or_else(|| DriverError::InvalidCoordinate("显示宽度不能为 0".into()))?;
        let y = display
            .height
            .checked_sub(1)
            .ok_or_else(|| DriverError::InvalidCoordinate("显示高度不能为 0".into()))?;
        Ok(Point::new(
            (self.x * f64::from(x)).round() as i32,
            (self.y * f64::from(y)).round() as i32,
        ))
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Position {
    Absolute(Point),
    Normalized(NormalizedPoint),
}

impl Position {
    pub const fn absolute(x: i32, y: i32) -> Self {
        Self::Absolute(Point::new(x, y))
    }
    pub fn normalized(x: f64, y: f64) -> Result<Self> {
        NormalizedPoint::new(x, y).map(Self::Normalized)
    }
    pub fn resolve(self, display: DisplaySize) -> Result<Point> {
        match self {
            Self::Absolute(value) => Ok(value),
            Self::Normalized(value) => value.resolve(display),
        }
    }
}

impl From<Point> for Position {
    fn from(value: Point) -> Self {
        Self::Absolute(value)
    }
}
impl From<NormalizedPoint> for Position {
    fn from(value: NormalizedPoint) -> Self {
        Self::Normalized(value)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bounds {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl Bounds {
    pub const fn center(self) -> Point {
        Point::new((self.left + self.right) / 2, (self.top + self.bottom) / 2)
    }
    pub const fn width(self) -> i32 {
        self.right - self.left
    }
    pub const fn height(self) -> i32 {
        self.bottom - self.top
    }
    pub const fn is_valid(self) -> bool {
        self.right >= self.left && self.bottom >= self.top
    }
    pub fn parse(value: &str) -> Option<Self> {
        let values: Vec<i32> = value
            .split(|ch: char| !ch.is_ascii_digit() && ch != '-')
            .filter(|v| !v.is_empty())
            .filter_map(|v| v.parse().ok())
            .collect();
        (values.len() == 4)
            .then(|| Self {
                left: values[0],
                top: values[1],
                right: values[2],
                bottom: values[3],
            })
            .filter(|value| value.is_valid())
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DisplaySize {
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceInfo {
    pub manufacturer: String,
    pub model: String,
    pub android_version: String,
    pub sdk_level: u32,
    pub cpu_abi: String,
    pub display_size: DisplaySize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ScreenState {
    Awake,
    Asleep,
    Unknown(String),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ScreenshotMethod {
    #[default]
    Auto,
    AdbScreencap,
    U2,
}

macro_rules! identifier {
    ($name:ident) => {
        #[derive(Clone, Debug, PartialEq, Eq, Hash)]
        pub struct $name(String);
        impl $name {
            pub fn new(value: impl Into<String>) -> Result<Self> {
                let value = value.into();
                if valid_identifier(&value) {
                    Ok(Self(value))
                } else {
                    Err(DriverError::InvalidIdentifier(value))
                }
            }
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }
        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

identifier!(AppIdentifier);
identifier!(ActivityName);

fn valid_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 255
        && value.split('.').all(|part| {
            !part.is_empty()
                && part.chars().enumerate().all(|(index, ch)| {
                    ch == '_'
                        || ch == '$'
                        || ch.is_ascii_alphanumeric() && (index > 0 || !ch.is_ascii_digit())
                })
        })
}

/// Android `KeyEvent` 键码。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AndroidKeyCode(pub u32);

impl AndroidKeyCode {
    pub const HOME: Self = Self(3);
    pub const BACK: Self = Self(4);
    pub const POWER: Self = Self(26);
    pub const ENTER: Self = Self(66);
}

impl From<u32> for AndroidKeyCode {
    fn from(value: u32) -> Self {
        Self(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serial_never_formats_plaintext() {
        let serial = DeviceSerial::new("sensitive-serial");
        assert_eq!(serial.to_string(), "<redacted>");
        assert!(!format!("{serial:?}").contains("sensitive"));
    }

    #[test]
    fn parses_android_bounds() {
        assert_eq!(
            Bounds::parse("[1,2][30,40]"),
            Some(Bounds {
                left: 1,
                top: 2,
                right: 30,
                bottom: 40
            })
        );
    }
}
