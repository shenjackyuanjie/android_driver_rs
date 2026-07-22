//! Android UI XML 的主机端树模型。

use crate::{Bounds, DriverError, Result};
use quick_xml::Reader;
use quick_xml::events::{BytesStart, Event};
use std::collections::BTreeMap;

/// UI 树节点。属性名已经规范化为跨 Android 调用稳定的名称。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UiNode {
    attributes: BTreeMap<String, String>,
    pub children: Vec<UiNode>,
}

impl UiNode {
    pub fn attribute(&self, name: &str) -> Option<String> {
        self.attributes.get(name).cloned()
    }
    pub fn attributes(&self) -> &BTreeMap<String, String> {
        &self.attributes
    }
    pub fn node_type(&self) -> Option<String> {
        self.attribute("type")
    }
    pub fn bounds(&self) -> Option<Bounds> {
        self.attributes
            .get("bounds")
            .and_then(|value| Bounds::parse(value))
    }

    pub(crate) fn parse(xml: &str) -> Result<Self> {
        let mut reader = Reader::from_str(xml);
        reader.config_mut().trim_text(true);
        let mut stack = Vec::new();
        let mut roots = Vec::new();
        loop {
            match reader.read_event() {
                Ok(Event::Start(event)) => stack.push(from_event(&event, reader.decoder())?),
                Ok(Event::Empty(event)) => attach(
                    from_event(&event, reader.decoder())?,
                    &mut stack,
                    &mut roots,
                ),
                Ok(Event::End(_)) => {
                    let node = stack
                        .pop()
                        .ok_or_else(|| DriverError::Protocol("UI XML 结束标签不匹配".into()))?;
                    attach(node, &mut stack, &mut roots);
                }
                Ok(Event::Eof) => break,
                Err(error) => {
                    return Err(DriverError::Protocol(format!("UI XML 无法解析：{error}")));
                }
                _ => {}
            }
        }
        if !stack.is_empty() {
            return Err(DriverError::Protocol("UI XML 未完整结束".into()));
        }
        match roots.len() {
            0 => Err(DriverError::Protocol("UI XML 为空".into())),
            1 => Ok(roots.remove(0)),
            _ => Ok(Self {
                attributes: BTreeMap::from([("type".into(), "hierarchy".into())]),
                children: roots,
            }),
        }
    }

    pub(crate) fn to_xml(&self) -> String {
        let mut output = String::new();
        self.write_xml(&mut output);
        output
    }

    fn write_xml(&self, output: &mut String) {
        output.push_str("<node");
        for (name, value) in &self.attributes {
            output.push(' ');
            output.push_str(name);
            output.push_str("=\"");
            output.push_str(&escape(value));
            output.push('"');
        }
        if self.children.is_empty() {
            output.push_str("/>");
            return;
        }
        output.push('>');
        for child in &self.children {
            child.write_xml(output);
        }
        output.push_str("</node>");
    }
}

fn attach(node: UiNode, stack: &mut [UiNode], roots: &mut Vec<UiNode>) {
    if let Some(parent) = stack.last_mut() {
        parent.children.push(node);
    } else {
        roots.push(node);
    }
}

fn from_event(event: &BytesStart<'_>, decoder: quick_xml::encoding::Decoder) -> Result<UiNode> {
    let mut attributes = BTreeMap::new();
    for attribute in event.attributes() {
        let attribute = attribute
            .map_err(|error| DriverError::Protocol(format!("UI XML 属性无效：{error}")))?;
        let name = String::from_utf8_lossy(attribute.key.as_ref()).into_owned();
        let value = attribute
            .decoded_and_normalized_value(quick_xml::XmlVersion::Implicit1_0, decoder)
            .map_err(|error| DriverError::Protocol(format!("UI XML 属性无效：{error}")))?
            .into_owned();
        attributes.insert(normalize_name(&name).to_owned(), value);
    }
    let tag = String::from_utf8_lossy(event.name().as_ref()).into_owned();
    attributes.entry("type".into()).or_insert(tag);
    Ok(UiNode {
        attributes,
        children: Vec::new(),
    })
}

fn normalize_name(name: &str) -> &str {
    match name {
        "resource-id" => "id",
        "class" => "type",
        "content-desc" => "description",
        "package" => "packageName",
        value => value,
    }
}

fn escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_android_attributes() {
        let tree = UiNode::parse(r#"<hierarchy><node resource-id="com.demo:id/title" class="android.widget.TextView" content-desc="标题" package="com.demo" bounds="[1,2][30,40]"/></hierarchy>"#).unwrap();
        let node = &tree.children[0];
        assert_eq!(node.attribute("id").as_deref(), Some("com.demo:id/title"));
        assert_eq!(node.node_type().as_deref(), Some("android.widget.TextView"));
        assert_eq!(node.bounds().unwrap().center().x, 15);
    }
}
