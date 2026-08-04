use super::{Element, XPathElement, block_on};
use crate::{
    ActivityName, AdbConfig, AgentProfile, AgentSource, AndroidDriver as AsyncDriver,
    AndroidDriverBuilder as AsyncBuilder, AndroidKeyCode, AppIdentifier, DeviceDescriptor,
    DeviceInfo, DeviceSelector, DisplaySize, DriverConfig, Point, Position, Result, ScreenState,
    ScreenshotMethod, Selector, UiAutomationConflictPolicy, UiNode,
};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tracing::trace;

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
    pub fn ui_automation_conflict_policy(mut self, value: UiAutomationConflictPolicy) -> Self {
        self.inner = self.inner.ui_automation_conflict_policy(value);
        self
    }
    pub fn connect(self) -> Result<AndroidDriver> {
        trace!(target: "android_driver_rs::blocking", "阻塞 AndroidDriver::connect");
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
        trace!(target: "android_driver_rs::blocking", "阻塞 AndroidDriver::recover");
        block_on(self.inner.recover())?
    }
    pub fn close(&self) -> Result<()> {
        trace!(target: "android_driver_rs::blocking", "阻塞 AndroidDriver::close");
        block_on(self.inner.close())?
    }
    pub fn call_json_rpc(&self, method: &str, params: Value) -> Result<Value> {
        trace!(target: "android_driver_rs::blocking", "阻塞 call_json_rpc");
        block_on(self.inner.call_json_rpc(method, params))?
    }
    pub fn display_size(&self) -> Result<DisplaySize> {
        trace!(target: "android_driver_rs::blocking", "阻塞 AndroidDriver::display_size");
        block_on(self.inner.display_size())?
    }
    pub fn device_info(&self) -> Result<DeviceInfo> {
        trace!(target: "android_driver_rs::blocking", "阻塞 AndroidDriver::device_info");
        block_on(self.inner.device_info())?
    }
    pub fn screen_state(&self) -> Result<ScreenState> {
        trace!(target: "android_driver_rs::blocking", "阻塞 AndroidDriver::screen_state");
        block_on(self.inner.screen_state())?
    }
    pub fn screen_on(&self) -> Result<()> {
        trace!(target: "android_driver_rs::blocking", "阻塞 AndroidDriver::screen_on");
        block_on(self.inner.screen_on())?
    }
    pub fn screen_off(&self) -> Result<()> {
        trace!(target: "android_driver_rs::blocking", "阻塞 AndroidDriver::screen_off");
        block_on(self.inner.screen_off())?
    }
    pub fn unlock(&self) -> Result<()> {
        trace!(target: "android_driver_rs::blocking", "阻塞 AndroidDriver::unlock");
        block_on(self.inner.unlock())?
    }
    pub fn mute_media(&self) -> Result<()> {
        trace!(target: "android_driver_rs::blocking", "阻塞 AndroidDriver::mute_media");
        block_on(self.inner.mute_media())?
    }
    pub fn press_key(&self, key: impl Into<AndroidKeyCode>) -> Result<()> {
        trace!(target: "android_driver_rs::blocking", "阻塞 AndroidDriver::press_key");
        block_on(self.inner.press_key(key))?
    }
    pub fn go_back(&self) -> Result<()> {
        trace!(target: "android_driver_rs::blocking", "阻塞 AndroidDriver::go_back");
        block_on(self.inner.go_back())?
    }
    pub fn go_home(&self) -> Result<()> {
        trace!(target: "android_driver_rs::blocking", "阻塞 AndroidDriver::go_home");
        block_on(self.inner.go_home())?
    }
    pub fn click(&self, point: Point) -> Result<()> {
        trace!(target: "android_driver_rs::blocking", "阻塞 AndroidDriver::click");
        block_on(self.inner.click(point))?
    }
    pub fn click_position(&self, position: Position) -> Result<()> {
        trace!(target: "android_driver_rs::blocking", "阻塞 AndroidDriver::click_position");
        block_on(self.inner.click_position(position))?
    }
    pub fn long_click(&self, point: Point, duration_ms: u32) -> Result<()> {
        trace!(target: "android_driver_rs::blocking", "阻塞 AndroidDriver::long_click");
        block_on(self.inner.long_click(point, duration_ms))?
    }
    pub fn swipe(&self, from: Point, to: Point, duration_ms: u32) -> Result<()> {
        trace!(target: "android_driver_rs::blocking", "阻塞 AndroidDriver::swipe");
        block_on(self.inner.swipe(from, to, duration_ms))?
    }
    pub fn swipe_positions(&self, from: Position, to: Position, duration_ms: u32) -> Result<()> {
        trace!(target: "android_driver_rs::blocking", "阻塞 AndroidDriver::swipe_positions");
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
        trace!(target: "android_driver_rs::blocking", "阻塞 AndroidDriver::start_app");
        block_on(self.inner.start_app(package, activity))?
    }
    pub fn stop_app(&self, package: &AppIdentifier) -> Result<()> {
        trace!(target: "android_driver_rs::blocking", "阻塞 AndroidDriver::stop_app");
        block_on(self.inner.stop_app(package))?
    }
    pub fn resolve_activity(&self, package: &AppIdentifier) -> Result<String> {
        trace!(target: "android_driver_rs::blocking", "阻塞 AndroidDriver::resolve_activity");
        block_on(self.inner.resolve_activity(package))?
    }
    pub fn current_app(&self) -> Result<Option<(AppIdentifier, ActivityName)>> {
        trace!(target: "android_driver_rs::blocking", "阻塞 AndroidDriver::current_app");
        block_on(self.inner.current_app())?
    }
    pub fn screenshot(&self) -> Result<Vec<u8>> {
        trace!(target: "android_driver_rs::blocking", "阻塞 AndroidDriver::screenshot");
        block_on(self.inner.screenshot())?
    }
    pub fn screenshot_with_method(&self, method: ScreenshotMethod) -> Result<Vec<u8>> {
        trace!(target: "android_driver_rs::blocking", "阻塞 AndroidDriver::screenshot_with_method");
        block_on(self.inner.screenshot_with_method(method))?
    }
    pub fn screenshot_to(&self, path: impl AsRef<Path>) -> Result<()> {
        trace!(target: "android_driver_rs::blocking", "阻塞 AndroidDriver::screenshot_to");
        block_on(self.inner.screenshot_to(path))?
    }
    pub fn ui_tree_xml(&self) -> Result<String> {
        trace!(target: "android_driver_rs::blocking", "阻塞 AndroidDriver::ui_tree_xml");
        block_on(self.inner.ui_tree_xml())?
    }
    pub fn ui_tree(&self) -> Result<UiNode> {
        trace!(target: "android_driver_rs::blocking", "阻塞 AndroidDriver::ui_tree");
        block_on(self.inner.ui_tree())?
    }
    pub fn find(&self, selector: &Selector) -> Result<Option<Element>> {
        trace!(target: "android_driver_rs::blocking", "阻塞 AndroidDriver::find");
        Ok(block_on(self.inner.find(selector))??.map(|inner| Element { inner }))
    }
    pub fn find_all(&self, selector: &Selector) -> Result<Vec<Element>> {
        trace!(target: "android_driver_rs::blocking", "阻塞 AndroidDriver::find_all");
        Ok(block_on(self.inner.find_all(selector))??
            .into_iter()
            .map(|inner| Element { inner })
            .collect())
    }
    pub fn exists(&self, selector: &Selector) -> Result<bool> {
        trace!(target: "android_driver_rs::blocking", "阻塞 AndroidDriver::exists");
        block_on(self.inner.exists(selector))?
    }
    pub fn count(&self, selector: &Selector) -> Result<usize> {
        trace!(target: "android_driver_rs::blocking", "阻塞 AndroidDriver::count");
        block_on(self.inner.count(selector))?
    }
    pub fn click_if_exists(&self, selector: &Selector) -> Result<bool> {
        trace!(target: "android_driver_rs::blocking", "阻塞 AndroidDriver::click_if_exists");
        block_on(self.inner.click_if_exists(selector))?
    }
    pub fn wait_for(&self, selector: &Selector, timeout: Duration) -> Result<Element> {
        trace!(target: "android_driver_rs::blocking", "阻塞 AndroidDriver::wait_for");
        block_on(self.inner.wait_for(selector, timeout))?.map(|inner| Element { inner })
    }
    pub fn wait_until_gone(&self, selector: &Selector, timeout: Duration) -> Result<bool> {
        trace!(target: "android_driver_rs::blocking", "阻塞 AndroidDriver::wait_until_gone");
        block_on(self.inner.wait_until_gone(selector, timeout))?
    }
    pub fn wait_until<F>(&self, timeout: Duration, condition: F) -> Result<bool>
    where
        F: FnMut() -> Result<bool>,
    {
        trace!(target: "android_driver_rs::blocking", "阻塞 AndroidDriver::wait_until");
        super::wait_until(timeout, self.inner.wait_interval(), condition)
    }
    pub fn wait_for_xpath(&self, expression: &str, timeout: Duration) -> Result<XPathElement> {
        trace!(target: "android_driver_rs::blocking", "阻塞 AndroidDriver::wait_for_xpath");
        block_on(self.inner.wait_for_xpath(expression, timeout))?
            .map(|inner| XPathElement { inner })
    }
    pub fn xpath(&self, expression: &str) -> Result<XPathElement> {
        trace!(target: "android_driver_rs::blocking", "阻塞 AndroidDriver::xpath");
        block_on(self.inner.xpath(expression))?.map(|inner| XPathElement { inner })
    }
    pub fn xpath_optional(&self, expression: &str) -> Result<Option<XPathElement>> {
        trace!(target: "android_driver_rs::blocking", "阻塞 AndroidDriver::xpath_optional");
        Ok(block_on(self.inner.xpath_optional(expression))??.map(|inner| XPathElement { inner }))
    }
    pub fn xpath_all(&self, expression: &str) -> Result<Vec<XPathElement>> {
        trace!(target: "android_driver_rs::blocking", "阻塞 AndroidDriver::xpath_all");
        Ok(block_on(self.inner.xpath_all(expression))??
            .into_iter()
            .map(|inner| XPathElement { inner })
            .collect())
    }
    pub fn xpath_exists(&self, expression: &str) -> Result<bool> {
        trace!(target: "android_driver_rs::blocking", "阻塞 AndroidDriver::xpath_exists");
        block_on(self.inner.xpath_exists(expression))?
    }
}
