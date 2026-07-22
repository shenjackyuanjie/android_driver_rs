//! uiautomator Selector 和每次重新定位的 Element。

use crate::{AndroidDriver, Bounds, DriverError, Point, Result};
use serde_json::{Map, Value, json};
use std::time::Duration;

/// 字符串属性匹配方式。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MatchPattern {
    Equals(String),
    Contains(String),
    StartsWith(String),
    EndsWith(String),
}

impl From<&str> for MatchPattern {
    fn from(value: &str) -> Self {
        Self::Equals(value.into())
    }
}
impl From<String> for MatchPattern {
    fn from(value: String) -> Self {
        Self::Equals(value)
    }
}

#[derive(Clone, Debug)]
enum Condition {
    String(&'static str, MatchPattern),
    Boolean(&'static str, bool),
}

/// 可串联条件的 Android 控件选择器。
#[derive(Clone, Debug, Default)]
pub struct Selector {
    conditions: Vec<Condition>,
}

impl Selector {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn id(self, value: impl Into<MatchPattern>) -> Self {
        self.string("resourceId", value.into())
    }
    pub fn type_name(self, value: impl Into<MatchPattern>) -> Self {
        self.string("className", value.into())
    }
    pub fn text(self, value: impl Into<MatchPattern>) -> Self {
        self.string("text", value.into())
    }
    pub fn description(self, value: impl Into<MatchPattern>) -> Self {
        self.string("description", value.into())
    }
    pub fn package_name(self, value: impl Into<MatchPattern>) -> Self {
        self.string("packageName", value.into())
    }
    pub fn clickable(self, value: bool) -> Self {
        self.boolean("clickable", value)
    }
    pub fn enabled(self, value: bool) -> Self {
        self.boolean("enabled", value)
    }
    pub fn focused(self, value: bool) -> Self {
        self.boolean("focused", value)
    }
    pub fn selected(self, value: bool) -> Self {
        self.boolean("selected", value)
    }
    pub fn checked(self, value: bool) -> Self {
        self.boolean("checked", value)
    }
    pub fn scrollable(self, value: bool) -> Self {
        self.boolean("scrollable", value)
    }

    fn string(mut self, name: &'static str, value: MatchPattern) -> Self {
        self.conditions.push(Condition::String(name, value));
        self
    }
    fn boolean(mut self, name: &'static str, value: bool) -> Self {
        self.conditions.push(Condition::Boolean(name, value));
        self
    }

    pub(crate) fn value(&self, index: usize) -> Value {
        let mut output = Map::new();
        let mut mask = 0_u32;
        output.insert("childOrSibling".into(), json!([]));
        output.insert("childOrSiblingSelector".into(), json!([]));
        for condition in &self.conditions {
            match condition {
                Condition::Boolean(name, value) => {
                    output.insert((*name).into(), json!(value));
                    mask |= field_mask(name);
                }
                Condition::String(name, pattern) => {
                    let (key, value) = string_field(name, pattern);
                    mask |= field_mask(&key);
                    output.insert(key, json!(value));
                }
            }
        }
        if index > 0 {
            output.insert("instance".into(), json!(index));
            mask |= field_mask("instance");
        }
        output.insert("mask".into(), json!(mask));
        Value::Object(output)
    }
}

fn string_field(name: &str, pattern: &MatchPattern) -> (String, String) {
    match pattern {
        MatchPattern::Equals(value) => (name.into(), value.clone()),
        MatchPattern::Contains(value) if matches!(name, "text" | "description") => {
            (format!("{name}Contains"), value.clone())
        }
        MatchPattern::StartsWith(value) if matches!(name, "text" | "description") => {
            (format!("{name}StartsWith"), value.clone())
        }
        MatchPattern::Contains(value) => (
            format!("{name}Matches"),
            format!(".*{}.*", regex::escape(value)),
        ),
        MatchPattern::StartsWith(value) => (
            format!("{name}Matches"),
            format!("^{}.*", regex::escape(value)),
        ),
        MatchPattern::EndsWith(value) => (
            format!("{name}Matches"),
            format!(".*{}$", regex::escape(value)),
        ),
    }
}

fn field_mask(name: &str) -> u32 {
    match name {
        "text" => 0x01,
        "textContains" => 0x02,
        "textMatches" => 0x04,
        "textStartsWith" => 0x08,
        "className" => 0x10,
        "classNameMatches" => 0x20,
        "description" => 0x40,
        "descriptionContains" => 0x80,
        "descriptionMatches" => 0x0100,
        "descriptionStartsWith" => 0x0200,
        "checkable" => 0x0400,
        "checked" => 0x0800,
        "clickable" => 0x1000,
        "longClickable" => 0x2000,
        "scrollable" => 0x4000,
        "enabled" => 0x8000,
        "focusable" => 0x010000,
        "focused" => 0x020000,
        "selected" => 0x040000,
        "packageName" => 0x080000,
        "packageNameMatches" => 0x100000,
        "resourceId" => 0x200000,
        "resourceIdMatches" => 0x400000,
        "index" => 0x800000,
        "instance" => 0x01000000,
        _ => 0,
    }
}

/// 控件引用。只保存 Selector、序号和 session generation。
#[derive(Clone, Debug)]
pub struct Element {
    pub(crate) driver: AndroidDriver,
    pub(crate) selector: Selector,
    pub(crate) index: usize,
    pub(crate) generation: u64,
}

impl Element {
    fn ensure_generation(&self) -> Result<()> {
        if self.driver.generation() == self.generation {
            Ok(())
        } else {
            Err(DriverError::SessionInvalid)
        }
    }

    async fn info(&self) -> Result<Value> {
        self.ensure_generation()?;
        self.driver
            .call_json_rpc("objInfo", json!([self.selector.value(self.index)]))
            .await
    }

    pub async fn attribute(&self, name: &str) -> Result<Value> {
        let info = self.info().await?;
        let actual = match name {
            "id" => "resourceName",
            "type" => "className",
            "description" => "contentDescription",
            "packageName" => "packageName",
            value => value,
        };
        info.get(actual)
            .cloned()
            .ok_or(DriverError::ElementNotFound)
    }
    pub async fn text(&self) -> Result<String> {
        value_string(self.attribute("text").await?)
    }
    pub async fn id(&self) -> Result<String> {
        value_string(self.attribute("id").await?)
    }
    pub async fn type_name(&self) -> Result<String> {
        value_string(self.attribute("type").await?)
    }
    pub async fn description(&self) -> Result<String> {
        value_string(self.attribute("description").await?)
    }
    pub async fn bounds(&self) -> Result<Bounds> {
        let value = self.attribute("bounds").await?;
        if let Some(object) = value.as_object() {
            let number = |name| {
                object
                    .get(name)
                    .and_then(Value::as_i64)
                    .and_then(|v| i32::try_from(v).ok())
            };
            return Ok(Bounds {
                left: number("left").ok_or(DriverError::ElementNotFound)?,
                top: number("top").ok_or(DriverError::ElementNotFound)?,
                right: number("right").ok_or(DriverError::ElementNotFound)?,
                bottom: number("bottom").ok_or(DriverError::ElementNotFound)?,
            });
        }
        value
            .as_str()
            .and_then(Bounds::parse)
            .ok_or(DriverError::ElementNotFound)
    }
    pub async fn center(&self) -> Result<Point> {
        Ok(self.bounds().await?.center())
    }
    pub async fn click(&self) -> Result<()> {
        self.ensure_generation()?;
        self.driver
            .call_json_rpc("click", json!([self.selector.value(self.index)]))
            .await
            .map(|_| ())
    }
    pub async fn long_click(&self) -> Result<()> {
        self.ensure_generation()?;
        self.driver
            .call_json_rpc("longClick", json!([self.selector.value(self.index)]))
            .await
            .map(|_| ())
    }
    pub async fn set_text(&self, text: &str) -> Result<()> {
        self.ensure_generation()?;
        self.driver
            .call_json_rpc("setText", json!([self.selector.value(self.index), text]))
            .await
            .map(|_| ())
    }
    pub async fn clear_text(&self) -> Result<()> {
        self.set_text("").await
    }
    pub async fn wait_until_gone(&self, timeout: Duration) -> Result<bool> {
        self.driver.wait_until_gone(&self.selector, timeout).await
    }
}

fn value_string(value: Value) -> Result<String> {
    value
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| DriverError::Protocol("控件属性不是字符串".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_official_uiautomator_selector_mask() {
        let value = Selector::new()
            .text(MatchPattern::Contains("新闻".into()))
            .clickable(true)
            .value(2);
        assert_eq!(value["textContains"], "新闻");
        assert_eq!(value["instance"], 2);
        assert_eq!(value["mask"], json!(0x02 | 0x1000 | 0x01000000));
        assert_eq!(value["childOrSibling"], json!([]));
    }
}
