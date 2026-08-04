use super::*;

impl AndroidDriver {
    pub async fn ui_tree_xml(&self) -> Result<String> {
        trace!(target: "android_driver_rs::driver", "获取 UI 树 XML");
        let value = self
            .call_json_rpc("dumpWindowHierarchy", json!([false, 50]))
            .await?;
        value
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| DriverError::Protocol("UI 树响应不是 XML 字符串".into()))
    }
    pub async fn ui_tree(&self) -> Result<UiNode> {
        UiNode::parse(&self.ui_tree_xml().await?)
    }

    pub async fn find(&self, selector: &Selector) -> Result<Option<Element>> {
        trace!(target: "android_driver_rs::driver", ?selector, "查找元素");
        let exists = self
            .call_json_rpc("exist", json!([selector.value(0)]))
            .await?
            .as_bool()
            .unwrap_or(false);
        Ok(exists.then(|| Element {
            driver: self.clone(),
            selector: selector.clone(),
            index: 0,
            generation: self.generation(),
        }))
    }
    pub async fn find_all(&self, selector: &Selector) -> Result<Vec<Element>> {
        let count = self.count(selector).await?;
        Ok((0..count)
            .map(|index| Element {
                driver: self.clone(),
                selector: selector.clone(),
                index,
                generation: self.generation(),
            })
            .collect())
    }
    pub async fn exists(&self, selector: &Selector) -> Result<bool> {
        Ok(self.find(selector).await?.is_some())
    }
    pub async fn count(&self, selector: &Selector) -> Result<usize> {
        self.call_json_rpc("count", json!([selector.value(0)]))
            .await?
            .as_u64()
            .and_then(|value| usize::try_from(value).ok())
            .ok_or_else(|| DriverError::Protocol("Selector count 响应无效".into()))
    }
    pub async fn click_if_exists(&self, selector: &Selector) -> Result<bool> {
        if let Some(element) = self.find(selector).await? {
            element.click().await?;
            Ok(true)
        } else {
            Ok(false)
        }
    }
    pub async fn wait_for(&self, selector: &Selector, timeout: Duration) -> Result<Element> {
        trace!(target: "android_driver_rs::driver", ?selector, ?timeout, "等待元素");
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(element) = self.find(selector).await? {
                return Ok(element);
            }
            if Instant::now() >= deadline {
                return Err(DriverError::ElementNotFound);
            }
            sleep(self.inner.config.wait_interval).await;
        }
    }
    pub async fn wait_until_gone(&self, selector: &Selector, timeout: Duration) -> Result<bool> {
        let deadline = Instant::now() + timeout;
        loop {
            if !self.exists(selector).await? {
                return Ok(true);
            }
            if Instant::now() >= deadline {
                return Ok(false);
            }
            sleep(self.inner.config.wait_interval).await;
        }
    }
    pub async fn wait_until<F, Fut>(&self, timeout: Duration, mut condition: F) -> Result<bool>
    where
        F: FnMut() -> Fut,
        Fut: Future<Output = Result<bool>>,
    {
        trace!(target: "android_driver_rs::driver", ?timeout, "等待条件");
        let deadline = Instant::now() + timeout;
        loop {
            if condition().await? {
                return Ok(true);
            }
            if Instant::now() >= deadline {
                return Ok(false);
            }
            sleep(self.inner.config.wait_interval).await;
        }
    }

    pub async fn xpath_all(&self, expression: &str) -> Result<Vec<XPathElement>> {
        // 先取代际再抓树：若抓取期间发生 recover()，快照会被判定为过期而非静默陈旧。
        let generation = self.generation();
        crate::xpath::evaluate(self.clone(), &self.ui_tree().await?, expression, generation)
    }
    pub async fn xpath_optional(&self, expression: &str) -> Result<Option<XPathElement>> {
        Ok(self.xpath_all(expression).await?.into_iter().next())
    }
    pub async fn xpath(&self, expression: &str) -> Result<XPathElement> {
        self.xpath_optional(expression)
            .await?
            .ok_or(DriverError::XPathNotFound)
    }
    pub async fn xpath_exists(&self, expression: &str) -> Result<bool> {
        Ok(self.xpath_optional(expression).await?.is_some())
    }
    pub async fn wait_for_xpath(
        &self,
        expression: &str,
        timeout: Duration,
    ) -> Result<XPathElement> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(element) = self.xpath_optional(expression).await? {
                return Ok(element);
            }
            if Instant::now() >= deadline {
                return Err(DriverError::XPathNotFound);
            }
            sleep(self.inner.config.wait_interval).await;
        }
    }
}
