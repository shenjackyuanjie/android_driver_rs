//! 共享进程级 Tokio runtime 的阻塞门面。

mod driver;
mod element;
mod xpath;

pub use driver::{AndroidDriver, AndroidDriverBuilder};
pub use element::Element;
pub use xpath::XPathElement;

use crate::{DriverError, Result};
use std::future::Future;
use std::sync::OnceLock;
use std::time::{Duration, Instant};
use tokio::runtime::Runtime;
use tracing::{debug, trace};

static RUNTIME: OnceLock<Runtime> = OnceLock::new();

pub(crate) fn block_on<F: Future>(future: F) -> Result<F::Output> {
    if tokio::runtime::Handle::try_current().is_ok() {
        debug!(target: "android_driver_rs::blocking", "在异步上下文中调用 block_on，拒绝执行");
        return Err(DriverError::BlockingInAsyncContext);
    }
    let runtime = if let Some(runtime) = RUNTIME.get() {
        runtime
    } else {
        let runtime = Runtime::new().map_err(DriverError::Io)?;
        let _ = RUNTIME.set(runtime);
        RUNTIME.get().expect("runtime 已初始化")
    };
    trace!(target: "android_driver_rs::blocking", "block_on 执行 future");
    Ok(runtime.block_on(future))
}

pub(crate) fn wait_until<F>(timeout: Duration, interval: Duration, mut condition: F) -> Result<bool>
where
    F: FnMut() -> Result<bool>,
{
    let deadline = Instant::now() + timeout;
    loop {
        if condition()? {
            return Ok(true);
        }
        if Instant::now() >= deadline {
            return Ok(false);
        }
        let now = Instant::now();
        if now < deadline {
            std::thread::sleep(std::cmp::min(interval, deadline - now));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn blocking_wait_condition_runs_outside_tokio_context() {
        let result = wait_until(Duration::from_millis(20), Duration::from_millis(1), || {
            assert!(tokio::runtime::Handle::try_current().is_err());
            Ok(true)
        });
        assert!(result.unwrap());
    }

    #[tokio::test]
    async fn rejects_nested_blocking() {
        assert!(matches!(
            super::block_on(async { 1 }),
            Err(crate::DriverError::BlockingInAsyncContext)
        ));
    }
}
