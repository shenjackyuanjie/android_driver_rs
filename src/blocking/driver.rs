use super::{Element, XPathElement, block_on};
use crate::{
    ActivityName, AdbConfig, AgentProfile, AgentSource, AndroidDriver as AsyncDriver,
    AndroidDriverBuilder as AsyncBuilder, AndroidKeyCode, AppIdentifier, DeviceDescriptor,
    DeviceInfo, DeviceSelector, DisplaySize, DriverConfig, Point, Position, Result, ScreenState,
    ScreenshotMethod, Selector, UiNode,
};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Clone, Debug, Default)]
pub struct AndroidDriverBuilder {
    inner: AsyncBuilder,
}

impl AndroidDriverBuilder {
    pub fn device(mut self, value: DeviceSelector) -> Self {
        self.inner = self.inner.device(value);
        self
    }
    pub fn adb_path(mut self, value: impl Into<PathBuf>) -> Self {
        self.inner = self.inner.adb_path(value);
        self
    }
    pub fn adb_server(mut self, host: impl Into<String>, port: u16) -> Self {
        self.inner = self.inner.adb_server(host, port);
        self
    }
    pub fn adb_config(mut self, value: AdbConfig) -> Self {
        self.inner = self.inner.adb_config(value);
        self
    }
    pub fn agent_source(mut self, value: AgentSource) -> Self {
        self.inner = self.inner.agent_source(value);
        self
    }
    pub fn driver_config(mut self, value: DriverConfig) -> Self {
        self.inner = self.inner.driver_config(value);
        self
    }
    pub fn connect(self) -> Result<AndroidDriver> {
        block_on(self.inner.connect())?.map(|inner| AndroidDriver { inner })
    }
}

#[derive(Clone, Debug)]
pub struct AndroidDriver {
    inner: AsyncDriver,
}

impl AndroidDriver {
    pub fn builder() -> AndroidDriverBuilder {
        AndroidDriverBuilder::default()
    }
    pub fn discover_devices(config: AdbConfig) -> Result<Vec<DeviceDescriptor>> {
        block_on(AsyncDriver::discover_devices(config))?
    }
    pub fn agent_profile(&self) -> &AgentProfile {
        self.inner.agent_profile()
    }
    pub fn generation(&self) -> u64 {
        self.inner.generation()
    }
    pub fn recover(&self) -> Result<()> {
        block_on(self.inner.recover())?
    }
    pub fn close(&self) -> Result<()> {
        block_on(self.inner.close())?
    }
    pub fn call_json_rpc(&self, method: &str, params: Value) -> Result<Value> {
        block_on(self.inner.call_json_rpc(method, params))?
    }
    pub fn display_size(&self) -> Result<DisplaySize> {
        block_on(self.inner.display_size())?
    }
    pub fn device_info(&self) -> Result<DeviceInfo> {
        block_on(self.inner.device_info())?
    }
    pub fn screen_state(&self) -> Result<ScreenState> {
        block_on(self.inner.screen_state())?
    }
    pub fn screen_on(&self) -> Result<()> {
        block_on(self.inner.screen_on())?
    }
    pub fn screen_off(&self) -> Result<()> {
        block_on(self.inner.screen_off())?
    }
    pub fn unlock(&self) -> Result<()> {
        block_on(self.inner.unlock())?
    }
    pub fn press_key(&self, key: impl Into<AndroidKeyCode>) -> Result<()> {
        block_on(self.inner.press_key(key))?
    }
    pub fn go_back(&self) -> Result<()> {
        block_on(self.inner.go_back())?
    }
    pub fn go_home(&self) -> Result<()> {
        block_on(self.inner.go_home())?
    }
    pub fn click(&self, point: Point) -> Result<()> {
        block_on(self.inner.click(point))?
    }
    pub fn click_position(&self, position: Position) -> Result<()> {
        block_on(self.inner.click_position(position))?
    }
    pub fn long_click(&self, point: Point, duration_ms: u32) -> Result<()> {
        block_on(self.inner.long_click(point, duration_ms))?
    }
    pub fn swipe(&self, from: Point, to: Point, duration_ms: u32) -> Result<()> {
        block_on(self.inner.swipe(from, to, duration_ms))?
    }
    pub fn swipe_positions(&self, from: Position, to: Position, duration_ms: u32) -> Result<()> {
        block_on(self.inner.swipe_positions(from, to, duration_ms))?
    }
    pub fn input_text(&self, text: &str) -> Result<()> {
        block_on(self.inner.input_text(text))?
    }
    pub fn start_app(
        &self,
        package: &AppIdentifier,
        activity: Option<&ActivityName>,
    ) -> Result<()> {
        block_on(self.inner.start_app(package, activity))?
    }
    pub fn stop_app(&self, package: &AppIdentifier) -> Result<()> {
        block_on(self.inner.stop_app(package))?
    }
    pub fn resolve_activity(&self, package: &AppIdentifier) -> Result<String> {
        block_on(self.inner.resolve_activity(package))?
    }
    pub fn current_app(&self) -> Result<Option<(AppIdentifier, ActivityName)>> {
        block_on(self.inner.current_app())?
    }
    pub fn screenshot(&self) -> Result<Vec<u8>> {
        block_on(self.inner.screenshot())?
    }
    pub fn screenshot_with_method(&self, method: ScreenshotMethod) -> Result<Vec<u8>> {
        block_on(self.inner.screenshot_with_method(method))?
    }
    pub fn screenshot_to(&self, path: impl AsRef<Path>) -> Result<()> {
        block_on(self.inner.screenshot_to(path))?
    }
    pub fn ui_tree_xml(&self) -> Result<String> {
        block_on(self.inner.ui_tree_xml())?
    }
    pub fn ui_tree(&self) -> Result<UiNode> {
        block_on(self.inner.ui_tree())?
    }
    pub fn find(&self, selector: &Selector) -> Result<Option<Element>> {
        Ok(block_on(self.inner.find(selector))??.map(|inner| Element { inner }))
    }
    pub fn find_all(&self, selector: &Selector) -> Result<Vec<Element>> {
        Ok(block_on(self.inner.find_all(selector))??
            .into_iter()
            .map(|inner| Element { inner })
            .collect())
    }
    pub fn exists(&self, selector: &Selector) -> Result<bool> {
        block_on(self.inner.exists(selector))?
    }
    pub fn count(&self, selector: &Selector) -> Result<usize> {
        block_on(self.inner.count(selector))?
    }
    pub fn click_if_exists(&self, selector: &Selector) -> Result<bool> {
        block_on(self.inner.click_if_exists(selector))?
    }
    pub fn wait_for(&self, selector: &Selector, timeout: Duration) -> Result<Element> {
        block_on(self.inner.wait_for(selector, timeout))?.map(|inner| Element { inner })
    }
    pub fn wait_for_xpath(&self, expression: &str, timeout: Duration) -> Result<XPathElement> {
        block_on(self.inner.wait_for_xpath(expression, timeout))?
            .map(|inner| XPathElement { inner })
    }
    pub fn xpath(&self, expression: &str) -> Result<XPathElement> {
        block_on(self.inner.xpath(expression))?.map(|inner| XPathElement { inner })
    }
    pub fn xpath_optional(&self, expression: &str) -> Result<Option<XPathElement>> {
        Ok(block_on(self.inner.xpath_optional(expression))??.map(|inner| XPathElement { inner }))
    }
    pub fn xpath_all(&self, expression: &str) -> Result<Vec<XPathElement>> {
        Ok(block_on(self.inner.xpath_all(expression))??
            .into_iter()
            .map(|inner| XPathElement { inner })
            .collect())
    }
    pub fn xpath_exists(&self, expression: &str) -> Result<bool> {
        block_on(self.inner.xpath_exists(expression))?
    }
}
