use super::block_on;
use crate::{Bounds, Point, Result};

#[derive(Clone, Debug)]
pub struct XPathElement {
    pub(super) inner: crate::XPathElement,
}
impl XPathElement {
    pub fn exists(&self) -> bool {
        self.inner.exists()
    }
    pub fn attribute(&self, name: &str) -> Option<&str> {
        self.inner.attribute(name)
    }
    pub fn attributes(&self) -> &std::collections::BTreeMap<String, String> {
        self.inner.attributes()
    }
    pub fn bounds(&self) -> Option<Bounds> {
        self.inner.bounds()
    }
    pub fn center(&self) -> Option<Point> {
        self.inner.center()
    }
    pub fn text(&self) -> Option<&str> {
        self.inner.text()
    }
    pub fn click(&self) -> Result<()> {
        block_on(self.inner.click())?
    }
    pub fn long_click(&self) -> Result<()> {
        block_on(self.inner.long_click())?
    }
    pub fn input_text(&self, text: &str) -> Result<()> {
        block_on(self.inner.input_text(text))?
    }
}
