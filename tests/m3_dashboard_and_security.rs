use std::fs::{self, File};
use std::os::unix::fs::symlink;
use std::path::PathBuf;
use std::process::Command;

use trestle::core::errors::EXIT_DASHBOARD_ERROR;
use trestle::core::task_store::{CreateTaskOpts, create_task, manage_doc, manage_material};
use trestle::dashboard::collector::{collect_snapshot, collect_snapshot_with_hook};
use trestle::dashboard::process::http_request_loopback;
use trestle::dashboard::server::start_dashboard_background;
use trestle::workspace::init_ws::{InitOpts, init_workbench};
use serde_json::Value;
use tempfile::TempDir;

fn wb_bin() -> &'static str {
    env!("CARGO_BIN_EXE_wb")
}

fn setup_ws(tmp: &TempDir, name: &str) -> PathBuf {
    let ws = tmp.path().join(name);
    init_workbench(InitOpts {
        target_path: ws.to_str().unwrap(),
        name: Some(name),
        yes: true,
        ..Default::default()
    })
    .unwrap();
    ws
}

#[test]
fn m3_redaction_caps_and_single_task_fault_isolation() {
    let tmp = TempDir::new().unwrap();
    let ws = setup_ws(&tmp, "ws-collect");

    create_task(
        &ws,
        CreateTaskOpts {
            task_id: "T-GOOD",
            title: "Secret Bearer topsecret123456 token=ghp_0123456789abcdef1234",
            goal: "Fetch https://alice:s3cr3tPass@git.example.com/org/repo.git?api_key=abc123",
            ..Default::default()
        },
    )
    .unwrap();

    // T-BAD has corrupted task.yaml -> isolated without breaking T-GOOD
    let bad_dir = ws.join("tasks/T-BAD");
    fs::create_dir_all(&bad_dir).unwrap();
    fs::write(bad_dir.join("task.yaml"), "schema: [broken").unwrap();

    // T-HUGE-FILE has > 2 MiB progress.md -> triggers FILE_TOO_LARGE
    create_task(
        &ws,
        CreateTaskOpts {
            task_id: "T-HUGE-FILE",
            title: "Large single file",
            ..Default::default()
        },
    )
    .unwrap();
    let pfile = File::create(ws.join("tasks/T-HUGE-FILE/progress.md")).unwrap();
    pfile.set_len(2 * 1024 * 1024 + 100).unwrap();

    let res = collect_snapshot(&ws).unwrap();
    assert_eq!(res["updated"].as_bool(), Some(true));
    let snap = &res["snapshot"];

    // T-GOOD collected and all secrets masked
    let tasks = snap["tasks"].as_array().unwrap();
    let t_good = tasks.iter().find(|t| t["id"] == "T-GOOD").unwrap();
    let title = t_good["title"].as_str().unwrap();
    let goal = t_good["goal"].as_str().unwrap();
    assert!(!title.contains("topsecret123456"));
    assert!(!title.contains("ghp_0123456789abcdef1234"));
    assert!(!goal.contains("s3cr3tPass"));
    assert!(!goal.contains("abc123"));
    assert!(title.contains("***REDACTED***"));
    assert!(goal.contains("***REDACTED***"));

    // T-BAD and T-HUGE-FILE recorded in data_issues
    let issues = snap["data_issues"].as_array().unwrap();
    assert!(issues.iter().any(|i| i["task_id"] == "T-BAD"));
    assert!(
        issues
            .iter()
            .any(|i| i["task_id"] == "T-HUGE-FILE" && i["code"] == "FILE_TOO_LARGE")
    );
}

#[test]
fn m3_mid_collection_mutation_retains_old_snapshot() {
    let tmp = TempDir::new().unwrap();
    let ws = setup_ws(&tmp, "ws-mid");
    create_task(
        &ws,
        CreateTaskOpts {
            task_id: "T-1",
            title: "Original title",
            ..Default::default()
        },
    )
    .unwrap();

    let first = collect_snapshot(&ws).unwrap();
    assert_eq!(first["updated"].as_bool(), Some(true));
    let snap_path = ws.join(".wb-runtime/snapshot.json");
    let old_bytes = fs::read(&snap_path).unwrap();

    let mut mutated = false;
    let second = collect_snapshot_with_hook(
        &ws,
        Some(|| {
            if !mutated {
                mutated = true;
                fs::write(ws.join("tasks/T-1/progress.md"), "# concurrent edit\n").unwrap();
            }
        }),
    )
    .unwrap();

    assert_eq!(second["updated"].as_bool(), Some(false));
    assert!(second["aborted_reason"].as_str().is_some());
    assert_eq!(fs::read(&snap_path).unwrap(), old_bytes);
}

#[test]
fn m3_http_server_security_doc_boundary_and_csv_escaping() {
    let tmp = TempDir::new().unwrap();
    let ws = setup_ws(&tmp, "ws-srv");

    create_task(
        &ws,
        CreateTaskOpts {
            task_id: "T-SEC",
            title: "=cmd|'/C calc'!A0",
            goal: "Verify security headers and endpoints",
            ..Default::default()
        },
    )
    .unwrap();

    // Safe registered doc (with a token to verify redaction)
    fs::write(
        ws.join("docs/safe.md"),
        "# Design\napi_key: secret_demo_value_123\n",
    )
    .unwrap();
    manage_doc(&ws, "T-SEC", "add", "docs/safe.md").unwrap();

    // Unregistered doc inside workspace
    fs::write(ws.join("docs/unregistered.md"), "# Secret\n").unwrap();

    // Registered non-md material
    fs::write(ws.join("analysis/raw.txt"), "plain").unwrap();
    manage_material(&ws, "T-SEC", "add", "analysis/raw.txt").unwrap();

    // Symlink pointing outside workspace
    let outside_secret = tmp.path().join("outside-secret.md");
    fs::write(&outside_secret, "TOP SECRET OUTSIDE").unwrap();
    symlink(&outside_secret, ws.join("docs/escape-link.md")).unwrap();

    let mut handle = start_dashboard_background(&ws, 0).unwrap();
    let port = handle.port;

    // Port conflict must fail with EXIT_DASHBOARD_ERROR (1)
    let conflict_err = match start_dashboard_background(&ws, port) {
        Ok(_) => panic!("second bind on occupied port must fail"),
        Err(e) => e,
    };
    assert_eq!(conflict_err.exit_code, EXIT_DASHBOARD_ERROR);

    // 1. Non-loopback Host header rejected with 403
    let (host_code, _) =
        http_request_loopback(port, "GET", "/api/snapshot", &[("Host", "evil.example.com")]).unwrap();
    assert_eq!(host_code, 403);

    // 2. Mutating POST without CSRF token -> 403
    let (no_csrf_code, _) = http_request_loopback(port, "POST", "/api/refresh", &[]).unwrap();
    assert_eq!(no_csrf_code, 403);

    // 3. Mutating POST with bad Origin even with valid CSRF token -> 403
    let (bad_origin_code, _) = http_request_loopback(
        port,
        "POST",
        "/api/refresh",
        &[
            ("X-WB-CSRF-Token", &handle.csrf_token),
            ("Origin", "http://evil.example.com"),
        ],
    )
    .unwrap();
    assert_eq!(bad_origin_code, 403);

    // 4. Valid CSRF + loopback Origin -> 200
    let origin = format!("http://127.0.0.1:{port}");
    let (good_refresh, _) = http_request_loopback(
        port,
        "POST",
        "/api/refresh",
        &[
            ("X-WB-CSRF-Token", &handle.csrf_token),
            ("Origin", &origin),
        ],
    )
    .unwrap();
    assert_eq!(good_refresh, 200);

    // 5. /api/doc boundary checks
    let (code_ok_doc, body_ok_doc) =
        http_request_loopback(port, "GET", "/api/doc?task_id=T-SEC&path=docs/safe.md", &[])
            .unwrap();
    assert_eq!(code_ok_doc, 200);
    assert!(!body_ok_doc.contains("secret_demo_value_123"));
    assert!(body_ok_doc.contains("***REDACTED***"));

    let (code_unreg, _) = http_request_loopback(
        port,
        "GET",
        "/api/doc?task_id=T-SEC&path=docs/unregistered.md",
        &[],
    )
    .unwrap();
    assert_eq!(code_unreg, 403);

    let (code_non_md, _) = http_request_loopback(
        port,
        "GET",
        "/api/doc?task_id=T-SEC&path=analysis/raw.txt",
        &[],
    )
    .unwrap();
    assert_eq!(code_non_md, 403);

    let (code_trav, _) = http_request_loopback(
        port,
        "GET",
        "/api/doc?task_id=T-SEC&path=../outside-secret.md",
        &[],
    )
    .unwrap();
    assert_eq!(code_trav, 403);

    // Even if symlink path were injected directly into task.yaml docs, canonicalize check blocks with 403
    let tfile = ws.join("tasks/T-SEC/task.yaml");
    let raw_yaml = fs::read_to_string(&tfile).unwrap();
    fs::write(
        &tfile,
        raw_yaml.replace("docs/safe.md", "docs/escape-link.md"),
    )
    .unwrap();
    let (code_sym, _) = http_request_loopback(
        port,
        "GET",
        "/api/doc?task_id=T-SEC&path=docs/escape-link.md",
        &[],
    )
    .unwrap();
    assert_eq!(code_sym, 403);

    // 6. CSV export formula escaping
    let (csv_code, csv_body) = http_request_loopback(port, "GET", "/api/export.csv", &[]).unwrap();
    assert_eq!(csv_code, 200);
    assert!(csv_body.contains("'=cmd|'/C calc'!A0"));

    handle.stop();
}

#[test]
fn m3_daemon_multi_workspace_isolation_and_instance_id_guard() {
    let tmp = TempDir::new().unwrap();
    let ws_a = setup_ws(&tmp, "ws-a");
    let ws_b = setup_ws(&tmp, "ws-b");

    // Start two independent daemons on ephemeral ports
    let out_a = Command::new(wb_bin())
        .args([
            "--workspace",
            ws_a.to_str().unwrap(),
            "dashboard",
            "serve",
            "--port",
            "0",
            "--json",
        ])
        .output()
        .unwrap();
    assert_eq!(
        out_a.status.code(),
        Some(0),
        "ws_a serve stderr: {}",
        String::from_utf8_lossy(&out_a.stderr)
    );
    let info_a: Value = serde_json::from_slice(&out_a.stdout).unwrap();
    let port_a = info_a["port"].as_u64().unwrap() as u16;
    let id_a = info_a["instance_id"].as_str().unwrap().to_string();

    let out_b = Command::new(wb_bin())
        .args([
            "--workspace",
            ws_b.to_str().unwrap(),
            "dashboard",
            "serve",
            "--port",
            "0",
            "--json",
        ])
        .output()
        .unwrap();
    assert_eq!(out_b.status.code(), Some(0));
    let info_b: Value = serde_json::from_slice(&out_b.stdout).unwrap();
    let id_b = info_b["instance_id"].as_str().unwrap().to_string();
    assert_ne!(id_a, id_b);

    // Tamper ws_a's dashboard.json instance_id -> stop MUST fail with exit code 1 and keep daemon alive
    let rpath_a = ws_a.join(".wb-runtime/dashboard.json");
    let orig_a = fs::read_to_string(&rpath_a).unwrap();
    let mut tampered: Value = serde_json::from_str(&orig_a).unwrap();
    tampered["instance_id"] = Value::String("wrong-instance-id".into());
    fs::write(&rpath_a, serde_json::to_string_pretty(&tampered).unwrap()).unwrap();

    let bad_stop = Command::new(wb_bin())
        .args(["--workspace", ws_a.to_str().unwrap(), "dashboard", "stop"])
        .output()
        .unwrap();
    assert_eq!(bad_stop.status.code(), Some(1));

    // Verify ws_a server is STILL reachable
    let (hcode, _) = http_request_loopback(port_a, "GET", "/api/health", &[]).unwrap();
    assert_eq!(hcode, 200);

    // Restore ws_a dashboard.json -> stop succeeds cleanly, ws_b unaffected
    fs::write(&rpath_a, orig_a).unwrap();
    let good_stop_a = Command::new(wb_bin())
        .args([
            "--workspace",
            ws_a.to_str().unwrap(),
            "dashboard",
            "stop",
            "--json",
        ])
        .output()
        .unwrap();
    assert_eq!(good_stop_a.status.code(), Some(0));

    let st_b = Command::new(wb_bin())
        .args([
            "--workspace",
            ws_b.to_str().unwrap(),
            "dashboard",
            "status",
            "--json",
        ])
        .output()
        .unwrap();
    assert_eq!(st_b.status.code(), Some(0));
    let st_b_json: Value = serde_json::from_slice(&st_b.stdout).unwrap();
    assert_eq!(st_b_json["running"].as_bool(), Some(true));

    // Cleanup ws_b
    let good_stop_b = Command::new(wb_bin())
        .args(["--workspace", ws_b.to_str().unwrap(), "dashboard", "stop"])
        .output()
        .unwrap();
    assert_eq!(good_stop_b.status.code(), Some(0));
}
