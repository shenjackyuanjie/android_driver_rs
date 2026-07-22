use std::path::PathBuf;
use std::time::Duration;

/// 驱动统一结果类型。
pub type Result<T> = std::result::Result<T, DriverError>;

/// 连接、传输或设备操作错误。
#[derive(Debug, thiserror::Error)]
pub enum DriverError {
    #[error("找不到 ADB 可执行文件；请设置 Builder 路径、ADB_PATH 或 PATH")]
    AdbNotFound,
    #[error("ADB 路径不是文件：{0}")]
    InvalidAdbPath(PathBuf),
    #[error("启动 ADB 失败：{0}")]
    AdbSpawn(#[source] std::io::Error),
    #[error("ADB 命令在 {timeout:?} 后超时")]
    AdbTimeout { timeout: Duration },
    #[error("ADB 命令失败（退出码 {code:?}）：{message}")]
    AdbCommand { code: Option<i32>, message: String },
    #[error("未发现在线 Android 设备")]
    DeviceNotFound,
    #[error("发现多台在线设备，请显式选择设备（数量：{count}）")]
    AmbiguousDevice { count: usize },
    #[error("所选设备不在线或未授权")]
    DeviceOffline,
    #[error("Agent 资源不存在：{0}")]
    AgentNotFound(PathBuf),
    #[error("Agent 资源校验失败：{0}")]
    AgentVerification(String),
    #[error("Agent 启动失败：{0}")]
    AgentStartup(String),
    #[error("无法建立 ADB forward：{0}")]
    Forward(String),
    #[error("清理 ADB forward tcp:{local_port} 失败：{source}")]
    ForwardCleanup {
        local_port: u16,
        #[source]
        source: Box<DriverError>,
    },
    #[error("RPC 连接失败：{0}")]
    RpcConnect(#[source] std::io::Error),
    #[error("RPC I/O 失败：{0}")]
    RpcIo(#[source] std::io::Error),
    #[error("RPC 请求在 {timeout:?} 后超时")]
    RpcTimeout { timeout: Duration },
    #[error("RPC 会话已失效；请调用 recover()")]
    SessionInvalid,
    #[error("JSON-RPC 返回错误：{0}")]
    Rpc(String),
    #[error("协议错误：{0}")]
    Protocol(String),
    #[error("JSON 解析失败：{0}")]
    Json(#[from] serde_json::Error),
    #[error("文件 I/O 失败：{0}")]
    Io(#[from] std::io::Error),
    #[error("应用或 Activity 标识不合法：{0}")]
    InvalidIdentifier(String),
    #[error("坐标无效：{0}")]
    InvalidCoordinate(String),
    #[error("控件不存在或已失效")]
    ElementNotFound,
    #[error("XPath 没有匹配节点")]
    XPathNotFound,
    #[error("XPath 表达式无效：{0}")]
    InvalidXPath(String),
    #[error("Driver 已关闭")]
    DriverClosed,
    #[error("不能在 Tokio 异步上下文中调用 blocking API")]
    BlockingInAsyncContext,
    #[error("截图数据无效：{0}")]
    InvalidScreenshot(String),
    #[error("辅助输入法不可用：{0}")]
    InputMethod(String),
}
