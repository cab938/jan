use hyper::body::Bytes;
use hyper::service::{make_service_fn, service_fn};
use hyper::{Body, Method, Request, Response, Server, StatusCode};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::convert::Infallible;
use std::net::{IpAddr, SocketAddr};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{broadcast, oneshot, Mutex};
use tokio::time::{timeout, Duration, Instant};
use uuid::Uuid;

use crate::core::state::ServerHandle;

#[derive(Clone, Default)]
pub struct CodexShimConfig {
    pub host: String,
    pub port: u16,
    pub tool_mode: String,
    pub model_id: Option<String>,
    pub codex_app_server_path: Option<String>,
}

#[derive(Default)]
pub struct CodexShimState {
    pub config: CodexShimConfig,
    pub port: Option<u16>,
    pub codex: Option<Arc<CodexProcess>>,
    pub thread_map: HashMap<String, String>,
}

struct CodexRpc {
    stdin: Mutex<tokio::process::ChildStdin>,
    pending: Mutex<HashMap<u64, oneshot::Sender<Result<Value, String>>>>,
    notifications: broadcast::Sender<Value>,
    next_id: AtomicU64,
}

impl CodexRpc {
    fn new(stdin: tokio::process::ChildStdin) -> Arc<Self> {
        let (tx, _rx) = broadcast::channel(128);
        Arc::new(Self {
            stdin: Mutex::new(stdin),
            pending: Mutex::new(HashMap::new()),
            notifications: tx,
            next_id: AtomicU64::new(1),
        })
    }

    fn subscribe(&self) -> broadcast::Receiver<Value> {
        self.notifications.subscribe()
    }

    async fn send_message(&self, payload: &Value) -> Result<(), String> {
        let serialized = serde_json::to_string(payload).map_err(|e| e.to_string())?;
        let mut stdin = self.stdin.lock().await;
        stdin
            .write_all(serialized.as_bytes())
            .await
            .map_err(|e| e.to_string())?;
        stdin.write_all(b"\n").await.map_err(|e| e.to_string())?;
        stdin.flush().await.map_err(|e| e.to_string())?;
        Ok(())
    }

    async fn send_notification(&self, method: &str, params: Option<Value>) -> Result<(), String> {
        let mut payload = json!({ "method": method });
        if let Some(params) = params {
            if let Some(obj) = payload.as_object_mut() {
                obj.insert("params".to_string(), params);
            }
        }
        self.send_message(&payload).await
    }

    async fn send_response(&self, id: u64, result: Value) -> Result<(), String> {
        let payload = json!({
            "id": id,
            "result": result
        });
        self.send_message(&payload).await
    }

    async fn send_error(&self, id: u64, code: i64, message: &str) -> Result<(), String> {
        let payload = json!({
            "id": id,
            "error": {
                "code": code,
                "message": message
            }
        });
        self.send_message(&payload).await
    }

    async fn request(&self, method: &str, params: Value) -> Result<Value, String> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);

        let payload = json!({
            "id": id,
            "method": method,
            "params": params
        });

        if let Err(err) = self.send_message(&payload).await {
            self.pending.lock().await.remove(&id);
            return Err(err);
        }

        match rx.await {
            Ok(Ok(result)) => Ok(result),
            Ok(Err(err)) => Err(err),
            Err(_) => Err("Codex app server request canceled".to_string()),
        }
    }
}

struct CodexProcess {
    child: Mutex<Child>,
    rpc: Arc<CodexRpc>,
    reader_handle: Mutex<Option<tauri::async_runtime::JoinHandle<()>>>,
    stderr_handle: Mutex<Option<tauri::async_runtime::JoinHandle<()>>>,
}

impl CodexProcess {
    async fn start(
        config: &CodexShimConfig,
        state: Arc<Mutex<CodexShimState>>,
    ) -> Result<Arc<Self>, String> {
        let binary = config
            .codex_app_server_path
            .clone()
            .unwrap_or_else(|| "codex".to_string());

        let mut command = Command::new(binary);
        command
            .arg("app-server")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = command.spawn().map_err(|e| e.to_string())?;
        let stdin = child.stdin.take().ok_or("Failed to open codex stdin")?;
        let stdout = child.stdout.take().ok_or("Failed to open codex stdout")?;
        let stderr = child.stderr.take().ok_or("Failed to open codex stderr")?;

        let rpc = CodexRpc::new(stdin);
        let rpc_clone = rpc.clone();
        let reader_state = state.clone();

        let reader_handle = tauri::async_runtime::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if line.trim().is_empty() {
                    continue;
                }

                let message: Value = match serde_json::from_str(&line) {
                    Ok(value) => value,
                    Err(error) => {
                        log::warn!("Codex app server JSON parse error: {error}");
                        continue;
                    }
                };

                let method = message.get("method").and_then(|v| v.as_str());
                let id = parse_id(message.get("id"));

                if let Some(method) = method {
                    if let Some(id) = id {
                        handle_server_request(id, method, &message, rpc_clone.clone(), reader_state.clone()).await;
                    } else {
                        let _ = rpc_clone.notifications.send(message);
                    }
                    continue;
                }

                if let Some(id) = id {
                    let response = if let Some(error) = message.get("error") {
                        Err(error.to_string())
                    } else {
                        Ok(message.get("result").cloned().unwrap_or(Value::Null))
                    };

                    if let Some(sender) = rpc_clone.pending.lock().await.remove(&id) {
                        let _ = sender.send(response);
                    }
                }
            }
        });

        let stderr_handle = tauri::async_runtime::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if !line.trim().is_empty() {
                    log::info!("codex app-server: {line}");
                }
            }
        });

        let process = Arc::new(Self {
            child: Mutex::new(child),
            rpc,
            reader_handle: Mutex::new(Some(reader_handle)),
            stderr_handle: Mutex::new(Some(stderr_handle)),
        });

        process.initialize().await?;
        Ok(process)
    }

    fn subscribe(&self) -> broadcast::Receiver<Value> {
        self.rpc.subscribe()
    }

    async fn request(&self, method: &str, params: Value) -> Result<Value, String> {
        self.rpc.request(method, params).await
    }

    async fn initialize(&self) -> Result<(), String> {
        let client_info = json!({
            "name": "jan",
            "title": "Jan Desktop",
            "version": env!("CARGO_PKG_VERSION")
        });

        self.rpc
            .request("initialize", json!({ "clientInfo": client_info }))
            .await?;
        self.rpc.send_notification("initialized", None).await?;
        Ok(())
    }

    async fn is_running(&self) -> bool {
        let mut child = self.child.lock().await;
        match child.try_wait() {
            Ok(Some(_)) => false,
            Ok(None) => true,
            Err(_) => false,
        }
    }

    async fn shutdown(&self) {
        if let Some(handle) = self.reader_handle.lock().await.take() {
            handle.abort();
        }
        if let Some(handle) = self.stderr_handle.lock().await.take() {
            handle.abort();
        }
        let mut child = self.child.lock().await;
        let _ = child.kill().await;
    }
}

fn parse_id(value: Option<&Value>) -> Option<u64> {
    match value {
        Some(Value::Number(number)) => number.as_u64(),
        Some(Value::String(text)) => text.parse().ok(),
        _ => None,
    }
}

async fn handle_server_request(
    id: u64,
    method: &str,
    message: &Value,
    rpc: Arc<CodexRpc>,
    state: Arc<Mutex<CodexShimState>>,
) {
    if method.ends_with("/requestApproval") {
        let tool_mode = { state.lock().await.config.tool_mode.clone() };
        let decision = if tool_mode == "codex" { "accept" } else { "decline" };
        let result = json!({ "decision": decision });
        let _ = rpc.send_response(id, result).await;
        return;
    }

    if method == "tool/requestUserInput" {
        let tool_mode = { state.lock().await.config.tool_mode.clone() };
        let params = message.get("params");
        let choice = choose_user_input_option(params, tool_mode.as_str());
        let result = json!({ "choice": choice });
        let _ = rpc.send_response(id, result).await;
        return;
    }

    let _ = rpc
        .send_error(id, -32601, "Codex request not supported by Jan")
        .await;
}

fn choose_user_input_option(params: Option<&Value>, tool_mode: &str) -> String {
    let target = if tool_mode == "codex" { "accept" } else { "decline" };
    if let Some(params) = params {
        if let Some(options) = params.get("options").and_then(|v| v.as_array()) {
            for option in options {
                if let Some(label) = option.get("label").and_then(|v| v.as_str()) {
                    if label.eq_ignore_ascii_case(target) {
                        if let Some(value) = option.get("value").and_then(|v| v.as_str()) {
                            return value.to_string();
                        }
                        return label.to_string();
                    }
                }
            }
            if let Some(first) = options.first() {
                if let Some(value) = first.get("value").and_then(|v| v.as_str()) {
                    return value.to_string();
                }
                if let Some(label) = first.get("label").and_then(|v| v.as_str()) {
                    return label.to_string();
                }
            }
        }
    }
    target.to_string()
}

pub async fn start_server(
    handle: Arc<Mutex<Option<ServerHandle>>>,
    state: Arc<Mutex<CodexShimState>>,
    config: CodexShimConfig,
) -> Result<u16, String> {
    let is_running = { handle.lock().await.is_some() };
    if is_running {
        let mut guard = state.lock().await;
        let same_bind = guard.config.host == config.host && guard.config.port == config.port;
        guard.config = config.clone();
        if same_bind {
            if let Some(port) = guard.port {
                return Ok(port);
            }
            return Ok(config.port);
        }
    }

    stop_http_server(handle.clone()).await?;

    {
        let mut guard = state.lock().await;
        guard.config = config.clone();
        guard.port = None;
    }

    let host = config.host.parse::<IpAddr>().map_err(|e| e.to_string())?;
    let addr = SocketAddr::new(host, config.port);
    let state_clone = state.clone();

    let builder = Server::try_bind(&addr).map_err(|e| e.to_string())?;
    log::info!("Codex shim server starting on http://{addr}");
    let actual_port = builder.local_addr().port();

    let server = builder.serve(make_service_fn(move |_| {
        let state = state_clone.clone();
        async move {
            Ok::<_, Infallible>(service_fn(move |req| {
                let state = state.clone();
                async move { handle_request(req, state).await }
            }))
        }
    }));

    let join_handle = tauri::async_runtime::spawn(async move {
        if let Err(err) = server.await {
            log::error!("Codex shim server error: {err}");
        }
        Ok(())
    });

    {
        let mut guard = state.lock().await;
        guard.port = Some(actual_port);
    }

    log::info!("Codex shim server started on port {actual_port}");
    let mut guard = handle.lock().await;
    *guard = Some(join_handle);

    Ok(actual_port)
}

async fn stop_http_server(handle: Arc<Mutex<Option<ServerHandle>>>) -> Result<(), String> {
    let mut guard = handle.lock().await;
    if let Some(join_handle) = guard.take() {
        join_handle.abort();
        log::info!("Codex shim server stopped");
    }
    Ok(())
}

pub async fn stop_server(
    handle: Arc<Mutex<Option<ServerHandle>>>,
    state: Arc<Mutex<CodexShimState>>,
) -> Result<(), String> {
    stop_http_server(handle).await?;
    let codex = { state.lock().await.codex.take() };
    if let Some(process) = codex {
        process.shutdown().await;
    }
    Ok(())
}

pub async fn is_running(handle: Arc<Mutex<Option<ServerHandle>>>) -> bool {
    handle.lock().await.is_some()
}

async fn ensure_codex_process(
    state: Arc<Mutex<CodexShimState>>,
) -> Result<Arc<CodexProcess>, String> {
    let existing = { state.lock().await.codex.clone() };
    if let Some(process) = existing {
        if process.is_running().await {
            return Ok(process);
        }
    }

    let config = { state.lock().await.config.clone() };
    let process = CodexProcess::start(&config, state.clone()).await?;

    let mut guard = state.lock().await;
    guard.codex = Some(process.clone());
    Ok(process)
}

fn pick_model_id(config: &CodexShimConfig, requested: Option<String>) -> Option<String> {
    if let Some(requested) = requested {
        if !requested.trim().is_empty() && requested != "codex-app-server" {
            return Some(requested);
        }
    }
    config.model_id.clone().filter(|value| !value.trim().is_empty())
}

async fn start_or_resume_thread(
    codex: Arc<CodexProcess>,
    state: Arc<Mutex<CodexShimState>>,
    jan_thread_id: Option<String>,
    model_id: Option<String>,
    approval_policy: &str,
) -> Result<String, String> {
    let mut events = codex.subscribe();
    if let Some(jan_id) = jan_thread_id.clone() {
        if let Some(existing) = state.lock().await.thread_map.get(&jan_id).cloned() {
            let _ = codex
                .request("thread/resume", json!({ "threadId": existing }))
                .await;
            return Ok(existing);
        }
    }

    let mut params = serde_json::Map::new();
    if let Some(model_id) = model_id.clone() {
        params.insert("model".to_string(), Value::String(model_id));
    }
    params.insert(
        "approvalPolicy".to_string(),
        Value::String(approval_policy.to_string()),
    );
    let result = codex.request("thread/start", Value::Object(params)).await?;
    let thread_id = if let Some(thread_id) = extract_thread_id(&result) {
        thread_id
    } else if let Some(thread_id) = await_thread_started(&mut events, Duration::from_secs(2)).await
    {
        thread_id
    } else {
        log::warn!("Codex app server thread/start result missing thread id: {result}");
        return Err("Codex app server did not return threadId".to_string());
    };

    if let Some(jan_id) = jan_thread_id {
        state.lock().await.thread_map.insert(jan_id, thread_id.clone());
    }

    Ok(thread_id)
}

fn extract_user_message(request_json: &Value) -> String {
    let messages = request_json.get("messages").and_then(|value| value.as_array());
    if let Some(messages) = messages {
        for message in messages.iter().rev() {
            if message.get("role").and_then(|v| v.as_str()) != Some("user") {
                continue;
            }
            if let Some(content) = message.get("content") {
                if let Some(text) = content.as_str() {
                    return text.to_string();
                }
                if let Some(parts) = content.as_array() {
                    let mut combined = String::new();
                    for part in parts {
                        if part.get("type").and_then(|v| v.as_str()) == Some("text") {
                            if let Some(text) = part.get("text").and_then(|v| v.as_str()) {
                                combined.push_str(text);
                            }
                        }
                    }
                    return combined;
                }
            }
        }
    }
    String::new()
}

fn extract_agent_delta(event: &Value) -> Option<String> {
    let method = event.get("method").and_then(|v| v.as_str())?;
    let params = event.get("params");

    if method == "item/agentMessage/delta" || method == "item/assistantMessage/delta" {
        if let Some(delta) = params.and_then(|p| p.get("delta")).and_then(|v| v.as_str()) {
            log::debug!("Codex delta ({method}): {} chars", delta.len());
            return Some(delta.to_string());
        }
        if let Some(item_delta) = params
            .and_then(|p| p.get("item"))
            .and_then(|item| item.get("delta"))
            .and_then(|v| v.as_str())
        {
            log::debug!("Codex delta ({method}): {} chars", item_delta.len());
            return Some(item_delta.to_string());
        }
    }

    if method == "item/agentMessage/added" || method == "item/assistantMessage/added" {
        if let Some(item) = params
            .and_then(|p| p.get("item"))
            .or_else(|| params.and_then(|p| p.get("message")))
        {
            if let Some(text) = extract_item_text(item) {
                log::debug!("Codex item added ({method}): {} chars", text.len());
                return Some(text);
            }
        }
    }

    if method == "item/added" || method == "item/completed" {
        if let Some(item) = params
            .and_then(|p| p.get("item"))
            .or_else(|| params.and_then(|p| p.get("message")))
        {
            if !is_assistant_item(item) {
                let role = extract_item_role(item);
                log::debug!(
                    "Ignoring non-assistant item for {method}: role={role:?}"
                );
                return None;
            }

            if let Some(text) = extract_item_text(item) {
                log::debug!("Codex item completed ({method}): {} chars", text.len());
                return Some(text);
            }
        }
    }

    None
}

fn extract_item_role(item: &Value) -> Option<&str> {
    if let Some(role) = item.get("role").and_then(|v| v.as_str()) {
        return Some(role);
    }
    if let Some(kind) = item.get("kind").and_then(|v| v.as_str()) {
        return Some(kind);
    }
    if let Some(item_type) = item.get("type").and_then(|v| v.as_str()) {
        return Some(item_type);
    }
    if let Some(author) = item.get("author") {
        if let Some(role) = author.get("role").and_then(|v| v.as_str()) {
            return Some(role);
        }
        if let Some(kind) = author.get("kind").and_then(|v| v.as_str()) {
            return Some(kind);
        }
        if let Some(author_type) = author.get("type").and_then(|v| v.as_str()) {
            return Some(author_type);
        }
    }
    if let Some(sender) = item.get("sender").and_then(|v| v.as_str()) {
        return Some(sender);
    }
    None
}

fn is_assistant_role(role: &str) -> bool {
    role.eq_ignore_ascii_case("assistant")
        || role.eq_ignore_ascii_case("assistant_message")
        || role.eq_ignore_ascii_case("assistantmessage")
        || role.eq_ignore_ascii_case("agent")
        || role.eq_ignore_ascii_case("agent_message")
        || role.eq_ignore_ascii_case("agentmessage")
        || role.eq_ignore_ascii_case("model")
}

fn is_assistant_item(item: &Value) -> bool {
    extract_item_role(item)
        .map(is_assistant_role)
        .unwrap_or(false)
}

fn extract_item_text(item: &Value) -> Option<String> {
    if let Some(text) = item.get("text").and_then(|v| v.as_str()) {
        return Some(text.to_string());
    }
    if let Some(content) = item.get("content") {
        if let Some(text) = content.as_str() {
            return Some(text.to_string());
        }
        if let Some(parts) = content.as_array() {
            let mut combined = String::new();
            for part in parts {
                if part.get("type").and_then(|v| v.as_str()) == Some("text") {
                    if let Some(text) = part.get("text").and_then(|v| v.as_str()) {
                        combined.push_str(text);
                    }
                }
            }
            if !combined.is_empty() {
                return Some(combined);
            }
        }
    }
    None
}

fn event_matches_turn(event: &Value, thread_id: &str, turn_id: Option<&str>) -> bool {
    let params = match event.get("params") {
        Some(params) => params,
        None => return true,
    };

    let event_thread_id = params
        .get("threadId")
        .and_then(|v| v.as_str())
        .or_else(|| params.get("thread_id").and_then(|v| v.as_str()))
        .or_else(|| {
            params
                .get("thread")
                .and_then(|v| v.get("id"))
                .and_then(|v| v.as_str())
        });

    if let Some(event_thread_id) = event_thread_id {
        if event_thread_id != thread_id {
            return false;
        }
    }

    if let Some(turn_id) = turn_id {
        let event_turn_id = params
            .get("turnId")
            .and_then(|v| v.as_str())
            .or_else(|| params.get("turn_id").and_then(|v| v.as_str()))
            .or_else(|| {
                params
                    .get("turn")
                    .and_then(|v| v.get("id"))
                    .and_then(|v| v.as_str())
            });
        if let Some(event_turn_id) = event_turn_id {
            return event_turn_id == turn_id;
        }
    }

    true
}

fn normalize_models(result: &Value) -> Vec<String> {
    let candidate = result
        .get("models")
        .or_else(|| result.get("data"))
        .unwrap_or(result);

    let Some(list) = candidate.as_array() else {
        return Vec::new();
    };

    let mut models = Vec::new();
    for entry in list {
        if let Some(id) = entry.as_str() {
            models.push(id.to_string());
            continue;
        }
        if let Some(id) = entry.get("id").and_then(|v| v.as_str()) {
            models.push(id.to_string());
            continue;
        }
        if let Some(id) = entry.get("model").and_then(|v| v.as_str()) {
            models.push(id.to_string());
            continue;
        }
        if let Some(id) = entry.get("name").and_then(|v| v.as_str()) {
            models.push(id.to_string());
        }
    }
    models
}

fn extract_turn_id(result: &Value) -> Option<String> {
    result
        .get("turnId")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .or_else(|| {
            result
                .get("turn_id")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
        })
        .or_else(|| {
            result
                .get("turn")
                .and_then(|v| v.get("id"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
        })
}

fn extract_thread_id(result: &Value) -> Option<String> {
    result
        .get("threadId")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .or_else(|| {
            result
                .get("thread_id")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
        })
        .or_else(|| {
            result
                .get("thread")
                .and_then(|v| v.get("id"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
        })
}

async fn await_thread_started(
    events: &mut broadcast::Receiver<Value>,
    timeout_duration: Duration,
) -> Option<String> {
    let deadline = Instant::now() + timeout_duration;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return None;
        }

        match timeout(remaining, events.recv()).await {
            Ok(Ok(event)) => {
                if let Some(method) = event.get("method").and_then(|v| v.as_str()) {
                    if method == "thread/started" {
                        if let Some(thread) = event.get("params").and_then(|p| p.get("thread")) {
                            if let Some(id) = thread.get("id").and_then(|v| v.as_str()) {
                                return Some(id.to_string());
                            }
                        }
                    }
                }
            }
            Ok(Err(broadcast::error::RecvError::Lagged(_))) => continue,
            Ok(Err(_)) => return None,
            Err(_) => return None,
        }
    }
}

fn is_turn_completed(event: &Value) -> bool {
    matches!(
        event.get("method").and_then(|v| v.as_str()),
        Some("turn/completed") | Some("turn/complete") | Some("turn/ended") | Some("turn/finished")
    )
}

async fn collect_turn_output(
    events: &mut broadcast::Receiver<Value>,
    thread_id: &str,
    turn_id: Option<&str>,
) -> String {
    let mut output = String::new();
    loop {
        match events.recv().await {
            Ok(event) => {
                if !event_matches_turn(&event, thread_id, turn_id) {
                    continue;
                }
                if is_turn_completed(&event) {
                    break;
                }
                if let Some(delta) = extract_agent_delta(&event) {
                    output.push_str(&delta);
                }
            }
            Err(broadcast::error::RecvError::Lagged(_)) => continue,
            Err(_) => break,
        }
    }
    output
}

async fn handle_request(
    req: Request<Body>,
    state: Arc<Mutex<CodexShimState>>,
) -> Result<Response<Body>, hyper::Error> {
    if req.method() == Method::OPTIONS {
        return Ok(cors_response(StatusCode::OK, Body::empty()));
    }

    let path = req.uri().path().to_string();
    match (req.method(), path.as_str()) {
        (&Method::GET, "/v1/models") | (&Method::GET, "/models") => {
            let codex = match ensure_codex_process(state.clone()).await {
                Ok(codex) => codex,
                Err(error) => {
                    let body = json!({
                        "error": {
                            "message": error
                        }
                    });
                    return Ok(cors_response(StatusCode::BAD_GATEWAY, Body::from(body.to_string())));
                }
            };

            let result = codex.request("model/list", json!({})).await;
            let models = match result {
                Ok(result) => normalize_models(&result),
                Err(error) => {
                    let body = json!({
                        "error": {
                            "message": error
                        }
                    });
                    return Ok(cors_response(StatusCode::BAD_GATEWAY, Body::from(body.to_string())));
                }
            };

            let created = chrono::Utc::now().timestamp();
            let data = if models.is_empty() {
                vec![json!({
                    "id": "codex-app-server",
                    "object": "model",
                    "created": created,
                    "owned_by": "codex-app-server"
                })]
            } else {
                models
                    .into_iter()
                    .map(|model| {
                        json!({
                            "id": model,
                            "object": "model",
                            "created": created,
                            "owned_by": "codex-app-server"
                        })
                    })
                    .collect()
            };

            let body = json!({
                "object": "list",
                "data": data
            });
            Ok(cors_response(StatusCode::OK, Body::from(body.to_string())))
        }
        (&Method::POST, "/v1/chat/completions")
        | (&Method::POST, "/chat/completions") => {
            handle_chat(req, state).await
        }
        _ => Ok(cors_response(
            StatusCode::NOT_FOUND,
            Body::from("Not found"),
        )),
    }
}

async fn handle_chat(
    req: Request<Body>,
    state: Arc<Mutex<CodexShimState>>,
) -> Result<Response<Body>, hyper::Error> {
    let jan_thread_id = req
        .headers()
        .get("x-jan-thread-id")
        .and_then(|value| value.to_str().ok())
        .map(|value| value.to_string());

    let body_bytes = hyper::body::to_bytes(req.into_body()).await?;
    let request_json: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap_or_default();

    let streaming = request_json
        .get("stream")
        .and_then(|value| value.as_bool())
        .unwrap_or(true);
    let requested_model = request_json
        .get("model")
        .and_then(|value| value.as_str())
        .map(|value| value.to_string());

    let user_message = extract_user_message(&request_json);
    if user_message.trim().is_empty() {
        let body = json!({
            "error": {
                "message": "No user message provided."
            }
        });
        return Ok(cors_response(StatusCode::BAD_REQUEST, Body::from(body.to_string())));
    }

    let codex = match ensure_codex_process(state.clone()).await {
        Ok(codex) => codex,
        Err(error) => {
            let body = json!({
                "error": {
                    "message": error
                }
            });
            return Ok(cors_response(StatusCode::BAD_GATEWAY, Body::from(body.to_string())));
        }
    };

    let config = { state.lock().await.config.clone() };
    let model_id = pick_model_id(&config, requested_model);
    let approval_policy = if config.tool_mode == "codex" {
        "never"
    } else {
        "on-request"
    };

    let thread_id = match start_or_resume_thread(
        codex.clone(),
        state.clone(),
        jan_thread_id,
        model_id.clone(),
        approval_policy,
    )
    .await
    {
        Ok(thread_id) => thread_id,
        Err(error) => {
            let body = json!({
                "error": {
                    "message": error
                }
            });
            return Ok(cors_response(StatusCode::BAD_GATEWAY, Body::from(body.to_string())));
        }
    };

    let mut turn_params = serde_json::Map::new();
    turn_params.insert("threadId".to_string(), Value::String(thread_id.clone()));
    turn_params.insert(
        "input".to_string(),
        json!([{ "type": "text", "text": user_message }]),
    );
    turn_params.insert(
        "approvalPolicy".to_string(),
        Value::String(approval_policy.to_string()),
    );
    if let Some(model_id) = model_id.clone() {
        turn_params.insert("model".to_string(), Value::String(model_id));
    }

    let mut events = codex.subscribe();
    let turn_result = codex.request("turn/start", Value::Object(turn_params)).await;
    let turn_id = match turn_result {
        Ok(result) => extract_turn_id(&result),
        Err(error) => {
            let body = json!({
                "error": {
                    "message": error
                }
            });
            return Ok(cors_response(StatusCode::BAD_GATEWAY, Body::from(body.to_string())));
        }
    };

    let created = chrono::Utc::now().timestamp() as u64;
    let model_name = model_id.unwrap_or_else(|| "codex-app-server".to_string());

    if !streaming {
        let response_text = collect_turn_output(&mut events, &thread_id, turn_id.as_deref()).await;
        let body = json!({
            "id": format!("chatcmpl-{}", created),
            "object": "chat.completion",
            "created": created,
            "model": model_name,
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": response_text
                },
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": 0,
                "completion_tokens": 0,
                "total_tokens": 0
            }
        });
        return Ok(cors_response(StatusCode::OK, Body::from(body.to_string())));
    }

    let (mut sender, body) = Body::channel();
    let response_id = format!("chatcmpl-{}", Uuid::new_v4());
    let thread_id_clone = thread_id.clone();
    let turn_id_clone = turn_id.clone();
    let model_name_clone = model_name.clone();

    tauri::async_runtime::spawn(async move {
        let mut sent_role = false;
        let mut sent_any_delta = false;
        loop {
            match events.recv().await {
                Ok(event) => {
                    if !event_matches_turn(&event, &thread_id_clone, turn_id_clone.as_deref()) {
                        continue;
                    }
                    if is_turn_completed(&event) {
                        break;
                    }
                    if let Some(delta) = extract_agent_delta(&event) {
                        if !sent_role {
                            sent_role = true;
                            let role_chunk = sse_data(json!({
                                "id": response_id,
                                "object": "chat.completion.chunk",
                                "created": created,
                                "model": model_name_clone,
                                "choices": [{
                                    "index": 0,
                                    "delta": { "role": "assistant" },
                                    "finish_reason": null
                                }]
                            }));
                            let _ = sender.send_data(Bytes::from(role_chunk)).await;
                        }

                        if !delta.is_empty() {
                            sent_any_delta = true;
                            let chunk = sse_data(json!({
                                "id": response_id,
                                "object": "chat.completion.chunk",
                                "created": created,
                                "model": model_name_clone,
                                "choices": [{
                                    "index": 0,
                                    "delta": { "content": delta },
                                    "finish_reason": null
                                }]
                            }));
                            let _ = sender.send_data(Bytes::from(chunk)).await;
                        }
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(_) => break,
            }
        }

        if !sent_role {
            let role_chunk = sse_data(json!({
                "id": response_id,
                "object": "chat.completion.chunk",
                "created": created,
                "model": model_name_clone,
                "choices": [{
                    "index": 0,
                    "delta": { "role": "assistant" },
                    "finish_reason": null
                }]
            }));
            let _ = sender.send_data(Bytes::from(role_chunk)).await;
        }

        if !sent_any_delta {
            log::warn!(
                "Codex stream finished without assistant deltas (thread_id={thread_id_clone}, turn_id={turn_id_clone:?})"
            );
        }

        let finish = sse_data(json!({
            "id": response_id,
            "object": "chat.completion.chunk",
            "created": created,
            "model": model_name_clone,
            "choices": [{
                "index": 0,
                "delta": {},
                "finish_reason": "stop"
            }]
        }));
        let _ = sender.send_data(Bytes::from(finish)).await;
        let _ = sender.send_data(Bytes::from("data: [DONE]\n\n")).await;
    });

    let response = Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "text/event-stream")
        .header("Cache-Control", "no-cache")
        .header("Connection", "keep-alive")
        .header("Access-Control-Allow-Origin", "*")
        .body(body)
        .unwrap();

    Ok(response)
}

fn sse_data(value: serde_json::Value) -> String {
    format!("data: {}\n\n", value.to_string())
}

fn cors_response(status: StatusCode, body: Body) -> Response<Body> {
    Response::builder()
        .status(status)
        .header("Access-Control-Allow-Origin", "*")
        .header(
            "Access-Control-Allow-Headers",
            "content-type, authorization, x-jan-thread-id",
        )
        .header("Access-Control-Allow-Methods", "GET, POST, OPTIONS")
        .body(body)
        .unwrap()
}
