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
use tokio::runtime::Runtime;

static RUNTIME: OnceLock<Runtime> = OnceLock::new();

pub(crate) fn block_on<F: Future>(future: F) -> Result<F::Output> {
    if tokio::runtime::Handle::try_current().is_ok() {
        return Err(DriverError::BlockingInAsyncContext);
    }
    let runtime = if let Some(runtime) = RUNTIME.get() {
        runtime
    } else {
        let runtime = Runtime::new().map_err(DriverError::Io)?;
        let _ = RUNTIME.set(runtime);
        RUNTIME.get().expect("runtime 已初始化")
    };
    Ok(runtime.block_on(future))
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn rejects_nested_blocking() {
        assert!(matches!(
            super::block_on(async { 1 }),
            Err(crate::DriverError::BlockingInAsyncContext)
        ));
    }
}
