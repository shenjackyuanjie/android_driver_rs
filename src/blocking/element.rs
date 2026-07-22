use super::block_on;
use crate::{Bounds, Point, Result};
use serde_json::Value;

#[derive(Debug)]
pub struct Element {
    pub(super) inner: crate::Element,
}

impl Element {
    pub fn attribute(&self, name: &str) -> Result<Value> {
        block_on(self.inner.attribute(name))?
    }
    pub fn text(&self) -> Result<String> {
        block_on(self.inner.text())?
    }
    pub fn id(&self) -> Result<String> {
        block_on(self.inner.id())?
    }
    pub fn type_name(&self) -> Result<String> {
        block_on(self.inner.type_name())?
    }
    pub fn description(&self) -> Result<String> {
        block_on(self.inner.description())?
    }
    pub fn bounds(&self) -> Result<Bounds> {
        block_on(self.inner.bounds())?
    }
    pub fn center(&self) -> Result<Point> {
        block_on(self.inner.center())?
    }
    pub fn click(&self) -> Result<()> {
        block_on(self.inner.click())?
    }
    pub fn long_click(&self) -> Result<()> {
        block_on(self.inner.long_click())?
    }
    pub fn set_text(&self, text: &str) -> Result<()> {
        block_on(self.inner.set_text(text))?
    }
    pub fn clear_text(&self) -> Result<()> {
        block_on(self.inner.clear_text())?
    }
    pub fn wait_until_gone(&self, timeout: std::time::Duration) -> Result<bool> {
        block_on(self.inner.wait_until_gone(timeout))?
    }
}
