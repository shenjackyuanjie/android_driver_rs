//! 单会话、单在途请求的 HTTP JSON-RPC 2.0 客户端。

use crate::{DriverError, Result};
use serde_json::{Value, json};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::Mutex;
use tokio::time::timeout;

#[derive(Clone)]
pub(crate) struct RpcClient {
    inner: Arc<RpcInner>,
}

struct RpcInner {
    port: u16,
    timeout: Duration,
    max_size: usize,
    id: AtomicU64,
    valid: AtomicBool,
    in_flight: Mutex<()>,
}

impl RpcClient {
    pub fn new(port: u16, timeout: Duration, max_size: usize) -> Self {
        Self {
            inner: Arc::new(RpcInner {
                port,
                timeout,
                max_size,
                id: AtomicU64::new(1),
                valid: AtomicBool::new(true),
                in_flight: Mutex::new(()),
            }),
        }
    }

    pub fn is_valid(&self) -> bool {
        self.inner.valid.load(Ordering::Acquire)
    }
    pub fn invalidate(&self) {
        self.inner.valid.store(false, Ordering::Release);
    }

    pub async fn call(&self, method: &str, params: Value) -> Result<Value> {
        if !self.is_valid() {
            return Err(DriverError::SessionInvalid);
        }
        let _lock = self.inner.in_flight.lock().await;
        if !self.is_valid() {
            return Err(DriverError::SessionInvalid);
        }
        let mut guard = InFlightGuard {
            client: self.clone(),
            complete: false,
        };
        let id = self.inner.id.fetch_add(1, Ordering::Relaxed);
        let result = timeout(self.inner.timeout, self.exchange(id, method, params)).await;
        let result = match result {
            Ok(value) => value,
            Err(_) => Err(DriverError::RpcTimeout {
                timeout: self.inner.timeout,
            }),
        };
        match &result {
            Ok(_) | Err(DriverError::Rpc(_)) => guard.complete = true,
            Err(_) => {}
        }
        result
    }

    async fn exchange(&self, id: u64, method: &str, params: Value) -> Result<Value> {
        let body = serde_json::to_vec(
            &json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }),
        )?;
        if body.len() > self.inner.max_size {
            return Err(DriverError::Protocol("RPC 请求超过 8 MiB 上限".into()));
        }
        let mut stream = TcpStream::connect(("127.0.0.1", self.inner.port))
            .await
            .map_err(DriverError::RpcConnect)?;
        let header = format!(
            "POST /jsonrpc/0 HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            self.inner.port,
            body.len()
        );
        stream
            .write_all(header.as_bytes())
            .await
            .map_err(DriverError::RpcIo)?;
        stream.write_all(&body).await.map_err(DriverError::RpcIo)?;
        let response = read_http_message(&mut stream, self.inner.max_size).await?;
        parse_http_response(id, &response, self.inner.max_size)
    }
}

struct InFlightGuard {
    client: RpcClient,
    complete: bool,
}
impl Drop for InFlightGuard {
    fn drop(&mut self) {
        if !self.complete {
            self.client.invalidate();
        }
    }
}

fn parse_http_response(id: u64, response: &[u8], max_size: usize) -> Result<Value> {
    let split = response
        .windows(4)
        .position(|value| value == b"\r\n\r\n")
        .ok_or_else(|| DriverError::Protocol("HTTP 响应缺少头部终止符".into()))?;
    let headers = std::str::from_utf8(&response[..split])
        .map_err(|_| DriverError::Protocol("HTTP 头不是 UTF-8".into()))?;
    let status = headers.lines().next().unwrap_or_default();
    if !status.contains(" 200 ") {
        return Err(DriverError::Protocol(format!("HTTP 状态无效：{status}")));
    }
    let length = content_length(headers)?;
    if length > max_size {
        return Err(DriverError::Protocol("RPC JSON 超过 8 MiB 上限".into()));
    }
    if response.len().saturating_sub(split + 4) < length {
        return Err(DriverError::Protocol("HTTP 响应正文不完整".into()));
    }
    let body = &response[split + 4..split + 4 + length];
    let value: Value = serde_json::from_slice(body)?;
    if value.get("id").and_then(Value::as_u64) != Some(id) {
        return Err(DriverError::Protocol("JSON-RPC id 不匹配".into()));
    }
    if let Some(error) = value.get("error").filter(|value| !value.is_null()) {
        return Err(DriverError::Rpc(error.to_string()));
    }
    value
        .get("result")
        .cloned()
        .ok_or_else(|| DriverError::Protocol("JSON-RPC 响应缺少 result".into()))
}

fn response_complete(response: &[u8], max_size: usize) -> Result<bool> {
    let Some(split) = response.windows(4).position(|value| value == b"\r\n\r\n") else {
        if response.len() > 16 * 1024 {
            return Err(DriverError::Protocol("HTTP 响应头超过 16 KiB".into()));
        }
        return Ok(false);
    };
    let headers = std::str::from_utf8(&response[..split])
        .map_err(|_| DriverError::Protocol("HTTP 头不是 UTF-8".into()))?;
    let length = content_length(headers)?;
    if length > max_size {
        return Err(DriverError::Protocol("RPC JSON 超过 8 MiB 上限".into()));
    }
    Ok(response.len().saturating_sub(split + 4) >= length)
}

async fn read_http_message(stream: &mut TcpStream, max_size: usize) -> Result<Vec<u8>> {
    let mut response = Vec::new();
    let mut buffer = [0_u8; 8192];
    loop {
        let read = stream.read(&mut buffer).await.map_err(DriverError::RpcIo)?;
        if read == 0 {
            break;
        }
        response.extend_from_slice(&buffer[..read]);
        if response.len() > max_size + 16 * 1024 {
            return Err(DriverError::Protocol("HTTP 响应超过大小上限".into()));
        }
        if response_complete(&response, max_size)? {
            break;
        }
    }
    Ok(response)
}

fn content_length(headers: &str) -> Result<usize> {
    headers
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())
                .flatten()
        })
        .ok_or_else(|| DriverError::Protocol("HTTP 响应缺少 Content-Length".into()))
}

pub(crate) async fn ping(port: u16, duration: Duration) -> bool {
    timeout(duration, async move {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).await.ok()?;
        stream
            .write_all(b"GET /ping HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
            .await
            .ok()?;
        let response = read_http_message(&mut stream, 1024).await.ok()?;
        let text = String::from_utf8_lossy(&response).to_ascii_lowercase();
        (text.contains(" 200 ") && (text.contains("pong") || text.contains("success")))
            .then_some(())
    })
    .await
    .ok()
    .flatten()
    .is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;

    #[tokio::test]
    async fn parses_json_rpc_and_increments_ids() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            for expected in 1..=2 {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = [0_u8; 1024];
                let _ = socket.read(&mut request).await.unwrap();
                let body = format!(r#"{{"jsonrpc":"2.0","id":{expected},"result":{expected}}}"#);
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                socket.write_all(response.as_bytes()).await.unwrap();
            }
        });
        let client = RpcClient::new(port, Duration::from_secs(1), 1024);
        assert_eq!(client.call("x", json!([])).await.unwrap(), json!(1));
        assert_eq!(client.call("x", json!([])).await.unwrap(), json!(2));
    }
}
