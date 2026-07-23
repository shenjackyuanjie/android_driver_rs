use super::block_on;
use crate::{Bounds, Point, Result};
use serde_json::Value;
use tracing::trace;

#[derive(Debug)]
pub struct Element {
    pub(super) inner: crate::Element,
}

impl Element {
    pub fn attribute(&self, name: &str) -> Result<Value> {
        trace!(target: "android_driver_rs::blocking", "阻塞 Element::attribute");
        block_on(self.inner.attribute(name))?
    }
    pub fn text(&self) -> Result<String> {
        trace!(target: "android_driver_rs::blocking", "阻塞 Element::text");
        block_on(self.inner.text())?
    }
    pub fn id(&self) -> Result<String> {
        trace!(target: "android_driver_rs::blocking", "阻塞 Element::id");
        block_on(self.inner.id())?
    }
    pub fn type_name(&self) -> Result<String> {
        trace!(target: "android_driver_rs::blocking", "阻塞 Element::type_name");
        block_on(self.inner.type_name())?
    }
    pub fn description(&self) -> Result<String> {
        trace!(target: "android_driver_rs::blocking", "阻塞 Element::description");
        block_on(self.inner.description())?
    }
    pub fn bounds(&self) -> Result<Bounds> {
        trace!(target: "android_driver_rs::blocking", "阻塞 Element::bounds");
        block_on(self.inner.bounds())?
    }
    pub fn center(&self) -> Result<Point> {
        trace!(target: "android_driver_rs::blocking", "阻塞 Element::center");
        block_on(self.inner.center())?
    }
    pub fn click(&self) -> Result<()> {
        trace!(target: "android_driver_rs::blocking", "阻塞 Element::click");
        block_on(self.inner.click())?
    }
    pub fn long_click(&self) -> Result<()> {
        trace!(target: "android_driver_rs::blocking", "阻塞 Element::long_click");
        block_on(self.inner.long_click())?
    }
    pub fn set_text(&self, text: &str) -> Result<()> {
        trace!(target: "android_driver_rs::blocking", "阻塞 Element::set_text");
        block_on(self.inner.set_text(text))?
    }
    pub fn clear_text(&self) -> Result<()> {
        trace!(target: "android_driver_rs::blocking", "阻塞 Element::clear_text");
        self.set_text("")
    }
    pub fn wait_until_gone(&self, timeout: std::time::Duration) -> Result<bool> {
        trace!(target: "android_driver_rs::blocking", "阻塞 Element::wait_until_gone");
        block_on(self.inner.wait_until_gone(timeout))?
    }
}
