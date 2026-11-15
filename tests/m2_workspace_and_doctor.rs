use std::fs;
use std::process::Command;

use serde_json::Value;
use tempfile::TempDir;

fn wb_bin() -> &'static str {
    env!("CARGO_BIN_EXE_wb")
}

#[test]
fn m2_workspace_locator_precedence_and_exit_3() {
    let tmp = TempDir::new().unwrap();
    let ws1 = tmp.path().join("ws1");
    let ws2 = tmp.path().join("ws2");
    let nested = ws2.join("docs").join("sub");
    let outside = tmp.path().join("outside");
    fs::create_dir_all(&outside).unwrap();

    for ws in [&ws1, &ws2] {
        let out = Command::new(wb_bin())
            .args(["init", ws.to_str().unwrap(), "--yes"])
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(0));
    }
    fs::create_dir_all(&nested).unwrap();

    // Seed unique task in ws1 vs ws2
    assert_eq!(
        Command::new(wb_bin())
            .args(["--workspace", ws1.to_str().unwrap(), "new", "T-WS1", "--title", "In WS1"])
            .status()
            .unwrap()
            .code(),
        Some(0)
    );
    assert_eq!(
        Command::new(wb_bin())
            .args(["--workspace", ws2.to_str().unwrap(), "new", "T-WS2", "--title", "In WS2"])
            .status()
            .unwrap()
            .code(),
        Some(0)
    );

    // 1. Upward search from nested subdir finds ws2
    let out_up = Command::new(wb_bin())
        .args(["task", "list", "--json"])
        .current_dir(&nested)
        .env_remove("WB_ROOT")
        .output()
        .unwrap();
    assert_eq!(out_up.status.code(), Some(0));
    let arr_up: Vec<Value> = serde_json::from_slice(&out_up.stdout).unwrap();
    assert_eq!(arr_up[0]["id"].as_str(), Some("T-WS2"));

    // 2. WB_ROOT overrides upward cwd search
    let out_env = Command::new(wb_bin())
        .args(["task", "list", "--json"])
        .current_dir(&nested)
        .env("WB_ROOT", ws1.to_str().unwrap())
        .output()
        .unwrap();
    assert_eq!(out_env.status.code(), Some(0));
    let arr_env: Vec<Value> = serde_json::from_slice(&out_env.stdout).unwrap();
    assert_eq!(arr_env[0]["id"].as_str(), Some("T-WS1"));

    // 3. --workspace overrides WB_ROOT and cwd
    let out_cli = Command::new(wb_bin())
        .args(["--workspace", ws2.to_str().unwrap(), "task", "list", "--json"])
        .current_dir(&ws1)
        .env("WB_ROOT", ws1.to_str().unwrap())
        .output()
        .unwrap();
    assert_eq!(out_cli.status.code(), Some(0));
    let arr_cli: Vec<Value> = serde_json::from_slice(&out_cli.stdout).unwrap();
    assert_eq!(arr_cli[0]["id"].as_str(), Some("T-WS2"));

    // 4. Outside any workbench -> EXIT_NOT_FOUND (3)
    let out_missing = Command::new(wb_bin())
        .args(["task", "list"])
        .current_dir(&outside)
        .env_remove("WB_ROOT")
        .output()
        .unwrap();
    assert_eq!(out_missing.status.code(), Some(3));
}

#[test]
fn m2_init_dry_run_example_and_non_destructive_reinit() {
    let tmp = TempDir::new().unwrap();
    let ws = tmp.path().join("fresh-ws");

    // Dry-run does not create directory
    let dry = Command::new(wb_bin())
        .args(["init", ws.to_str().unwrap(), "--dry-run", "--example", "--json"])
        .output()
        .unwrap();
    assert_eq!(dry.status.code(), Some(0));
    assert!(!ws.exists());

    // Actual init with --yes --example --agent-guide
    let real = Command::new(wb_bin())
        .args([
            "init",
            ws.to_str().unwrap(),
            "--yes",
            "--example",
            "--agent-guide",
            "--name",
            "custom-bench",
        ])
        .output()
        .unwrap();
    assert_eq!(real.status.code(), Some(0));
    assert!(ws.join(".wb.yml").is_file());
    assert!(ws.join("AGENTS.md").is_file());
    assert!(ws.join("tasks/TASK-EXAMPLE-001/task.yaml").is_file());

    // Customize existing AGENTS.md and .wb.yml and re-run init
    fs::write(ws.join("AGENTS.md"), "# CUSTOM AGENTS GUIDE\n").unwrap();
    let cfg_before = fs::read_to_string(ws.join(".wb.yml")).unwrap();
    let reinit = Command::new(wb_bin())
        .args(["init", ws.to_str().unwrap(), "--yes", "--agent-guide"])
        .output()
        .unwrap();
    assert_eq!(reinit.status.code(), Some(0));
    assert_eq!(
        fs::read_to_string(ws.join("AGENTS.md")).unwrap(),
        "# CUSTOM AGENTS GUIDE\n"
    );
    assert_eq!(fs::read_to_string(ws.join(".wb.yml")).unwrap(), cfg_before);
}

#[test]
fn m2_doctor_readonly_and_upgrade_backup_apply() {
    let tmp = TempDir::new().unwrap();
    let ws = tmp.path().join("ws-doc");
    assert_eq!(
        Command::new(wb_bin())
            .args(["init", ws.to_str().unwrap(), "--yes", "--example"])
            .status()
            .unwrap()
            .code(),
        Some(0)
    );

    // Healthy workspace returns 0
    let ok_doc = Command::new(wb_bin())
        .args(["--workspace", ws.to_str().unwrap(), "doctor", "--json"])
        .output()
        .unwrap();
    assert_eq!(ok_doc.status.code(), Some(0));
    let ok_json: Value = serde_json::from_slice(&ok_doc.stdout).unwrap();
    assert_eq!(ok_json["healthy"].as_bool(), Some(true));

    // Break a task.yaml -> doctor reports error (exit 2) without modifying files
    let bad_dir = ws.join("tasks/TASK-BROKEN");
    fs::create_dir_all(&bad_dir).unwrap();
    let bad_content = "schema: 1\nid: TASK-BROKEN\nstatus: invalid_state\n";
    fs::write(bad_dir.join("task.yaml"), bad_content).unwrap();

    let bad_doc = Command::new(wb_bin())
        .args(["--workspace", ws.to_str().unwrap(), "doctor", "--json"])
        .output()
        .unwrap();
    assert_eq!(bad_doc.status.code(), Some(2));
    let bad_json: Value = serde_json::from_slice(&bad_doc.stdout).unwrap();
    assert_eq!(bad_json["healthy"].as_bool(), Some(false));
    assert_eq!(fs::read_to_string(bad_dir.join("task.yaml")).unwrap(), bad_content);

    // Remove broken task before upgrade test
    fs::remove_dir_all(&bad_dir).unwrap();
    fs::remove_file(ws.join("WORKBENCH.md")).unwrap();

    let up_dry = Command::new(wb_bin())
        .args(["--workspace", ws.to_str().unwrap(), "workspace", "upgrade", "--dry-run", "--json"])
        .output()
        .unwrap();
    assert_eq!(up_dry.status.code(), Some(0));
    assert!(!ws.join("WORKBENCH.md").exists());

    let up_apply = Command::new(wb_bin())
        .args(["--workspace", ws.to_str().unwrap(), "workspace", "upgrade", "--apply", "--json"])
        .output()
        .unwrap();
    assert_eq!(up_apply.status.code(), Some(0));
    let up_json: Value = serde_json::from_slice(&up_apply.stdout).unwrap();
    assert!(ws.join("WORKBENCH.md").is_file());
    let backup_dir = up_json["backup_dir"].as_str().unwrap();
    assert!(fs::metadata(backup_dir).unwrap().is_dir());
}
