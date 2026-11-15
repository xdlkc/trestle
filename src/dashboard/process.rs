use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Map, Value, json};

use crate::core::errors::WbError;
use crate::core::io_utils::read_json_file;

use super::server::{cleanup_runtime_file, runtime_file_path, serve_foreground};

pub fn http_request_loopback(
    port: u16,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
) -> Result<(u16, String), WbError> {
    let addr: SocketAddr = ([127, 0, 0, 1], port).into();
    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_secs(2))
        .map_err(|e| WbError::dashboard(format!("连接本机端口 127.0.0.1:{port} 失败: {e}")))?;
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));

    let has_host = headers
        .iter()
        .any(|(k, _)| k.eq_ignore_ascii_case("host"));

    let mut req = format!("{method} {path} HTTP/1.1\r\n");
    if !has_host {
        req.push_str(&format!("Host: 127.0.0.1:{port}\r\n"));
    }
    for (k, v) in headers {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    req.push_str("Connection: close\r\n\r\n");

    stream
        .write_all(req.as_bytes())
        .map_err(|e| WbError::dashboard(format!("发送 HTTP 请求失败: {e}")))?;
    let _ = stream.flush();

    let mut raw = Vec::new();
    stream
        .read_to_end(&mut raw)
        .map_err(|e| WbError::dashboard(format!("读取 HTTP 响应失败: {e}")))?;

    let text = String::from_utf8_lossy(&raw).into_owned();
    let (head, body) = match text.find("\r\n\r\n") {
        Some(idx) => (&text[..idx], text[idx + 4..].to_string()),
        None => match text.find("\n\n") {
            Some(idx) => (&text[..idx], text[idx + 2..].to_string()),
            None => return Err(WbError::dashboard("HTTP 响应格式无效")),
        },
    };
    let status_line = head.lines().next().unwrap_or("");
    let status: u16 = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse::<u16>().ok())
        .ok_or_else(|| WbError::dashboard(format!("无法解析 HTTP 状态行: {status_line}")))?;

    Ok((status, body))
}

fn canonical_ws(ws_root: &Path) -> String {
    fs::canonicalize(ws_root)
        .unwrap_or_else(|_| ws_root.to_path_buf())
        .to_string_lossy()
        .into_owned()
}

pub fn get_dashboard_status(ws_root: &Path) -> Result<Value, WbError> {
    let rpath = runtime_file_path(ws_root);
    if !rpath.is_file() {
        return Ok(json!({
            "running": false,
            "verified": false,
            "reason": "未启动",
        }));
    }

    let info = read_json_file(&rpath)?;
    let port = info.get("port").and_then(|v| v.as_u64()).unwrap_or(0) as u16;
    let pid = info.get("pid").and_then(|v| v.as_i64()).unwrap_or(0);
    let expected_id = info
        .get("instance_id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let expected_ws = canonical_ws(ws_root);

    if port == 0 || expected_id.is_empty() {
        return Ok(json!({
            "running": false,
            "verified": false,
            "pid": pid,
            "port": port,
            "instance_id": expected_id,
            "reason": "dashboard.json 记录不完整",
        }));
    }

    match http_request_loopback(port, "GET", "/api/health", &[]) {
        Ok((200, body)) => {
            let health: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
            let actual_id = health
                .get("instance_id")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let actual_ws = health
                .get("workspace")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let id_match = actual_id == expected_id;
            let ws_match = actual_ws == expected_ws;
            let verified = health.get("ok").and_then(|v| v.as_bool()) == Some(true)
                && id_match
                && ws_match;

            let reason = if verified {
                Value::Null
            } else {
                Value::String(format!(
                    "instance_id 或 workspace 不匹配 (expected_id={expected_id}, actual_id={actual_id})"
                ))
            };
            let mut m = Map::new();
            m.insert("running".into(), Value::Bool(verified));
            m.insert("verified".into(), Value::Bool(verified));
            m.insert("pid".into(), Value::from(pid));
            m.insert("port".into(), Value::from(port as i64));
            m.insert("instance_id".into(), Value::String(expected_id));
            m.insert(
                "url".into(),
                Value::String(format!("http://127.0.0.1:{port}")),
            );
            m.insert("workspace".into(), Value::String(expected_ws));
            m.insert("reason".into(), reason);
            Ok(Value::Object(m))
        }
        Ok((code, _)) => Ok(json!({
            "running": false,
            "verified": false,
            "pid": pid,
            "port": port,
            "instance_id": expected_id,
            "reason": format!("/api/health 返回非 200 状态码: {code}"),
        })),
        Err(e) => Ok(json!({
            "running": false,
            "verified": false,
            "pid": pid,
            "port": port,
            "instance_id": expected_id,
            "reason": e.message,
        })),
    }
}

pub fn start_dashboard(ws_root: &Path, port: u16, foreground: bool) -> Result<Value, WbError> {
    let status = get_dashboard_status(ws_root)?;
    if status.get("running").and_then(|v| v.as_bool()) == Some(true) {
        let existing_port = status.get("port").and_then(|v| v.as_u64()).unwrap_or(0);
        return Err(WbError::dashboard(format!(
            "当前工作台看板已在运行中: http://127.0.0.1:{existing_port}"
        )));
    }

    if foreground {
        serve_foreground(ws_root, port)?;
        return Ok(json!({
            "stopped": true,
            "foreground": true,
        }));
    }

    let runtime_dir = ws_root.join(".wb-runtime");
    fs::create_dir_all(&runtime_dir)
        .map_err(|e| WbError::io(format!("创建 .wb-runtime 失败: {e}")))?;
    let log_path = runtime_dir.join("dashboard.log");
    let stdout_file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .map_err(|e| WbError::io(format!("打开 dashboard.log 失败: {e}")))?;
    let stderr_file = stdout_file
        .try_clone()
        .map_err(|e| WbError::io(format!("复制日志文件句柄失败: {e}")))?;

    let exe = std::env::current_exe()
        .map_err(|e| WbError::dashboard(format!("获取当前 wb 可执行文件路径失败: {e}")))?;

    let ws_canon = canonical_ws(ws_root);
    let mut child = Command::new(exe)
        .args([
            "--workspace",
            &ws_canon,
            "dashboard",
            "__daemon",
            "--port",
            &port.to_string(),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout_file))
        .stderr(Stdio::from(stderr_file))
        .spawn()
        .map_err(|e| WbError::dashboard(format!("后台启动看板进程失败: {e}")))?;

    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if let Ok(Some(exit_status)) = child.try_wait() {
            let log_tail = fs::read_to_string(&log_path).unwrap_or_default();
            let tail_msg = log_tail
                .lines()
                .rev()
                .find(|l| !l.trim().is_empty())
                .unwrap_or("看板子进程提前退出");
            return Err(WbError::dashboard(format!(
                "看板启动失败 (exit={}): {tail_msg}",
                exit_status.code().unwrap_or(1)
            )));
        }

        if let Ok(st) = get_dashboard_status(ws_root) {
            if st.get("running").and_then(|v| v.as_bool()) == Some(true)
                && st.get("verified").and_then(|v| v.as_bool()) == Some(true)
            {
                let mut obj = st.as_object().cloned().unwrap_or_default();
                obj.insert("started".into(), Value::Bool(true));
                return Ok(Value::Object(obj));
            }
        }
        thread::sleep(Duration::from_millis(25));
    }

    let _ = child.kill();
    Err(WbError::dashboard("等待看板进程就绪超时 (5s)"))
}

pub fn stop_dashboard(ws_root: &Path) -> Result<Value, WbError> {
    let rpath = runtime_file_path(ws_root);
    if !rpath.is_file() {
        return Err(WbError::dashboard(
            "当前工作台未记录运行中的看板实例 (.wb-runtime/dashboard.json 不存在)",
        ));
    }

    let info = read_json_file(&rpath)?;
    let port = info.get("port").and_then(|v| v.as_u64()).unwrap_or(0) as u16;
    let expected_id = info
        .get("instance_id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let csrf_token = info
        .get("csrf_token")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let expected_ws = canonical_ws(ws_root);

    if port == 0 || expected_id.is_empty() {
        return Err(WbError::dashboard("dashboard.json 缺少有效的端口或 instance_id"));
    }

    let (code, body) = http_request_loopback(port, "GET", "/api/health", &[]).map_err(|e| {
        WbError::dashboard(format!(
            "看板健康检查失败（未向未知进程发送停止指令）: {}",
            e.message
        ))
    })?;
    if code != 200 {
        return Err(WbError::dashboard(format!(
            "看板健康检查返回非 200 状态码 ({code})，拒绝停止"
        )));
    }

    let health: Value = serde_json::from_str(&body)
        .map_err(|_| WbError::dashboard("看板健康检查返回非 JSON 响应，拒绝停止"))?;
    let actual_id = health
        .get("instance_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let actual_ws = health
        .get("workspace")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    if actual_id != expected_id || actual_ws != expected_ws {
        return Err(WbError::dashboard(format!(
            "看板实例标识 (instance_id) 或工作台校验失败：期望 {expected_id}@{expected_ws}，实际 {actual_id}@{actual_ws}，拒绝停止非本实例进程"
        )));
    }

    let (stop_code, _) = http_request_loopback(
        port,
        "POST",
        "/api/shutdown",
        &[("X-WB-CSRF-Token", &csrf_token)],
    )?;
    if stop_code != 200 {
        return Err(WbError::dashboard(format!(
            "发送优雅停止请求失败 (HTTP {stop_code})"
        )));
    }

    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        if http_request_loopback(port, "GET", "/api/health", &[]).is_err() {
            break;
        }
        thread::sleep(Duration::from_millis(25));
    }

    cleanup_runtime_file(ws_root, &expected_id);

    Ok(json!({
        "stopped": true,
        "instance_id": expected_id,
        "port": port,
        "workspace": expected_ws,
    }))
}
