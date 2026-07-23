use super::block_on;
use crate::{Bounds, Point, Result};
use tracing::trace;

#[derive(Clone, Debug)]
pub struct XPathElement {
    pub(super) inner: crate::XPathElement,
}
impl XPathElement {
    pub fn exists(&self) -> bool {
        trace!(target: "android_driver_rs::blocking", "阻塞 XPathElement::exists");
        self.inner.exists()
    }
    pub fn attribute(&self, name: &str) -> Option<&str> {
        trace!(target: "android_driver_rs::blocking", "阻塞 XPathElement::attribute");
        self.inner.attribute(name)
    }
    pub fn attributes(&self) -> &std::collections::BTreeMap<String, String> {
        self.inner.attributes()
    }
    pub fn bounds(&self) -> Option<Bounds> {
        trace!(target: "android_driver_rs::blocking", "阻塞 XPathElement::bounds");
        self.inner.bounds()
    }
    pub fn center(&self) -> Option<Point> {
        trace!(target: "android_driver_rs::blocking", "阻塞 XPathElement::center");
        self.inner.center()
    }
    pub fn text(&self) -> Option<&str> {
        trace!(target: "android_driver_rs::blocking", "阻塞 XPathElement::text");
        self.inner.text()
    }
    pub fn click(&self) -> Result<()> {
        trace!(target: "android_driver_rs::blocking", "阻塞 XPathElement::click");
        block_on(self.inner.click())?
    }
    pub fn long_click(&self) -> Result<()> {
        trace!(target: "android_driver_rs::blocking", "阻塞 XPathElement::long_click");
        block_on(self.inner.long_click())?
    }
    pub fn input_text(&self, text: &str) -> Result<()> {
        trace!(target: "android_driver_rs::blocking", "阻塞 XPathElement::input_text");
        block_on(self.inner.input_text(text))?
    }
}
