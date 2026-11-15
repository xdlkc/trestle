use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use serde_json::{Map, Value, json};
use url::Url;

use crate::core::errors::WbError;
use crate::core::io_utils::{atomic_write_json, ensure_within_workspace, now_iso};
use crate::core::models::ACTIVE_STATUSES;
use crate::core::task_store::load_task;

use super::collector::{MAX_SINGLE_FILE_BYTES, collect_snapshot, load_existing_snapshot};
use super::redaction::redact_text;

const INDEX_HTML: &str = include_str!("../static/index.html");
const APP_JS: &str = include_str!("../static/app.js");
const STYLES_CSS: &str = include_str!("../static/styles.css");

const MAX_HEADER_BYTES: usize = 64 * 1024;
const MAX_BODY_BYTES: usize = 1024 * 1024;

#[derive(Clone)]
struct ServerState {
    ws_root: PathBuf,
    port: u16,
    instance_id: String,
    csrf_token: String,
    snapshot: Arc<Mutex<Value>>,
    refresh_lock: Arc<Mutex<()>>,
    shutdown: Arc<AtomicBool>,
}

pub struct DashboardHandle {
    pub ws_root: PathBuf,
    pub port: u16,
    pub instance_id: String,
    pub csrf_token: String,
    shutdown: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
}

impl DashboardHandle {
    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    pub fn stop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        if let Some(handle) = self.join.take() {
            let _ = handle.join();
        }
        cleanup_runtime_file(&self.ws_root, &self.instance_id);
    }
}

impl Drop for DashboardHandle {
    fn drop(&mut self) {
        self.stop();
    }
}

fn random_hex_token() -> String {
    format!("{:032x}", fastrand::u128(..))
}

pub fn runtime_file_path(ws_root: &Path) -> PathBuf {
    ws_root.join(".wb-runtime").join("dashboard.json")
}

fn write_runtime_file(
    ws_root: &Path,
    port: u16,
    instance_id: &str,
    csrf_token: &str,
) -> Result<(), WbError> {
    let rpath = runtime_file_path(ws_root);
    let mut m = Map::new();
    m.insert("schema".into(), Value::from(1));
    m.insert("pid".into(), Value::from(std::process::id() as i64));
    m.insert("port".into(), Value::from(port as i64));
    m.insert("host".into(), Value::String("127.0.0.1".to_string()));
    m.insert("instance_id".into(), Value::String(instance_id.to_string()));
    m.insert("csrf_token".into(), Value::String(csrf_token.to_string()));
    m.insert(
        "workspace".into(),
        Value::String(
            fs::canonicalize(ws_root)
                .unwrap_or_else(|_| ws_root.to_path_buf())
                .to_string_lossy()
                .into_owned(),
        ),
    );
    m.insert("started_at".into(), Value::String(now_iso()));
    let val = Value::Object(m);
    atomic_write_json(&rpath, &val)?;
    if let Ok(meta) = fs::metadata(&rpath) {
        let mut perms = meta.permissions();
        perms.set_mode(0o600);
        let _ = fs::set_permissions(&rpath, perms);
    }
    Ok(())
}

pub fn cleanup_runtime_file(ws_root: &Path, instance_id: &str) {
    let rpath = runtime_file_path(ws_root);
    if !rpath.is_file() {
        return;
    }
    if let Ok(raw) = fs::read_to_string(&rpath) {
        if let Ok(val) = serde_json::from_str::<Value>(&raw) {
            if val.get("instance_id").and_then(|v| v.as_str()) == Some(instance_id) {
                let _ = fs::remove_file(&rpath);
            }
        }
    }
}

pub fn start_dashboard_background(ws_root: &Path, port: u16) -> Result<DashboardHandle, WbError> {
    let ws_canon = fs::canonicalize(ws_root).unwrap_or_else(|_| ws_root.to_path_buf());
    let listener = TcpListener::bind(("127.0.0.1", port)).map_err(|e| {
        WbError::dashboard(format!(
            "无法在 127.0.0.1:{port} 启动本地看板服务（可能端口已被占用）: {e}"
        ))
    })?;
    let actual_port = listener
        .local_addr()
        .map(|a| a.port())
        .unwrap_or(port);
    listener
        .set_nonblocking(true)
        .map_err(|e| WbError::dashboard(format!("设置监听器非阻塞失败: {e}")))?;

    let snap_res = collect_snapshot(&ws_canon)?;
    let initial_snap = snap_res
        .get("snapshot")
        .cloned()
        .or_else(|| load_existing_snapshot(&ws_canon))
        .unwrap_or_else(|| json!({}));

    let instance_id = random_hex_token();
    let csrf_token = random_hex_token();
    write_runtime_file(&ws_canon, actual_port, &instance_id, &csrf_token)?;

    let shutdown = Arc::new(AtomicBool::new(false));
    let state = ServerState {
        ws_root: ws_canon.clone(),
        port: actual_port,
        instance_id: instance_id.clone(),
        csrf_token: csrf_token.clone(),
        snapshot: Arc::new(Mutex::new(initial_snap)),
        refresh_lock: Arc::new(Mutex::new(())),
        shutdown: shutdown.clone(),
    };

    let loop_ws = ws_canon.clone();
    let loop_id = instance_id.clone();
    let join = thread::spawn(move || {
        accept_loop(listener, state);
        cleanup_runtime_file(&loop_ws, &loop_id);
    });

    Ok(DashboardHandle {
        ws_root: ws_canon,
        port: actual_port,
        instance_id,
        csrf_token,
        shutdown,
        join: Some(join),
    })
}

static SIGNAL_STOP: AtomicBool = AtomicBool::new(false);

extern "C" fn handle_stop_signal(_sig: libc::c_int) {
    SIGNAL_STOP.store(true, Ordering::Release);
}

pub fn serve_foreground(ws_root: &Path, port: u16) -> Result<(), WbError> {
    SIGNAL_STOP.store(false, Ordering::Release);
    unsafe {
        libc::signal(libc::SIGINT, handle_stop_signal as *const () as libc::sighandler_t);
        libc::signal(libc::SIGTERM, handle_stop_signal as *const () as libc::sighandler_t);
    }

    let mut handle = start_dashboard_background(ws_root, port)?;
    while !handle.shutdown.load(Ordering::Acquire) {
        if SIGNAL_STOP.load(Ordering::Acquire) {
            handle.shutdown.store(true, Ordering::Release);
            break;
        }
        thread::sleep(Duration::from_millis(50));
    }
    handle.stop();
    Ok(())
}

fn accept_loop(listener: TcpListener, state: ServerState) {
    while !state.shutdown.load(Ordering::Acquire) {
        match listener.accept() {
            Ok((stream, _addr)) => {
                let st = state.clone();
                thread::spawn(move || handle_connection(stream, &st));
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(25));
            }
            Err(_) => {
                thread::sleep(Duration::from_millis(50));
            }
        }
    }
}

struct HttpRequest {
    method: String,
    target: String,
    headers: BTreeMap<String, String>,
}

struct HttpResponse {
    status: u16,
    reason: &'static str,
    content_type: &'static str,
    body: Vec<u8>,
    extra_headers: Vec<(&'static str, String)>,
}

impl HttpResponse {
    fn json(status: u16, reason: &'static str, val: &Value) -> Self {
        Self {
            status,
            reason,
            content_type: "application/json; charset=utf-8",
            body: serde_json::to_vec(val).unwrap_or_else(|_| b"{}".to_vec()),
            extra_headers: Vec::new(),
        }
    }

    fn text(status: u16, reason: &'static str, content_type: &'static str, body: &str) -> Self {
        Self {
            status,
            reason,
            content_type,
            body: body.as_bytes().to_vec(),
            extra_headers: Vec::new(),
        }
    }
}

fn write_response(stream: &mut TcpStream, resp: &HttpResponse) {
    let mut head = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\nX-Content-Type-Options: nosniff\r\nX-Frame-Options: DENY\r\nReferrer-Policy: no-referrer\r\nCache-Control: no-store\r\nContent-Security-Policy: default-src 'self'; script-src 'self'; style-src 'self'; object-src 'none'; base-uri 'self'; frame-ancestors 'none'\r\n",
        resp.status,
        resp.reason,
        resp.content_type,
        resp.body.len()
    );
    for (k, v) in &resp.extra_headers {
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    head.push_str("\r\n");
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(&resp.body);
    let _ = stream.flush();
    let _ = stream.shutdown(Shutdown::Both);
}

fn read_request(stream: &mut TcpStream) -> Result<HttpRequest, u16> {
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(10)));

    let mut reader = BufReader::new(stream.try_clone().map_err(|_| 500u16)?);
    let mut request_line = String::new();
    let n = reader.read_line(&mut request_line).map_err(|_| 400u16)?;
    if n == 0 || request_line.len() > 8192 {
        return Err(400);
    }
    let parts: Vec<&str> = request_line.trim_end_matches(['\r', '\n']).split_whitespace().collect();
    if parts.len() < 2 {
        return Err(400);
    }
    let method = parts[0].to_ascii_uppercase();
    let target = parts[1].to_string();

    let mut headers = BTreeMap::new();
    let mut header_bytes = request_line.len();
    loop {
        let mut line = String::new();
        let bytes = reader.read_line(&mut line).map_err(|_| 400u16)?;
        header_bytes += bytes;
        if header_bytes > MAX_HEADER_BYTES {
            return Err(431);
        }
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            break;
        }
        if let Some((k, v)) = trimmed.split_once(':') {
            headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
        }
    }

    if let Some(cl_str) = headers.get("content-length") {
        let cl = cl_str.parse::<usize>().map_err(|_| 400u16)?;
        if cl > MAX_BODY_BYTES {
            return Err(413);
        }
        if cl > 0 {
            let mut buf = vec![0u8; cl];
            reader.read_exact(&mut buf).map_err(|_| 400u16)?;
        }
    }

    Ok(HttpRequest {
        method,
        target,
        headers,
    })
}

fn is_allowed_host(headers: &BTreeMap<String, String>, port: u16) -> bool {
    let Some(host) = headers.get("host") else {
        return false;
    };
    let lower = host.trim().to_ascii_lowercase();
    lower == format!("127.0.0.1:{port}")
        || lower == format!("localhost:{port}")
        || lower == "127.0.0.1"
        || lower == "localhost"
}

fn is_allowed_origin_url(raw: &str, port: u16) -> bool {
    let Ok(parsed) = Url::parse(raw.trim()) else {
        return false;
    };
    if parsed.scheme() != "http" {
        return false;
    }
    let Some(host) = parsed.host_str() else {
        return false;
    };
    let h = host.to_ascii_lowercase();
    if h != "127.0.0.1" && h != "localhost" {
        return false;
    }
    match parsed.port() {
        Some(p) => p == port,
        None => port == 80,
    }
}

fn check_mutating_security(req: &HttpRequest, state: &ServerState) -> Result<(), HttpResponse> {
    if let Some(origin) = req.headers.get("origin") {
        if !is_allowed_origin_url(origin, state.port) {
            return Err(HttpResponse::json(
                403,
                "Forbidden",
                &json!({"error": "非法 Origin 来源"}),
            ));
        }
    }
    if let Some(referer) = req.headers.get("referer") {
        if !is_allowed_origin_url(referer, state.port) {
            return Err(HttpResponse::json(
                403,
                "Forbidden",
                &json!({"error": "非法 Referer 来源"}),
            ));
        }
    }
    let token_ok = req
        .headers
        .get("x-wb-csrf-token")
        .map(|t| t == &state.csrf_token)
        .unwrap_or(false);
    if !token_ok {
        return Err(HttpResponse::json(
            403,
            "Forbidden",
            &json!({"error": "缺少或无效的 CSRF Token"}),
        ));
    }
    Ok(())
}

pub fn csv_safe_cell(raw: &str) -> String {
    let trimmed = raw.trim_start_matches(' ');
    let needs_prefix = raw.starts_with(['=', '+', '-', '@', '\t', '\r'])
        || trimmed.starts_with(['=', '+', '-', '@']);
    let val = if needs_prefix {
        format!("'{raw}")
    } else {
        raw.to_string()
    };
    if val.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", val.replace('"', "\"\""))
    } else {
        val
    }
}

fn handle_api_doc(state: &ServerState, params: &BTreeMap<String, String>) -> HttpResponse {
    let task_id = params.get("task_id").map(String::as_str).unwrap_or("").trim();
    let raw_rel = params.get("path").map(String::as_str).unwrap_or("").trim();
    if task_id.is_empty() || raw_rel.is_empty() {
        return HttpResponse::json(
            400,
            "Bad Request",
            &json!({"error": "缺少 task_id 或 path 参数"}),
        );
    }

    if raw_rel.contains('\0')
        || raw_rel.contains('\\')
        || raw_rel.starts_with('/')
        || !raw_rel.to_ascii_lowercase().ends_with(".md")
    {
        return HttpResponse::json(
            403,
            "Forbidden",
            &json!({"error": "仅允许只读访问工作台内已登记的相对 .md 文档"}),
        );
    }

    let rel_path = Path::new(raw_rel);
    if rel_path.components().any(|c| {
        matches!(
            c,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    }) {
        return HttpResponse::json(
            403,
            "Forbidden",
            &json!({"error": "禁止路径穿越访问"}),
        );
    }

    let task = match load_task(&state.ws_root, task_id) {
        Ok(t) => t,
        Err(e) if e.exit_code == 3 => {
            return HttpResponse::json(404, "Not Found", &json!({"error": e.message}));
        }
        Err(e) => {
            return HttpResponse::json(400, "Bad Request", &json!({"error": e.message}));
        }
    };

    let norm_rel: String = rel_path
        .components()
        .filter_map(|c| match c {
            Component::Normal(s) => Some(s.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/");

    let mut registered = false;
    for field in ["docs", "materials"] {
        if let Some(arr) = task.get(field).and_then(|v| v.as_array()) {
            for entry in arr {
                if let Some(s) = entry.as_str() {
                    let s_norm = Path::new(s.trim())
                        .components()
                        .filter_map(|c| match c {
                            Component::Normal(p) => Some(p.to_string_lossy().into_owned()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join("/");
                    if s_norm == norm_rel {
                        registered = true;
                        break;
                    }
                }
            }
        }
    }
    if !registered {
        return HttpResponse::json(
            403,
            "Forbidden",
            &json!({"error": "目标文档未在任务 docs/materials 中登记"}),
        );
    }

    let candidate = state.ws_root.join(&norm_rel);
    let resolved = match ensure_within_workspace(&state.ws_root, &candidate) {
        Ok(p) => p,
        Err(_) => {
            return HttpResponse::json(
                403,
                "Forbidden",
                &json!({"error": "路径超出工作台边界或存在不安全符号链接"}),
            );
        }
    };

    if !resolved.is_file() {
        return HttpResponse::json(404, "Not Found", &json!({"error": "文档文件不存在"}));
    }
    let meta = match fs::metadata(&resolved) {
        Ok(m) => m,
        Err(e) => {
            return HttpResponse::json(
                500,
                "Internal Server Error",
                &json!({"error": format!("读取文档元信息失败: {e}")}),
            );
        }
    };
    if meta.len() > MAX_SINGLE_FILE_BYTES {
        return HttpResponse::json(
            413,
            "Payload Too Large",
            &json!({"error": "文档超过单文件 2MiB 安全预览上限"}),
        );
    }
    let raw_content = match fs::read_to_string(&resolved) {
        Ok(c) => c,
        Err(e) => {
            return HttpResponse::json(
                500,
                "Internal Server Error",
                &json!({"error": format!("读取文档失败: {e}")}),
            );
        }
    };

    HttpResponse::json(
        200,
        "OK",
        &json!({
            "task_id": task_id,
            "path": norm_rel,
            "content": redact_text(&raw_content),
        }),
    )
}

fn handle_export_csv(state: &ServerState, params: &BTreeMap<String, String>) -> HttpResponse {
    let snap = state.snapshot.lock().unwrap().clone();
    let tasks = snap
        .get("tasks")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    let f_status = params.get("status").map(String::as_str).unwrap_or("");
    let f_category = params.get("category").map(String::as_str).unwrap_or("");
    let f_priority = params.get("priority").map(String::as_str).unwrap_or("");
    let f_stale = matches!(
        params.get("stale").map(String::as_str),
        Some("1" | "true")
    );
    let f_blocked = matches!(
        params.get("blocked").map(String::as_str),
        Some("1" | "true")
    );
    let f_active = matches!(
        params.get("active").map(String::as_str),
        Some("1" | "true")
    );
    let f_q = params
        .get("q")
        .map(|s| s.trim().to_ascii_lowercase())
        .unwrap_or_default();

    let mut rows = vec!["id,title,status,priority,category,owner,stale_days,updated_at".to_string()];
    for t in tasks {
        let tid = t.get("id").and_then(|v| v.as_str()).unwrap_or("");
        let title = t.get("title").and_then(|v| v.as_str()).unwrap_or("");
        let status = t.get("status").and_then(|v| v.as_str()).unwrap_or("");
        let prio = t.get("priority").and_then(|v| v.as_str()).unwrap_or("");
        let cat = t.get("category").and_then(|v| v.as_str()).unwrap_or("");
        let owner = t.get("owner").and_then(|v| v.as_str()).unwrap_or("");
        let goal = t.get("goal").and_then(|v| v.as_str()).unwrap_or("");
        let stale_days = t.get("stale_days").and_then(|v| v.as_i64()).unwrap_or(0);
        let is_stale_7d = t
            .get("is_stale_7d")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let updated_at = t.get("updated_at").and_then(|v| v.as_str()).unwrap_or("");
        let blockers_non_empty = t
            .get("blockers")
            .and_then(|v| v.as_array())
            .map(|a| !a.is_empty())
            .unwrap_or(false);

        if !f_status.is_empty() && status != f_status {
            continue;
        }
        if !f_category.is_empty() && cat != f_category {
            continue;
        }
        if !f_priority.is_empty() && prio != f_priority {
            continue;
        }
        if f_stale && !is_stale_7d {
            continue;
        }
        if f_blocked && !(status == "blocked" || blockers_non_empty) {
            continue;
        }
        if f_active && !ACTIVE_STATUSES.contains(&status) {
            continue;
        }
        if !f_q.is_empty() {
            let comps = t
                .get("components")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str())
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .unwrap_or_default();
            let hay = format!("{tid} {title} {owner} {goal} {comps}").to_ascii_lowercase();
            if !hay.contains(&f_q) {
                continue;
            }
        }

        let cols = [
            csv_safe_cell(tid),
            csv_safe_cell(title),
            csv_safe_cell(status),
            csv_safe_cell(prio),
            csv_safe_cell(cat),
            csv_safe_cell(owner),
            csv_safe_cell(&stale_days.to_string()),
            csv_safe_cell(updated_at),
        ];
        rows.push(cols.join(","));
    }

    let mut resp = HttpResponse::text(200, "OK", "text/csv; charset=utf-8", &rows.join("\r\n"));
    resp.extra_headers.push((
        "Content-Disposition",
        "attachment; filename=\"workbench-tasks.csv\"".to_string(),
    ));
    resp
}

fn handle_connection(mut stream: TcpStream, state: &ServerState) {
    let req = match read_request(&mut stream) {
        Ok(r) => r,
        Err(code) => {
            let resp = HttpResponse::json(code, "Bad Request", &json!({"error": "无效 HTTP 请求"}));
            write_response(&mut stream, &resp);
            return;
        }
    };

    if !is_allowed_host(&req.headers, state.port) {
        let resp = HttpResponse::json(
            403,
            "Forbidden",
            &json!({"error": "非法 Host 请求头，仅允许本机回环地址访问"}),
        );
        write_response(&mut stream, &resp);
        return;
    }

    let full_url = format!("http://127.0.0.1:{}{}", state.port, req.target);
    let parsed_url = match Url::parse(&full_url) {
        Ok(u) => u,
        Err(_) => {
            let resp = HttpResponse::json(400, "Bad Request", &json!({"error": "无效请求路径"}));
            write_response(&mut stream, &resp);
            return;
        }
    };
    let path = parsed_url.path().to_string();
    let params: BTreeMap<String, String> = parsed_url
        .query_pairs()
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();

    let resp = match (req.method.as_str(), path.as_str()) {
        ("GET", "/" | "/index.html" | "/static/index.html") => {
            HttpResponse::text(200, "OK", "text/html; charset=utf-8", INDEX_HTML)
        }
        ("GET", "/static/app.js" | "/app.js") => {
            HttpResponse::text(200, "OK", "application/javascript; charset=utf-8", APP_JS)
        }
        ("GET", "/static/styles.css" | "/styles.css") => {
            HttpResponse::text(200, "OK", "text/css; charset=utf-8", STYLES_CSS)
        }
        ("GET", "/api/health") => HttpResponse::json(
            200,
            "OK",
            &json!({
                "ok": true,
                "instance_id": state.instance_id,
                "pid": std::process::id(),
                "port": state.port,
                "workspace": state.ws_root.to_string_lossy(),
            }),
        ),
        ("GET", "/api/bootstrap") => HttpResponse::json(
            200,
            "OK",
            &json!({
                "csrf_token": state.csrf_token,
                "instance_id": state.instance_id,
                "workspace": state.ws_root.to_string_lossy(),
            }),
        ),
        ("GET", "/api/snapshot") => {
            let snap = state.snapshot.lock().unwrap().clone();
            HttpResponse::json(200, "OK", &snap)
        }
        ("GET", "/api/doc") => handle_api_doc(state, &params),
        ("GET", "/api/export.csv") => handle_export_csv(state, &params),
        ("POST", "/api/refresh") => {
            if let Err(err_resp) = check_mutating_security(&req, state) {
                err_resp
            } else {
                let _guard = state.refresh_lock.lock().unwrap();
                match collect_snapshot(&state.ws_root) {
                    Ok(res) => {
                        if res.get("updated").and_then(|v| v.as_bool()) == Some(true) {
                            if let Some(new_snap) = res.get("snapshot") {
                                *state.snapshot.lock().unwrap() = new_snap.clone();
                            }
                        }
                        HttpResponse::json(
                            200,
                            "OK",
                            &json!({
                                "updated": res.get("updated").cloned().unwrap_or(Value::Bool(false)),
                                "aborted_reason": res.get("aborted_reason").cloned().unwrap_or(Value::Null),
                                "generated_at": res.get("generated_at").cloned().unwrap_or(Value::Null),
                            }),
                        )
                    }
                    Err(e) => HttpResponse::json(
                        500,
                        "Internal Server Error",
                        &json!({"error": e.message}),
                    ),
                }
            }
        }
        ("POST", "/api/shutdown") => {
            if let Err(err_resp) = check_mutating_security(&req, state) {
                err_resp
            } else {
                state.shutdown.store(true, Ordering::Release);
                HttpResponse::json(
                    200,
                    "OK",
                    &json!({
                        "stopping": true,
                        "instance_id": state.instance_id,
                    }),
                )
            }
        }
        (_, "/api/refresh" | "/api/shutdown") => HttpResponse::json(
            405,
            "Method Not Allowed",
            &json!({"error": "仅支持 POST 方法"}),
        ),
        _ => HttpResponse::json(404, "Not Found", &json!({"error": "Not Found"})),
    };

    write_response(&mut stream, &resp);
}
