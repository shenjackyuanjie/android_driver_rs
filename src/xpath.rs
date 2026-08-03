//! 主机端 XPath 查询结果。

use crate::{AndroidDriver, Bounds, DriverError, Point, Result, UiNode};
use std::collections::BTreeMap;
use sxd_xpath::{Context, Factory, Value};

/// XPath 匹配节点的不可变快照。
///
/// 快照只保存抓取 UI 树那一刻的属性（含 `bounds` 坐标），并记录当时的会话代际。
/// 会话经 [`AndroidDriver::recover`] 重建后界面布局通常已经变化，快照里的坐标
/// 不再可信，因此所有会向设备发指令的方法都会先校验代际。
#[derive(Clone, Debug)]
pub struct XPathElement {
    pub(crate) driver: AndroidDriver,
    attributes: BTreeMap<String, String>,
    generation: u64,
}

impl XPathElement {
    fn ensure_generation(&self) -> Result<()> {
        if self.driver.generation() == self.generation {
            Ok(())
        } else {
            Err(DriverError::SessionInvalid)
        }
    }

    /// 快照是否仍属于当前会话。会话恢复后返回 `false`。
    pub fn exists(&self) -> bool {
        self.ensure_generation().is_ok()
    }
    pub fn attribute(&self, name: &str) -> Option<&str> {
        self.attributes.get(name).map(String::as_str)
    }
    pub fn attributes(&self) -> &BTreeMap<String, String> {
        &self.attributes
    }
    pub fn bounds(&self) -> Option<Bounds> {
        self.attribute("bounds").and_then(Bounds::parse)
    }
    pub fn center(&self) -> Option<Point> {
        self.bounds().map(Bounds::center)
    }
    pub fn text(&self) -> Option<&str> {
        self.attribute("text")
    }
    pub async fn click(&self) -> Result<()> {
        self.ensure_generation()?;
        self.driver
            .click(self.center().ok_or(DriverError::XPathNotFound)?)
            .await
    }
    pub async fn long_click(&self) -> Result<()> {
        self.ensure_generation()?;
        self.driver
            .long_click(self.center().ok_or(DriverError::XPathNotFound)?, 800)
            .await
    }
    pub async fn input_text(&self, text: &str) -> Result<()> {
        self.click().await?;
        self.driver.input_text(text).await
    }
}

pub(crate) fn evaluate(
    driver: AndroidDriver,
    root: &UiNode,
    expression: &str,
    generation: u64,
) -> Result<Vec<XPathElement>> {
    let package = sxd_document::parser::parse(&root.to_xml())
        .map_err(|error| DriverError::Protocol(format!("UI XML 无法用于 XPath：{error}")))?;
    let xpath = Factory::new()
        .build(expression)
        .map_err(|error| DriverError::InvalidXPath(error.to_string()))?
        .ok_or_else(|| DriverError::InvalidXPath("空表达式".into()))?;
    let value = xpath
        .evaluate(&Context::new(), package.as_document().root())
        .map_err(|error| DriverError::InvalidXPath(error.to_string()))?;
    let Value::Nodeset(nodes) = value else {
        return Err(DriverError::InvalidXPath("表达式结果必须是节点集合".into()));
    };
    Ok(nodes
        .document_order()
        .into_iter()
        .filter_map(|node| node.element())
        .map(|element| {
            let attributes = element
                .attributes()
                .iter()
                .map(|attribute| {
                    (
                        attribute.name().local_part().to_owned(),
                        attribute.value().to_owned(),
                    )
                })
                .collect();
            XPathElement {
                driver: driver.clone(),
                attributes,
                generation,
            }
        })
        .collect())
}
