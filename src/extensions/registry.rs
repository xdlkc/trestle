use std::collections::{BTreeSet, HashSet};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Map, Value, json};

use crate::core::errors::WbError;
use crate::core::io_utils::read_json_file;
use crate::workspace::config::load_workspace_config;

fn find_bin_in_path(bin_name: &str) -> bool {
    if bin_name.is_empty() || bin_name.contains('/') {
        let p = Path::new(bin_name);
        return is_executable_file(p);
    }
    let Some(path_os) = std::env::var_os("PATH") else {
        return false;
    };
    for dir in std::env::split_paths(&path_os) {
        let candidate = dir.join(bin_name);
        if is_executable_file(&candidate) {
            return true;
        }
    }
    false
}

fn is_executable_file(path: &Path) -> bool {
    let Ok(meta) = fs::metadata(path) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    meta.permissions().mode() & 0o111 != 0
}

fn collect_manifest_dirs(ws_root: &Path) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(env_val) = std::env::var_os("WB_EXTENSION_MANIFESTS") {
        for p in std::env::split_paths(&env_val) {
            if !p.as_os_str().is_empty() {
                dirs.push(p);
            }
        }
    }
    dirs.push(ws_root.join("extensions"));
    dirs
}

fn evaluate_manifest(
    ws_root: &Path,
    manifest_path: &Path,
    enabled_set: &BTreeSet<String>,
    seen_names: &mut HashSet<String>,
) -> Option<Value> {
    let fallback_name = manifest_path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "unknown".to_string());

    let raw = match read_json_file(manifest_path) {
        Ok(v) => v,
        Err(e) => {
            return Some(json!({
                "name": fallback_name,
                "version": null,
                "description": "",
                "enabled": enabled_set.contains(&fallback_name),
                "loaded": false,
                "status": "error",
                "capabilities": [],
                "manifest_path": manifest_path.to_string_lossy(),
                "error": format!("扩展清单解析失败: {}", e.message),
            }));
        }
    };

    let Some(obj) = raw.as_object() else {
        return Some(json!({
            "name": fallback_name,
            "version": null,
            "description": "",
            "enabled": enabled_set.contains(&fallback_name),
            "loaded": false,
            "status": "error",
            "capabilities": [],
            "manifest_path": manifest_path.to_string_lossy(),
            "error": "扩展清单顶层必须为 JSON 对象",
        }));
    };

    let name = match obj.get("name").and_then(|v| v.as_str()).map(str::trim) {
        Some(n) if !n.is_empty() => n.to_string(),
        _ => {
            return Some(json!({
                "name": fallback_name,
                "version": null,
                "description": "",
                "enabled": false,
                "loaded": false,
                "status": "error",
                "capabilities": [],
                "manifest_path": manifest_path.to_string_lossy(),
                "error": "扩展清单缺少有效 name 字段",
            }));
        }
    };

    if !seen_names.insert(name.clone()) {
        return None;
    }

    let version = obj
        .get("version")
        .and_then(|v| v.as_str())
        .map(|s| Value::String(s.to_string()))
        .unwrap_or(Value::Null);
    let description = obj
        .get("description")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let capabilities = obj
        .get("capabilities")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    let enabled = enabled_set.contains(&name);
    if !enabled {
        return Some(json!({
            "name": name,
            "version": version,
            "description": description,
            "enabled": false,
            "loaded": false,
            "status": "disabled",
            "capabilities": capabilities,
            "manifest_path": manifest_path.to_string_lossy(),
            "error": null,
        }));
    }

    let mut missing: Vec<String> = Vec::new();
    if let Some(bins) = obj.get("requires_bins") {
        let Some(arr) = bins.as_array() else {
            return Some(json!({
                "name": name,
                "version": version,
                "description": description,
                "enabled": true,
                "loaded": false,
                "status": "error",
                "capabilities": capabilities,
                "manifest_path": manifest_path.to_string_lossy(),
                "error": "requires_bins 必须为字符串数组",
            }));
        };
        for item in arr {
            if let Some(b) = item.as_str().map(str::trim) {
                if !b.is_empty() && !find_bin_in_path(b) {
                    missing.push(format!("bin:{b}"));
                }
            }
        }
    }

    if let Some(paths) = obj.get("requires_paths") {
        let Some(arr) = paths.as_array() else {
            return Some(json!({
                "name": name,
                "version": version,
                "description": description,
                "enabled": true,
                "loaded": false,
                "status": "error",
                "capabilities": capabilities,
                "manifest_path": manifest_path.to_string_lossy(),
                "error": "requires_paths 必须为字符串数组",
            }));
        };
        for item in arr {
            if let Some(raw_p) = item.as_str().map(str::trim) {
                if !raw_p.is_empty() {
                    let p = Path::new(raw_p);
                    let resolved = if p.is_absolute() {
                        p.to_path_buf()
                    } else {
                        ws_root.join(p)
                    };
                    if !resolved.exists() {
                        missing.push(format!("path:{raw_p}"));
                    }
                }
            }
        }
    }

    if !missing.is_empty() {
        return Some(json!({
            "name": name,
            "version": version,
            "description": description,
            "enabled": true,
            "loaded": false,
            "status": "unavailable",
            "capabilities": capabilities,
            "manifest_path": manifest_path.to_string_lossy(),
            "error": format!("缺少本地依赖: {}", missing.join(", ")),
        }));
    }

    if let Some(probe_val) = obj.get("probe_cmd") {
        if !probe_val.is_null() {
            let Some(arr) = probe_val.as_array() else {
                return Some(json!({
                    "name": name,
                    "version": version,
                    "description": description,
                    "enabled": true,
                    "loaded": false,
                    "status": "error",
                    "capabilities": capabilities,
                    "manifest_path": manifest_path.to_string_lossy(),
                    "error": "probe_cmd 必须为非空字符串数组",
                }));
            };
            let parts: Vec<String> = arr
                .iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect();
            if parts.is_empty() || parts.len() != arr.len() {
                return Some(json!({
                    "name": name,
                    "version": version,
                    "description": description,
                    "enabled": true,
                    "loaded": false,
                    "status": "error",
                    "capabilities": capabilities,
                    "manifest_path": manifest_path.to_string_lossy(),
                    "error": "probe_cmd 包含非字符串或为空",
                }));
            }
            match Command::new(&parts[0])
                .args(&parts[1..])
                .current_dir(ws_root)
                .output()
            {
                Ok(out) if out.status.success() => {}
                Ok(out) => {
                    let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
                    let detail = if stderr.is_empty() {
                        format!("探针命令退出码 {}", out.status.code().unwrap_or(1))
                    } else {
                        stderr
                    };
                    return Some(json!({
                        "name": name,
                        "version": version,
                        "description": description,
                        "enabled": true,
                        "loaded": false,
                        "status": "error",
                        "capabilities": capabilities,
                        "manifest_path": manifest_path.to_string_lossy(),
                        "error": format!("扩展探针执行失败（已隔离）: {detail}"),
                    }));
                }
                Err(e) => {
                    return Some(json!({
                        "name": name,
                        "version": version,
                        "description": description,
                        "enabled": true,
                        "loaded": false,
                        "status": "error",
                        "capabilities": capabilities,
                        "manifest_path": manifest_path.to_string_lossy(),
                        "error": format!("扩展探针启动异常（已隔离）: {e}"),
                    }));
                }
            }
        }
    }

    Some(json!({
        "name": name,
        "version": version,
        "description": description,
        "enabled": true,
        "loaded": true,
        "status": "loaded",
        "capabilities": capabilities,
        "manifest_path": manifest_path.to_string_lossy(),
        "error": null,
    }))
}

pub fn list_extensions(ws_root: &Path) -> Result<Value, WbError> {
    let cfg = load_workspace_config(ws_root)?;
    let mut enabled_set: BTreeSet<String> = BTreeSet::new();
    if let Some(arr) = cfg.get("extensions").and_then(|v| v.as_array()) {
        for item in arr {
            if let Some(s) = item.as_str().map(str::trim) {
                if !s.is_empty() {
                    enabled_set.insert(s.to_string());
                }
            }
        }
    }

    let mut manifest_files: Vec<PathBuf> = Vec::new();
    for dir in collect_manifest_dirs(ws_root) {
        if !dir.is_dir() {
            continue;
        }
        if let Ok(entries) = fs::read_dir(&dir) {
            let mut local: Vec<PathBuf> = entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| {
                    p.is_file()
                        && p.extension()
                            .and_then(|ext| ext.to_str())
                            .map(|ext| ext.eq_ignore_ascii_case("json"))
                            .unwrap_or(false)
                })
                .collect();
            local.sort();
            manifest_files.append(&mut local);
        }
    }

    let mut seen_names: HashSet<String> = HashSet::new();
    let mut results: Vec<Value> = Vec::new();
    for mpath in manifest_files {
        if let Some(entry) = evaluate_manifest(ws_root, &mpath, &enabled_set, &mut seen_names) {
            results.push(entry);
        }
    }

    for enabled_name in &enabled_set {
        if !seen_names.contains(enabled_name) {
            results.push(json!({
                "name": enabled_name,
                "version": null,
                "description": "",
                "enabled": true,
                "loaded": false,
                "status": "missing",
                "capabilities": [],
                "manifest_path": null,
                "error": "未找到已启用扩展的清单文件",
            }));
        }
    }

    let mut out = Map::new();
    out.insert(
        "enabled".into(),
        Value::Array(enabled_set.into_iter().map(Value::String).collect()),
    );
    out.insert("extensions".into(), Value::Array(results));
    Ok(Value::Object(out))
}
