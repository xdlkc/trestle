use std::fs;
use std::process::Command;

use serde_json::{Value, json};
use tempfile::TempDir;

fn wb_bin() -> &'static str {
    env!("CARGO_BIN_EXE_wb")
}

#[test]
fn m4_extensions_disabled_probe_isolation_and_standalone_cli() {
    let tmp = TempDir::new().unwrap();
    let ws = tmp.path().join("ws-ext");
    let ext_dir = tmp.path().join("ext-manifests");
    fs::create_dir_all(&ext_dir).unwrap();

    // Initialize clean workspace via binary
    assert_eq!(
        Command::new(wb_bin())
            .args(["init", ws.to_str().unwrap(), "--yes"])
            .status()
            .unwrap()
            .code(),
        Some(0)
    );

    let marker = tmp.path().join("disabled_ext_executed.marker");

    // 1. Disabled extension with a side-effect probe_cmd that must never execute
    fs::write(
        ext_dir.join("01-disabled.json"),
        serde_json::to_string_pretty(&json!({
            "name": "ext-disabled",
            "version": "1.0.0",
            "description": "Should never execute when disabled",
            "probe_cmd": ["sh", "-c", format!("touch '{}'", marker.display())]
        }))
        .unwrap(),
    )
    .unwrap();

    // 2. Enabled extension missing a required binary
    fs::write(
        ext_dir.join("02-missing-dep.json"),
        serde_json::to_string_pretty(&json!({
            "name": "ext-missing-dep",
            "version": "1.0.0",
            "requires_bins": ["__wb_definitely_absent_binary_xyz__"]
        }))
        .unwrap(),
    )
    .unwrap();

    // 3. Enabled extension whose probe exits non-zero (must be isolated)
    fs::write(
        ext_dir.join("03-failing-probe.json"),
        serde_json::to_string_pretty(&json!({
            "name": "ext-failing-probe",
            "version": "1.0.0",
            "probe_cmd": ["sh", "-c", "echo probe-err >&2; exit 2"]
        }))
        .unwrap(),
    )
    .unwrap();

    // 4. Malformed manifest JSON (must be isolated)
    fs::write(ext_dir.join("04-broken.json"), "{not-valid-json").unwrap();

    // 5. Healthy enabled extension
    fs::write(
        ext_dir.join("05-healthy.json"),
        serde_json::to_string_pretty(&json!({
            "name": "ext-healthy",
            "version": "1.2.3",
            "requires_paths": [".wb.yml"],
            "probe_cmd": ["sh", "-c", "exit 0"]
        }))
        .unwrap(),
    )
    .unwrap();

    // Enable selected extensions explicitly in .wb.yml (leaving ext-disabled off)
    let cfg_path = ws.join(".wb.yml");
    let cfg_text = fs::read_to_string(&cfg_path).unwrap();
    let updated_cfg = cfg_text.replace(
        "extensions: []",
        "extensions:\n  - ext-missing-dep\n  - ext-failing-probe\n  - ext-healthy",
    );
    fs::write(&cfg_path, updated_cfg).unwrap();

    let out = Command::new(wb_bin())
        .args(["--workspace", ws.to_str().unwrap(), "extensions", "list", "--json"])
        .env("WB_EXTENSION_MANIFESTS", ext_dir.to_str().unwrap())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));

    assert!(
        !marker.exists(),
        "disabled extension must never be loaded or probed"
    );

    let payload: Value = serde_json::from_slice(&out.stdout).unwrap();
    let items = payload["extensions"].as_array().unwrap();

    let d = items.iter().find(|e| e["name"] == "ext-disabled").unwrap();
    assert_eq!(d["enabled"].as_bool(), Some(false));
    assert_eq!(d["loaded"].as_bool(), Some(false));
    assert_eq!(d["status"].as_str(), Some("disabled"));

    let m = items.iter().find(|e| e["name"] == "ext-missing-dep").unwrap();
    assert_eq!(m["enabled"].as_bool(), Some(true));
    assert_eq!(m["loaded"].as_bool(), Some(false));
    assert_eq!(m["status"].as_str(), Some("unavailable"));

    let f = items
        .iter()
        .find(|e| e["name"] == "ext-failing-probe")
        .unwrap();
    assert_eq!(f["enabled"].as_bool(), Some(true));
    assert_eq!(f["loaded"].as_bool(), Some(false));
    assert_eq!(f["status"].as_str(), Some("error"));

    let b = items.iter().find(|e| e["name"] == "04-broken").unwrap();
    assert_eq!(b["status"].as_str(), Some("error"));

    let h = items.iter().find(|e| e["name"] == "ext-healthy").unwrap();
    assert_eq!(h["enabled"].as_bool(), Some(true));
    assert_eq!(h["loaded"].as_bool(), Some(true));
    assert_eq!(h["status"].as_str(), Some("loaded"));

    // Core commands remain completely healthy despite broken/failing extensions
    let doc_out = Command::new(wb_bin())
        .args(["--workspace", ws.to_str().unwrap(), "doctor", "--json"])
        .env("WB_EXTENSION_MANIFESTS", ext_dir.to_str().unwrap())
        .output()
        .unwrap();
    assert_eq!(doc_out.status.code(), Some(0));
}
