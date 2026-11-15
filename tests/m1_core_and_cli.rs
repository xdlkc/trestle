use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::thread;

use trestle::core::context::{build_context_data, format_context_plain};
use trestle::core::delivery::apply_delivery;
use trestle::core::errors::{EXIT_CONFLICT, EXIT_VALIDATION_ERROR};
use trestle::core::evidence::{
    confirm_action, evaluate_actions_status, export_evidence, import_evidence,
};
use trestle::core::models::compute_action_scope_sha256;
use trestle::core::review::{REVIEW_DIMENSIONS, prepare_review};
use trestle::core::sync::sync_task;
use trestle::core::task_store::{
    CreateTaskOpts, append_progress, create_task, load_session_ledger, load_task,
    manage_blockers, manage_next_steps, record_session,
};
use trestle::workspace::init_ws::{InitOpts, init_workbench};
use serde_json::{Value, json};
use tempfile::TempDir;

fn wb_bin() -> &'static str {
    env!("CARGO_BIN_EXE_wb")
}

fn setup_ws(tmp: &TempDir) -> std::path::PathBuf {
    let ws = tmp.path().join("ws");
    init_workbench(InitOpts {
        target_path: ws.to_str().unwrap(),
        name: Some("test-ws"),
        yes: true,
        ..Default::default()
    })
    .unwrap();
    ws
}

fn run_git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .expect("git should run");
    assert!(
        out.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[test]
fn m1_persistent_lock_and_concurrent_writes() {
    let tmp = TempDir::new().unwrap();
    let ws = setup_ws(&tmp);
    create_task(
        &ws,
        CreateTaskOpts {
            task_id: "T-CONC",
            title: "Concurrent task",
            priority: "P1",
            category: "requirement",
            ..Default::default()
        },
    )
    .unwrap();

    let lock_file = ws.join("tasks").join("T-CONC").join(".lock");
    assert!(lock_file.is_file(), "task .lock must be created and retained");

    let ws_arc = Arc::new(ws.clone());
    let mut handles = Vec::new();
    for i in 0..8 {
        let w = Arc::clone(&ws_arc);
        handles.push(thread::spawn(move || {
            append_progress(&w, "T-CONC", &format!("worker-{i}-progress")).unwrap();
            manage_next_steps(&w, "T-CONC", "add", &format!("step-{i}"), None).unwrap();
        }));
    }
    for h in handles {
        h.join().unwrap();
    }

    assert!(lock_file.is_file(), "persistent lock file must never be deleted");
    let prog = fs::read_to_string(ws.join("tasks/T-CONC/progress.md")).unwrap();
    for i in 0..8 {
        assert!(prog.contains(&format!("worker-{i}-progress")));
    }
    let task = load_task(&ws, "T-CONC").unwrap();
    assert_eq!(task["next_steps"].as_array().unwrap().len(), 8);
}

#[test]
fn m1_session_prefixes_and_deduplication() {
    let tmp = TempDir::new().unwrap();
    let ws = setup_ws(&tmp);
    create_task(
        &ws,
        CreateTaskOpts {
            task_id: "T-SESS",
            title: "Session test",
            category: "requirement",
            origin_session_id: Some("sess-1"),
            origin_host: Some("devbox"),
            origin_title: Some("[FD] Initial design"),
            ..Default::default()
        },
    )
    .unwrap();

    // Associated CR session does not alter task primary category
    record_session(
        &ws,
        Some("T-SESS"),
        "sess-2",
        "laptop",
        "[CR] Review follow-up",
        None,
        "associated",
    )
    .unwrap();

    // Updating same (session_id, host) deduplicates instead of duplicating
    record_session(
        &ws,
        Some("T-SESS"),
        "sess-2",
        "laptop",
        "[CR] Review follow-up v2",
        None,
        "associated",
    )
    .unwrap();

    let task = load_task(&ws, "T-SESS").unwrap();
    assert_eq!(task["category"].as_str(), Some("requirement"));
    assert_eq!(task["sessions"]["associated"].as_array().unwrap().len(), 1);
    assert_eq!(
        task["sessions"]["associated"][0]["category"].as_str(),
        Some("cr")
    );

    let ledger = load_session_ledger(&ws).unwrap();
    assert_eq!(ledger["sessions"].as_array().unwrap().len(), 2);

    // Multi-prefix rejected
    let multi = record_session(
        &ws,
        Some("T-SESS"),
        "sess-bad",
        "host",
        "[FD][CR] Invalid multi prefix",
        None,
        "associated",
    )
    .unwrap_err();
    assert_eq!(multi.exit_code, EXIT_VALIDATION_ERROR);

    // Unknown prefix rejected
    let unknown = record_session(
        &ws,
        Some("T-SESS"),
        "sess-bad2",
        "host",
        "[XX] Unknown prefix",
        None,
        "associated",
    )
    .unwrap_err();
    assert_eq!(unknown.exit_code, EXIT_VALIDATION_ERROR);
}

#[test]
fn m1_delivery_revision_conflict_history_and_cycle_detection() {
    let tmp = TempDir::new().unwrap();
    let ws = setup_ws(&tmp);
    create_task(
        &ws,
        CreateTaskOpts {
            task_id: "T-DELIV",
            title: "Delivery checks",
            ..Default::default()
        },
    )
    .unwrap();

    let draft1 = json!({
        "items": [{
            "id": "repo-main",
            "repo": "sample-repo",
            "source_branch": "feat/cache",
            "target_branch": "main",
            "allowed_paths": ["src/"],
            "depends_on": []
        }],
        "actions": [{
            "id": "act-test",
            "kind": "auto_check",
            "title": "Run cargo test",
            "item_id": "repo-main",
            "scope": {"cmd": "cargo test", "paths": ["src/"]}
        }]
    });

    let rev1 = apply_delivery(&ws, "T-DELIV", &draft1, None).unwrap();
    assert_eq!(rev1["revision"].as_i64(), Some(1));
    assert!(
        ws.join("tasks/T-DELIV/delivery-history/rev-0001.yaml")
            .is_file()
    );

    // Missing or stale expected_revision fails with conflict exit code 4
    let conflict = apply_delivery(&ws, "T-DELIV", &draft1, Some(0)).unwrap_err();
    assert_eq!(conflict.exit_code, EXIT_CONFLICT);

    // Cycle in depends_on fails with validation exit code 2
    let cyclic = json!({
        "items": [
            {"id": "a", "repo": "r1", "source_branch": "feat/a", "depends_on": ["b"]},
            {"id": "b", "repo": "r2", "source_branch": "feat/b", "depends_on": ["a"]}
        ],
        "actions": []
    });
    let cycle_err = apply_delivery(&ws, "T-DELIV", &cyclic, Some(1)).unwrap_err();
    assert_eq!(cycle_err.exit_code, EXIT_VALIDATION_ERROR);

    // Invalid branch name rejected
    let bad_branch = json!({
        "items": [
            {"id": "a", "repo": "r1", "source_branch": "feat/../escape"}
        ],
        "actions": []
    });
    let branch_err = apply_delivery(&ws, "T-DELIV", &bad_branch, Some(1)).unwrap_err();
    assert_eq!(branch_err.exit_code, EXIT_VALIDATION_ERROR);
}

#[test]
fn m1_action_confirm_invalidation_and_never_infers_progress() {
    let tmp = TempDir::new().unwrap();
    let ws = setup_ws(&tmp);
    create_task(
        &ws,
        CreateTaskOpts {
            task_id: "T-ACT",
            title: "Evidence gating",
            ..Default::default()
        },
    )
    .unwrap();

    let draft = json!({
        "items": [{"id": "core", "repo": "app", "source_branch": "feat/a", "target_branch": "main"}],
        "actions": [{
            "id": "act-review",
            "kind": "manual_review",
            "title": "Code review",
            "item_id": "core",
            "scope": {"target": "src/auth.rs", "checklist": "v1"}
        }]
    });
    let rev1 = apply_delivery(&ws, "T-ACT", &draft, None).unwrap();
    let sha1 = rev1["actions"][0]["scope_sha256"]
        .as_str()
        .unwrap()
        .to_string();

    // Writing progress.md claim never satisfies evidence
    append_progress(&ws, "T-ACT", "Code review act-review passed and approved").unwrap();
    let st_before = evaluate_actions_status(&ws, "T-ACT").unwrap();
    assert_eq!(st_before[0]["valid"].as_bool(), Some(false));
    assert_eq!(st_before[0]["status"].as_str(), Some("unconfirmed"));

    // Wrong digest rejected with conflict
    let wrong = confirm_action(
        &ws,
        "T-ACT",
        "act-review",
        "alice",
        "manual-check",
        "deadbeef",
        "",
    )
    .unwrap_err();
    assert_eq!(wrong.exit_code, EXIT_CONFLICT);

    // Valid confirm writes receipt and validates action
    confirm_action(
        &ws,
        "T-ACT",
        "act-review",
        "alice",
        "cr-101",
        &sha1,
        "LGTM",
    )
    .unwrap();
    let st_valid = evaluate_actions_status(&ws, "T-ACT").unwrap();
    assert_eq!(st_valid[0]["valid"].as_bool(), Some(true));
    assert_eq!(st_valid[0]["status"].as_str(), Some("valid"));

    // Import & export extra evidence file
    let artifact = tmp.path().join("report.log");
    fs::write(&artifact, "test ok\n").unwrap();
    import_evidence(&ws, "T-ACT", &artifact, None, "cargo output").unwrap();
    let export_dir = tmp.path().join("exported");
    let exp = export_evidence(&ws, "T-ACT", &export_dir).unwrap();
    assert!(export_dir.join("confirm-act-review.json").is_file());
    assert!(export_dir.join("report.log").is_file());
    assert!(exp["files"].as_array().unwrap().len() >= 2);

    // Updating scope invalidates prior receipt -> status == "stale"
    let draft_v2 = json!({
        "items": [{"id": "core", "repo": "app", "source_branch": "feat/a", "target_branch": "main"}],
        "actions": [{
            "id": "act-review",
            "kind": "manual_review",
            "title": "Code review",
            "item_id": "core",
            "scope": {"target": "src/auth.rs", "checklist": "v2"}
        }]
    });
    let expected_v2_sha = compute_action_scope_sha256(draft_v2["actions"][0].as_object().unwrap());
    assert_ne!(sha1, expected_v2_sha);
    apply_delivery(&ws, "T-ACT", &draft_v2, Some(1)).unwrap();

    let st_stale = evaluate_actions_status(&ws, "T-ACT").unwrap();
    assert_eq!(st_stale[0]["valid"].as_bool(), Some(false));
    assert_eq!(st_stale[0]["status"].as_str(), Some("stale"));

    // Removing receipt file marks status == "missing_receipt"
    fs::remove_file(ws.join("tasks/T-ACT/evidence/confirm-act-review.json")).unwrap();
    let st_missing = evaluate_actions_status(&ws, "T-ACT").unwrap();
    assert_eq!(st_missing[0]["valid"].as_bool(), Some(false));
    assert_eq!(st_missing[0]["status"].as_str(), Some("missing_receipt"));
}

#[test]
fn m1_sync_readonly_git_and_no_git_fallback() {
    let tmp = TempDir::new().unwrap();
    let ws = setup_ws(&tmp);
    let repos_root = tmp.path().join("repos");
    let repo_dir = repos_root.join("svc");
    fs::create_dir_all(repo_dir.join("src")).unwrap();
    fs::create_dir_all(repo_dir.join("docs")).unwrap();

    run_git(&repo_dir, &["init", "-b", "main"]);
    run_git(&repo_dir, &["config", "user.email", "dev@example.com"]);
    run_git(&repo_dir, &["config", "user.name", "Dev"]);
    fs::write(repo_dir.join("src/lib.rs"), "pub fn a() {}\n").unwrap();
    run_git(&repo_dir, &["add", "."]);
    run_git(&repo_dir, &["commit", "-m", "init"]);
    run_git(&repo_dir, &["checkout", "-b", "feat/work"]);
    let head = run_git(&repo_dir, &["rev-parse", "HEAD"]);

    // Make one in-scope dirty file and one out-of-scope dirty file
    fs::write(repo_dir.join("src/lib.rs"), "pub fn a() { /* mod */ }\n").unwrap();
    fs::write(repo_dir.join("docs/extra.txt"), "untracked outside allowed").unwrap();

    create_task(
        &ws,
        CreateTaskOpts {
            task_id: "T-SYNC",
            title: "Git observation",
            ..Default::default()
        },
    )
    .unwrap();
    let task_before = fs::read_to_string(ws.join("tasks/T-SYNC/task.yaml")).unwrap();

    let draft = json!({
        "items": [{
            "id": "svc-item",
            "repo": "svc",
            "source_branch": "feat/expected",
            "target_branch": "main",
            "allowed_paths": ["src/"]
        }]
    });
    apply_delivery(&ws, "T-SYNC", &draft, None).unwrap();
    let deliv_before = fs::read_to_string(ws.join("tasks/T-SYNC/delivery.yaml")).unwrap();

    let obs = sync_task(&ws, "T-SYNC", "../repos").unwrap();
    assert_eq!(obs["git_available"].as_bool(), Some(true));
    let item_obs = &obs["items"][0];
    assert_eq!(item_obs["actual_branch"].as_str(), Some("feat/work"));
    assert_eq!(item_obs["head_commit"].as_str(), Some(head.as_str()));
    assert_eq!(item_obs["branch_mismatch"].as_bool(), Some(true));
    assert_eq!(item_obs["dirty_file_count"].as_i64(), Some(2));
    assert!(
        item_obs["out_of_scope_paths"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p.as_str() == Some("docs/extra.txt"))
    );

    // sync never mutates task.yaml or delivery.yaml
    assert_eq!(
        task_before,
        fs::read_to_string(ws.join("tasks/T-SYNC/task.yaml")).unwrap()
    );
    assert_eq!(
        deliv_before,
        fs::read_to_string(ws.join("tasks/T-SYNC/delivery.yaml")).unwrap()
    );

    // No-Git graceful fallback via subprocess CLI
    let no_git_out = Command::new(wb_bin())
        .args(["--workspace", ws.to_str().unwrap(), "sync", "T-SYNC", "--json"])
        .env("WB_DISABLE_GIT", "1")
        .output()
        .unwrap();
    assert_eq!(no_git_out.status.code(), Some(0));
    let no_git_obs: Value = serde_json::from_slice(&no_git_out.stdout).unwrap();
    assert_eq!(no_git_obs["git_available"].as_bool(), Some(false));
    assert_eq!(
        no_git_obs["items"][0]["status"].as_str(),
        Some("git_unavailable")
    );
}

#[test]
fn m1_context_plain_line_budget_and_review_prepare() {
    let tmp = TempDir::new().unwrap();
    let ws = setup_ws(&tmp);
    create_task(
        &ws,
        CreateTaskOpts {
            task_id: "T-CTX",
            title: "Verbose handoff check",
            goal: "Ensure concise brief",
            status: "review",
            priority: "P0",
            ..Default::default()
        },
    )
    .unwrap();

    for i in 0..10 {
        manage_next_steps(&ws, "T-CTX", "add", &format!("step-{i}"), None).unwrap();
        manage_blockers(&ws, "T-CTX", "add", &format!("blocker-{i}"), None).unwrap();
    }

    let rev = prepare_review(
        &ws,
        "T-CTX",
        "https://example.local/cr/99",
        "analysis/cr-99.md",
    )
    .unwrap();
    let prompt = rev["prompt"].as_str().unwrap();
    for (dim, _) in REVIEW_DIMENSIONS {
        assert!(prompt.contains(dim), "missing review dimension {dim}");
    }
    assert!(ws.join("analysis/cr-99.md").is_file());
    assert!(ws.join("analysis/cr-99.summary.md").is_file());
    assert!(ws.join("analysis/cr-99.comments.md").is_file());

    let ctx = build_context_data(&ws, "T-CTX").unwrap();
    let plain = format_context_plain(&ctx, 80);
    let lines: Vec<&str> = plain.lines().collect();
    assert!(
        lines.len() <= 80,
        "plain context exceeded 80 lines: {}",
        lines.len()
    );
    assert!(plain.contains("T-CTX"));
    assert!(plain.contains("step-0"));
    assert!(plain.contains("step-2"));
    assert!(!plain.contains("step-3"));
}
