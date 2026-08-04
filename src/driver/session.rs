use super::*;

pub(super) async fn deploy_jar(adb: &AdbRunner, local: &Path) -> Result<()> {
    debug!(
        target: "android_driver_rs::driver",
        local = %local.display(),
        expected_size = agent::JAR_SIZE,
        expected_sha256 = agent::JAR_SHA256,
        "部署 Agent JAR"
    );
    adb.shell(["mkdir", "-p", REMOTE_DIR]).await?;
    match inspect_remote_file(adb, REMOTE_JAR, "部署前").await {
        Ok(current) if remote_file_matches(&current) => {
            debug!(target: "android_driver_rs::driver", "Agent JAR 已就绪，跳过部署");
            return Ok(());
        }
        Ok(_) => {
            debug!(target: "android_driver_rs::driver", "设备端现有 Agent JAR 不匹配，重新部署");
        }
        Err(error) => {
            debug!(target: "android_driver_rs::driver", error = %error, "无法检查设备端现有 Agent JAR，继续部署");
        }
    }

    let temporary = format!("{REMOTE_JAR}.{}.tmp", std::process::id());
    let push = adb
        .run_text(
            [
                OsString::from("push"),
                local.as_os_str().to_os_string(),
                OsString::from(&temporary),
            ],
            adb.transfer_timeout(),
        )
        .await?;
    debug!(
        target: "android_driver_rs::driver",
        remote = temporary,
        stdout = ?push.stdout.trim(),
        stderr = ?push.stderr.trim(),
        "Agent JAR 推送完成"
    );

    adb.shell(["chmod", "0644", &temporary]).await?;
    let pushed = inspect_remote_file(adb, &temporary, "push 后").await?;
    verify_remote_digest(&pushed, "临时")?;

    adb.shell(["mv", &temporary, REMOTE_JAR]).await?;
    let published = inspect_remote_file(adb, REMOTE_JAR, "mv 后").await?;
    verify_remote_digest(&published, "正式")?;
    info!(target: "android_driver_rs::driver", "Agent JAR 部署完成");
    Ok(())
}

struct RemoteFileInfo {
    digest: Option<String>,
    size: Option<u64>,
    exists: bool,
}

async fn inspect_remote_file(
    adb: &AdbRunner,
    remote: &str,
    stage: &'static str,
) -> Result<RemoteFileInfo> {
    let (digest, sha256_stdout, sha256_stderr) = match adb.shell(["sha256sum", remote]).await {
        Ok(output) => (
            parse_sha256_output(&output.stdout).map(str::to_owned),
            output.stdout,
            output.stderr,
        ),
        Err(error) => {
            debug!(
                target: "android_driver_rs::driver",
                stage,
                remote,
                error = %error,
                "设备端不支持 sha256sum，降级使用文件大小校验"
            );
            (None, String::new(), String::new())
        }
    };
    let size = match adb.shell(["stat", "-c", "%s", remote]).await {
        Ok(output) => output
            .stdout
            .split_whitespace()
            .next()
            .and_then(|value| value.parse().ok()),
        Err(error) => {
            debug!(
                target: "android_driver_rs::driver",
                stage,
                remote,
                error = %error,
                "无法获取设备端 Agent 文件大小"
            );
            match adb.shell(["wc", "-c", remote]).await {
                Ok(output) => output
                    .stdout
                    .split_whitespace()
                    .next()
                    .and_then(|value| value.parse().ok()),
                Err(_) => None,
            }
        }
    };
    let exists =
        digest.is_some() || size.is_some() || adb.shell(["test", "-f", remote]).await.is_ok();
    debug!(
        target: "android_driver_rs::driver",
        stage,
        remote,
        expected_sha256 = agent::JAR_SHA256,
        actual_sha256 = digest.as_deref().unwrap_or("<无法解析>"),
        size = ?size,
        sha256_stdout = ?sha256_stdout,
        sha256_stderr = ?sha256_stderr,
        exists,
        "设备端 Agent 文件信息"
    );
    Ok(RemoteFileInfo {
        digest,
        size,
        exists,
    })
}

fn parse_sha256_output(output: &str) -> Option<&str> {
    output
        .split_whitespace()
        .find(|value| value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()))
}

fn remote_digest_matches(info: &RemoteFileInfo) -> bool {
    info.digest
        .as_deref()
        .is_some_and(|value| value.eq_ignore_ascii_case(agent::JAR_SHA256))
}

fn remote_file_matches(info: &RemoteFileInfo) -> bool {
    remote_digest_matches(info)
        || (info.exists && info.digest.is_none() && info.size == Some(agent::JAR_SIZE))
}

fn verify_remote_digest(info: &RemoteFileInfo, kind: &str) -> Result<()> {
    if remote_digest_matches(info) {
        return Ok(());
    }
    if remote_file_matches(info) {
        warn!(
            target: "android_driver_rs::driver",
            kind,
            expected_size = agent::JAR_SIZE,
            "设备端不支持 SHA-256，已降级为文件大小校验"
        );
        return Ok(());
    }
    let actual = info.digest.as_deref().unwrap_or("<无法解析>");
    warn!(
        target: "android_driver_rs::driver",
        kind,
        expected_sha256 = agent::JAR_SHA256,
        actual_sha256 = actual,
        size = ?info.size,
        "设备端 Agent JAR 校验失败"
    );
    Err(DriverError::AgentVerification(format!(
        "设备端{kind} u2.jar 校验失败（expected_sha256={}, actual={actual}, size={:?}, exists={}）",
        agent::JAR_SHA256,
        info.size,
        info.exists
    )))
}

pub(super) async fn establish_session(
    adb: &AdbRunner,
    config: &DriverConfig,
) -> Result<EstablishedSession> {
    info!(target: "android_driver_rs::driver", "建立 RPC 会话");
    let compatible_process = compatible_agent_process(adb, DEFAULT_AGENT_PORT).await;
    let borrowed_forward = ForwardGuard::new(adb, create_forward(adb, DEFAULT_AGENT_PORT).await?);
    let borrowed_port = borrowed_forward
        .forward
        .as_ref()
        .expect("forward guard 已持有资源")
        .local_port;
    if compatible_process && ping(borrowed_port, adb.agent_timeout()).await {
        let forward = borrowed_forward.into_inner();
        return Ok(EstablishedSession {
            rpc: RpcClient::new(forward.local_port, config.rpc_timeout, config.max_json_size),
            forward,
            owned_agent: None,
        });
    }
    borrowed_forward.cleanup().await?;
    resolve_uiautomation_conflict(adb, config.ui_automation_conflict_policy).await?;

    let mut startup_errors = Vec::new();
    for port in OWNED_AGENT_PORTS {
        if remote_port_in_use(adb, port).await {
            continue;
        }
        let owned_agent = match start_agent(adb, port).await {
            Ok(value) => OwnedAgentGuard::new(adb, value),
            Err((error, guard)) => {
                guard.cleanup().await;
                warn!(
                    target: "android_driver_rs::driver",
                    remote_port = port,
                    error = %error,
                    "Agent 启动尝试失败"
                );
                if is_uiautomation_conflict_message(&error.to_string()) {
                    return Err(error);
                }
                startup_errors.push(format!("tcp:{port}: {error}"));
                continue;
            }
        };
        let forward = match create_forward(adb, port).await {
            Ok(forward) => ForwardGuard::new(adb, forward),
            Err(error) => {
                return match owned_agent.cleanup().await {
                    Ok(()) => Err(error),
                    Err(cleanup_error) => Err(DriverError::AgentStartup(format!(
                        "创建 ADB forward 失败：{error}；随后停止 Agent 也失败：{cleanup_error}"
                    ))),
                };
            }
        };
        let local_port = forward
            .forward
            .as_ref()
            .expect("forward guard 已持有资源")
            .local_port;
        let deadline = Instant::now() + adb.agent_timeout();
        loop {
            if ping(local_port, Duration::from_millis(500)).await {
                let forward = forward.into_inner();
                return Ok(EstablishedSession {
                    rpc: RpcClient::new(
                        forward.local_port,
                        config.rpc_timeout,
                        config.max_json_size,
                    ),
                    forward,
                    owned_agent: Some(owned_agent.into_inner()),
                });
            }
            if Instant::now() >= deadline {
                break;
            }
            sleep(Duration::from_millis(200)).await;
        }
        let forward_error = forward.cleanup().await.err();
        let agent_error = owned_agent.cleanup().await.err();
        if forward_error.is_some() || agent_error.is_some() {
            let mut cleanup_errors = Vec::new();
            if let Some(error) = forward_error {
                cleanup_errors.push(format!("清理 ADB forward 失败：{error}"));
            }
            if let Some(error) = agent_error {
                cleanup_errors.push(format!("停止 Agent 失败：{error}"));
            }
            return Err(DriverError::AgentStartup(format!(
                "等待 Agent 就绪超时，且资源清理未完全成功：{}",
                cleanup_errors.join("；")
            )));
        }
    }
    let detail = if startup_errors.is_empty() {
        "候选端口均已被占用".to_owned()
    } else {
        startup_errors.join("；")
    };
    Err(DriverError::AgentStartup(format!(
        "9008 不可借用，且 19008..=19017 均无法启动 Agent：{detail}"
    )))
}

async fn start_agent(
    adb: &AdbRunner,
    port: u16,
) -> std::result::Result<OwnedAgent, (DriverError, StartingAgentGuard)> {
    debug!(target: "android_driver_rs::driver", remote_port = port, "启动自有 Agent");
    let existing_app_processes = app_process_pids(adb).await;
    let mut startup_guard = StartingAgentGuard::new(adb, port);
    let classpath = format!("CLASSPATH={REMOTE_JAR}");
    let mut host_process = adb
        .spawn_long_running([
            "shell".to_owned(),
            classpath,
            "app_process".to_owned(),
            "/".to_owned(),
            "com.wetest.uia2.Main".to_owned(),
            "-p".to_owned(),
            port.to_string(),
        ])
        .map_err(|error| (error, StartingAgentGuard::new(adb, port)))?;
    let capture = match AgentCapture::attach(&mut host_process) {
        Ok(capture) => capture,
        Err(error) => return Err((error, startup_guard)),
    };
    let deadline = Instant::now() + adb.agent_timeout();
    loop {
        if let Some(pid) = agent_pid(adb, port).await {
            startup_guard.disarm();
            return Ok(OwnedAgent {
                pid,
                port,
                host_process,
                capture,
            });
        }
        if remote_port_in_use(adb, port).await
            && let Some(pid) = new_app_process_pid(adb, &existing_app_processes).await
        {
            startup_guard.disarm();
            debug!(
                target: "android_driver_rs::driver",
                remote_port = port,
                pid,
                "通过监听端口和新增 app_process 识别 Android 6 Agent"
            );
            return Ok(OwnedAgent {
                pid,
                port,
                host_process,
                capture,
            });
        }
        let status = match host_process.try_wait() {
            Ok(status) => status,
            Err(source) => return Err((DriverError::AdbSpawn(source), startup_guard)),
        };
        if let Some(status) = status {
            let error = agent_startup_failure(
                port,
                format!("Agent 子进程在监听端口前退出（status={status:?}）"),
                capture,
            )
            .await;
            return Err((error, startup_guard));
        }
        if Instant::now() >= deadline {
            let _ = host_process.kill().await;
            let error = agent_startup_failure(port, "等待 Agent PID 超时", capture).await;
            return Err((error, startup_guard));
        }
        sleep(Duration::from_millis(100)).await;
    }
}

async fn agent_startup_failure(
    port: u16,
    reason: impl Into<String>,
    capture: AgentCapture,
) -> DriverError {
    let diagnostic = capture.finish().await;
    let diagnostic = truncate_agent_diagnostic(&diagnostic);
    let reason = reason.into();
    let message = if is_uiautomation_conflict_message(&diagnostic) {
        format!(
            "UiAutomationService already registered：设备上的 UiAutomation 被其他进程占用（port={port}）；{diagnostic}"
        )
    } else if diagnostic.is_empty() {
        format!("{reason}（port={port}；设备端没有返回启动输出）")
    } else {
        format!("{reason}（port={port}；设备端输出：{diagnostic}）")
    };
    DriverError::AgentStartup(message)
}

pub(super) struct AgentCapture {
    stdout: Arc<Mutex<Vec<u8>>>,
    stderr: Arc<Mutex<Vec<u8>>>,
    readers: Vec<JoinHandle<()>>,
}

impl AgentCapture {
    fn attach(child: &mut Child) -> Result<Self> {
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| DriverError::AgentStartup("无法捕获 Agent stdout".into()))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| DriverError::AgentStartup("无法捕获 Agent stderr".into()))?;
        let stdout_sink = Arc::new(Mutex::new(Vec::new()));
        let stderr_sink = Arc::new(Mutex::new(Vec::new()));
        let readers = vec![
            spawn_agent_reader(stdout, Arc::clone(&stdout_sink)),
            spawn_agent_reader(stderr, Arc::clone(&stderr_sink)),
        ];
        Ok(Self {
            stdout: stdout_sink,
            stderr: stderr_sink,
            readers,
        })
    }

    async fn finish(mut self) -> String {
        for reader in self.readers.drain(..) {
            let _ = reader.await;
        }
        let stdout = String::from_utf8_lossy(&self.stdout.lock().await).into_owned();
        let stderr = String::from_utf8_lossy(&self.stderr.lock().await).into_owned();
        match (stdout.trim(), stderr.trim()) {
            ("", "") => String::new(),
            (stdout, "") => format!("stdout: {stdout}"),
            ("", stderr) => format!("stderr: {stderr}"),
            (stdout, stderr) => format!("stdout: {stdout}; stderr: {stderr}"),
        }
    }

    fn abort(&mut self) {
        for reader in &self.readers {
            reader.abort();
        }
        self.readers.clear();
    }
}

fn spawn_agent_reader<R>(mut reader: R, sink: Arc<Mutex<Vec<u8>>>) -> JoinHandle<()>
where
    R: AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut buffer = [0u8; 1024];
        loop {
            match reader.read(&mut buffer).await {
                Ok(0) | Err(_) => break,
                Ok(size) => {
                    let mut output = sink.lock().await;
                    output.extend_from_slice(&buffer[..size]);
                    if output.len() > 8192 {
                        let excess = output.len() - 8192;
                        output.drain(..excess);
                    }
                }
            }
        }
    })
}

fn truncate_agent_diagnostic(value: &str) -> String {
    let value = value.trim();
    if value.is_empty() {
        return String::new();
    }
    const LIMIT: usize = 8192;
    if value.chars().count() <= LIMIT {
        value.to_owned()
    } else {
        format!(
            "{}...[truncated]",
            value.chars().take(LIMIT).collect::<String>()
        )
    }
}

fn is_uiautomation_conflict_message(value: &str) -> bool {
    let normalized = value.to_ascii_lowercase();
    normalized.contains("uiautomationservice")
        && (normalized.contains("already registered")
            || normalized.contains("already_registered")
            || normalized.contains("已被"))
}

async fn resolve_uiautomation_conflict(
    adb: &AdbRunner,
    policy: UiAutomationConflictPolicy,
) -> Result<()> {
    let pids = uiautomator_pids(adb).await;
    if pids.is_empty() {
        return Ok(());
    }
    let list = pids
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(", ");
    match policy {
        UiAutomationConflictPolicy::Fail => Err(DriverError::AgentStartup(format!(
            "设备上的 UiAutomation 已被外部 uiautomator 进程占用（PID: {list}）；\
             请停止该进程后重试，或显式启用 KillStaleProcesses 策略"
        ))),
        UiAutomationConflictPolicy::KillStaleProcesses => {
            for pid in &pids {
                let pid = pid.to_string();
                adb.shell(["kill", &pid]).await.map_err(|error| {
                    DriverError::AgentStartup(format!(
                        "无法清理 uiautomator 进程 PID {pid}：{error}"
                    ))
                })?;
            }
            let deadline = Instant::now() + Duration::from_secs(2);
            loop {
                if uiautomator_pids(adb).await.is_empty() {
                    return Ok(());
                }
                if Instant::now() >= deadline {
                    return Err(DriverError::AgentStartup(format!(
                        "已请求清理 uiautomator 进程，但 PID {list} 仍在运行"
                    )));
                }
                sleep(Duration::from_millis(100)).await;
            }
        }
    }
}

pub(super) async fn stop_owned_agent_with_retries(
    adb: &AdbRunner,
    agent: &mut OwnedAgent,
) -> Result<()> {
    let mut last_error = None;
    for attempt in 1..=RESOURCE_CLEANUP_ATTEMPTS {
        match stop_owned_agent(adb, agent).await {
            Ok(()) => return Ok(()),
            Err(error) => {
                warn!(
                    target: "android_driver_rs::driver",
                    attempt,
                    max_attempts = RESOURCE_CLEANUP_ATTEMPTS,
                    error = %error,
                    "停止自有 Agent 失败"
                );
                last_error = Some(error);
            }
        }
    }
    Err(last_error.expect("资源清理至少尝试一次"))
}

async fn stop_owned_agent(adb: &AdbRunner, agent: &mut OwnedAgent) -> Result<()> {
    debug!(target: "android_driver_rs::driver", remote_port = agent.port, "停止自有 Agent");
    let pid = agent.pid.to_string();
    let result = adb
        .shell(["kill", &pid])
        .await
        .or_else(|error| match error {
            DriverError::AdbCommand { .. } => Ok(crate::CommandOutput {
                stdout: String::new(),
                stderr: String::new(),
                status: 0,
            }),
            value => Err(value),
        })
        .map(|_| ());
    agent.capture.abort();
    let _ = agent.host_process.kill().await;
    result
}

async fn uiautomator_pids(adb: &AdbRunner) -> Vec<u32> {
    for output in [
        adb.shell(["ps", "-A", "-o", "PID,ARGS"]).await.ok(),
        adb.shell(["ps", "-A"]).await.ok(),
        adb.shell(["ps"]).await.ok(),
    ]
    .into_iter()
    .flatten()
    {
        let pids = output
            .stdout
            .lines()
            .filter_map(parse_uiautomator_pid_line)
            .collect::<Vec<_>>();
        if !pids.is_empty() {
            return pids;
        }
    }

    let proc_listing = r#"for path in /proc/[0-9]*/cmdline; do pid=${path#/proc/}; pid=${pid%/cmdline}; cmdline=$(tr '\0' ' ' < "$path" 2>/dev/null); case "$cmdline" in uiautomator|uiautomator\ *|*/uiautomator|*/uiautomator\ *) echo "$pid $cmdline";; esac; done"#;
    adb.shell(["sh", "-c", proc_listing])
        .await
        .map(|output| {
            output
                .stdout
                .lines()
                .filter_map(parse_uiautomator_pid_line)
                .collect()
        })
        .unwrap_or_default()
}

fn parse_uiautomator_pid_line(line: &str) -> Option<u32> {
    let fields = line.split_whitespace().collect::<Vec<_>>();
    let pid_index = fields
        .iter()
        .position(|field| field.parse::<u32>().is_ok())?;
    let command = &fields[pid_index + 1..];
    if command
        .iter()
        .any(|field| field.contains("com.wetest.uia2.Main"))
    {
        return None;
    }
    command
        .iter()
        .any(|field| {
            *field == "uiautomator"
                || field.ends_with("/uiautomator")
                || field.starts_with("uiautomator:")
        })
        .then(|| fields[pid_index].parse().ok())
        .flatten()
}

pub(super) async fn agent_pid(adb: &AdbRunner, port: u16) -> Option<u32> {
    let port = port.to_string();
    if let Ok(output) = adb.shell(["ps", "-A", "-o", "PID,ARGS"]).await
        && let Some(pid) = output
            .stdout
            .lines()
            .find_map(|line| parse_agent_pid_line(line, &port))
    {
        return Some(pid);
    }
    if let Ok(output) = adb.shell(["ps", "-A"]).await
        && let Some(pid) = output
            .stdout
            .lines()
            .find_map(|line| parse_agent_pid_line(line, &port))
    {
        return Some(pid);
    }
    if let Ok(output) = adb.shell(["ps"]).await
        && let Some(pid) = output
            .stdout
            .lines()
            .find_map(|line| parse_agent_pid_line(line, &port))
    {
        return Some(pid);
    }

    let proc_listing = r#"for path in /proc/[0-9]*/cmdline; do pid=${path#/proc/}; pid=${pid%/cmdline}; cmdline=$(tr '\0' ' ' < "$path" 2>/dev/null); case "$cmdline" in *com.wetest.uia2.Main*) echo "$pid $cmdline";; esac; done"#;
    adb.shell(["sh", "-c", proc_listing])
        .await
        .ok()?
        .stdout
        .lines()
        .find_map(|line| parse_agent_pid_line(line, &port))
}

async fn app_process_pids(adb: &AdbRunner) -> Vec<u32> {
    for output in [
        adb.shell(["ps", "-A", "-o", "PID,ARGS"]).await.ok(),
        adb.shell(["ps", "-A"]).await.ok(),
        adb.shell(["ps"]).await.ok(),
    ]
    .into_iter()
    .flatten()
    {
        let pids = output
            .stdout
            .lines()
            .filter_map(parse_app_process_pid_line)
            .collect::<Vec<_>>();
        if !pids.is_empty() {
            return pids;
        }
    }
    Vec::new()
}

async fn new_app_process_pid(adb: &AdbRunner, existing: &[u32]) -> Option<u32> {
    let mut candidates = app_process_pids(adb)
        .await
        .into_iter()
        .filter(|pid| !existing.contains(pid));
    let pid = candidates.next()?;
    candidates.next().is_none().then_some(pid)
}

fn parse_app_process_pid_line(line: &str) -> Option<u32> {
    let fields = line.split_whitespace().collect::<Vec<_>>();
    let pid_index = fields
        .iter()
        .position(|field| field.parse::<u32>().is_ok())?;
    fields[pid_index + 1..]
        .iter()
        .any(|field| {
            matches!(
                field.rsplit('/').next(),
                Some("app_process" | "app_process32" | "app_process64")
            )
        })
        .then(|| fields[pid_index].parse().ok())
        .flatten()
}

fn parse_agent_pid_line(line: &str, port: &str) -> Option<u32> {
    let fields = line.split_whitespace().collect::<Vec<_>>();
    let command_start = fields
        .iter()
        .position(|field| field.contains("com.wetest.uia2.Main"))?;
    let command = &fields[command_start..];
    let has_port = command
        .windows(2)
        .any(|pair| pair[0] == "-p" && pair[1] == port)
        || command.contains(&port);
    if !has_port {
        return None;
    }
    fields[..command_start]
        .iter()
        .find_map(|field| field.parse::<u32>().ok())
}

async fn remote_port_in_use(adb: &AdbRunner, port: u16) -> bool {
    adb.shell(["cat", "/proc/net/tcp", "/proc/net/tcp6"])
        .await
        .map(|value| {
            value
                .stdout
                .lines()
                .filter_map(|line| line.split_whitespace().nth(1))
                .filter_map(|address| address.rsplit_once(':').map(|(_, port)| port))
                .filter_map(|value| u16::from_str_radix(value, 16).ok())
                .any(|value| value == port)
        })
        .unwrap_or(false)
}

async fn compatible_agent_process(adb: &AdbRunner, port: u16) -> bool {
    let Some(pid) = agent_pid(adb, port).await else {
        return false;
    };
    let command = format!("tr '\\0' '\\n' </proc/{pid}/environ");
    match adb.shell(["sh", "-c", &command]).await {
        Ok(output) => output.stdout.lines().any(classpath_matches),
        Err(error) => {
            warn!(
                target: "android_driver_rs::driver",
                pid,
                port,
                error = %error,
                "无法读取 Agent 进程环境，按目标命令行尝试复用"
            );
            true
        }
    }
}

fn classpath_matches(value: &str) -> bool {
    value
        .strip_prefix("CLASSPATH=")
        .is_some_and(|classpath| classpath.split(':').any(|entry| entry == REMOTE_JAR))
}

async fn create_forward(adb: &AdbRunner, remote_port: u16) -> Result<OwnedForward> {
    trace!(target: "android_driver_rs::driver", remote_port, "创建端口转发");
    let remote = format!("tcp:{remote_port}");
    let output = adb
        .run_text(["forward", "tcp:0", &remote], adb.agent_timeout())
        .await?;
    let local_port = output
        .stdout
        .trim()
        .parse()
        .map_err(|_| DriverError::Forward("ADB 未返回动态本地端口".into()))?;
    Ok(OwnedForward {
        local_port,
        remote_port,
    })
}

pub(super) async fn remove_forward_with_retries(
    adb: &AdbRunner,
    forward: &OwnedForward,
) -> Result<()> {
    let mut last_error = None;
    for attempt in 1..=RESOURCE_CLEANUP_ATTEMPTS {
        match remove_forward(adb, forward).await {
            Ok(()) => return Ok(()),
            Err(error) => {
                warn!(
                    target: "android_driver_rs::driver",
                    attempt,
                    max_attempts = RESOURCE_CLEANUP_ATTEMPTS,
                    local_port = forward.local_port,
                    error = %error,
                    "移除 ADB forward 失败"
                );
                last_error = Some(error);
            }
        }
    }
    Err(last_error.expect("资源清理至少尝试一次"))
}

async fn remove_forward(adb: &AdbRunner, forward: &OwnedForward) -> Result<()> {
    trace!(target: "android_driver_rs::driver", local_port = forward.local_port, remote_port = forward.remote_port, "移除端口转发");
    let local = format!("tcp:{}", forward.local_port);
    adb.run_text(["forward", "--remove", &local], adb.agent_timeout())
        .await?;
    for _ in 0..3 {
        let output = adb
            .run_text(["forward", "--list"], adb.agent_timeout())
            .await?;
        if !output
            .stdout
            .lines()
            .any(|line| line.split_whitespace().any(|value| value == local))
        {
            return Ok(());
        }
        sleep(Duration::from_millis(100)).await;
    }
    Err(DriverError::Forward(format!(
        "tcp:{} -> tcp:{} 删除后仍存在",
        forward.local_port, forward.remote_port
    )))
}

pub(super) async fn cleanup_resources(adb: &AdbRunner, state: &mut SessionState) -> Result<()> {
    debug!(target: "android_driver_rs::driver", "清理资源");
    let mut errors = Vec::new();
    if let Some(agent) = state.owned_agent.as_mut() {
        match stop_owned_agent_with_retries(adb, agent).await {
            Ok(()) => state.owned_agent = None,
            Err(error) => errors.push(format!("停止 Agent 失败：{error}")),
        }
    }
    let mut index = 0;
    while index < state.forwards.len() {
        let forward = state.forwards[index].clone();
        match remove_forward_with_retries(adb, &forward).await {
            Ok(()) => {
                state.forwards.remove(index);
            }
            Err(error) => {
                errors.push(format!(
                    "清理 ADB forward tcp:{} 失败：{error}",
                    forward.local_port
                ));
                index += 1;
            }
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(DriverError::AgentStartup(format!(
            "Driver 资源清理未完全成功：{}",
            errors.join("；")
        )))
    }
}

pub(super) async fn restore_ime_locked(adb: &AdbRunner, state: &mut SessionState) -> Result<()> {
    if let Some(original) = state.active_ime.as_deref() {
        adb.shell(["ime", "set", original]).await?;
        state.active_ime = None;
    }
    Ok(())
}

#[cfg(feature = "input-method")]
pub(super) struct ImeGuard {
    pub(super) adb: AdbRunner,
    pub(super) original: Option<String>,
    pub(super) state: Arc<DriverInner>,
}

#[cfg(feature = "input-method")]
impl ImeGuard {
    pub(super) async fn restore(mut self) -> Result<()> {
        if let Some(original) = self.original.as_deref() {
            self.adb.shell(["ime", "set", original]).await?;
            self.state.state.lock().await.active_ime = None;
            self.original = None;
        }
        Ok(())
    }
}

#[cfg(feature = "input-method")]
impl Drop for ImeGuard {
    fn drop(&mut self) {
        let Some(original) = self.original.take() else {
            return;
        };
        let adb = self.adb.clone();
        let state = self.state.clone();
        spawn_cleanup(async move {
            let _ = adb.shell(["ime", "set", &original]).await;
            state.state.lock().await.active_ime = None;
        });
    }
}

pub(super) fn spawn_cleanup(future: impl Future<Output = ()> + Send + 'static) {
    debug!(target: "android_driver_rs::driver", "生成后台清理任务");
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        handle.spawn(future);
    } else {
        let _ = std::thread::Builder::new()
            .name("android-driver-cleanup".into())
            .spawn(move || {
                if let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    runtime.block_on(future);
                }
            });
    }
}

impl Drop for DriverInner {
    fn drop(&mut self) {
        let Ok(mut state) = self.state.try_lock() else {
            warn!(target: "android_driver_rs::driver", "Driver 释放时会话仍被占用，无法兜底清理");
            return;
        };
        if state.closed {
            return;
        }
        if let Some(rpc) = state.rpc.take() {
            rpc.invalidate();
        }
        let mut detached = SessionState {
            rpc: None,
            forwards: std::mem::take(&mut state.forwards),
            owned_agent: state.owned_agent.take(),
            generation: state.generation,
            closed: false,
            active_ime: state.active_ime.take(),
        };
        let adb = self.adb.clone();
        spawn_cleanup(async move {
            let _ = restore_ime_locked(&adb, &mut detached).await;
            let _ = cleanup_resources(&adb, &mut detached).await;
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefers_override_display_size() {
        assert_eq!(
            parse_display_size("Physical size: 1080x2400\nOverride size: 720x1600\n"),
            Some(DisplaySize {
                width: 720,
                height: 1600
            })
        );
    }

    #[test]
    fn validates_png_signature() {
        assert!(validate_image(b"\x89PNG\r\n\x1a\nrest".to_vec()).is_ok());
        assert!(validate_image(b"\xff\xd8\xffrest".to_vec()).is_ok());
        assert!(validate_image(b"bad".to_vec()).is_err());
    }

    #[test]
    fn parses_common_sha256_output_formats() {
        let gnu = format!("{}  /data/local/tmp/u2.jar\n", agent::JAR_SHA256);
        assert_eq!(parse_sha256_output(&gnu), Some(agent::JAR_SHA256));

        let uppercase = agent::JAR_SHA256.to_ascii_uppercase();
        let bsd = format!("SHA256 (/data/local/tmp/u2.jar) = {uppercase}\r\n");
        assert_eq!(parse_sha256_output(&bsd), Some(uppercase.as_str()));
        assert!(remote_digest_matches(&RemoteFileInfo {
            digest: Some(uppercase),
            size: Some(agent::JAR_SIZE),
            exists: true,
        }));
        assert!(remote_file_matches(&RemoteFileInfo {
            digest: None,
            size: Some(agent::JAR_SIZE),
            exists: true,
        }));
        assert!(!remote_file_matches(&RemoteFileInfo {
            digest: None,
            size: Some(agent::JAR_SIZE - 1),
            exists: true,
        }));
    }

    #[test]
    fn parses_legacy_ps_rows_and_proc_rows() {
        assert_eq!(
            parse_agent_pid_line(
                "u0_a123 4321 88 123456 45678 ffffffff 00000000 S com.wetest.uia2.Main -p 19008",
                "19008"
            ),
            Some(4321)
        );
        assert_eq!(
            parse_agent_pid_line("4321 com.wetest.uia2.Main -p 19008", "19008"),
            Some(4321)
        );
        assert_eq!(
            parse_agent_pid_line("4321 com.wetest.uia2.Main -p 9008", "19008"),
            None
        );
    }

    #[test]
    fn accepts_classpath_variants() {
        assert!(classpath_matches(&format!("CLASSPATH={REMOTE_JAR}")));
        assert!(classpath_matches(&format!(
            "CLASSPATH=/system/framework/foo.jar:{REMOTE_JAR}:"
        )));
        assert!(!classpath_matches("PATH=/system/bin"));
    }
}
