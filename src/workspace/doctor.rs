use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use crate::core::evidence::evaluate_actions_status;
use crate::core::io_utils::{ensure_within_workspace, read_json_file, read_yaml_file};
use crate::core::models::{validate_delivery_payload, validate_task_dict};
use crate::core::sync::load_observation;

use super::config::load_workspace_config;

pub fn run_doctor(ws_root: &Path) -> Value {
    let mut findings: Vec<Value> = Vec::new();
    let mut add_finding = |severity: &str, code: &str, message: &str, task_id: Option<&str>| {
        let mut m = Map::new();
        m.insert("severity".into(), Value::String(severity.to_string()));
        m.insert("code".into(), Value::String(code.to_string()));
        m.insert(
            "task_id".into(),
            task_id.map(|s| Value::String(s.to_string())).unwrap_or(Value::Null),
        );
        m.insert("message".into(), Value::String(message.to_string()));
        findings.push(Value::Object(m));
    };

    if let Err(e) = load_workspace_config(ws_root) {
        add_finding("error", "config_invalid", &e.message, None);
    }

    let st_path = ws_root.join("tasks").join("session-types.json");
    if !st_path.is_file() {
        add_finding("warning", "session_types_missing", "缺少 tasks/session-types.json", None);
    } else {
        match read_json_file(&st_path) {
            Ok(data) => {
                if !data.is_object() || !data.get("sessions").map(|v| v.is_array()).unwrap_or(false) {
                    add_finding(
                        "error",
                        "session_types_invalid",
                        "tasks/session-types.json 结构无效",
                        None,
                    );
                }
            }
            Err(e) => add_finding("error", "session_types_invalid", &e.message, None),
        }
    }

    let tasks_dir = ws_root.join("tasks");
    let mut checked_tasks: usize = 0;
    if tasks_dir.is_dir() {
        let mut dirs: Vec<PathBuf> = fs::read_dir(&tasks_dir)
            .ok()
            .into_iter()
            .flatten()
            .filter_map(|e| e.ok().map(|x| x.path()))
            .filter(|p| p.is_dir())
            .collect();
        dirs.sort();

        for child in dirs {
            let tid = child
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            let tfile = child.join("task.yaml");
            if !tfile.is_file() {
                add_finding(
                    "warning",
                    "task_yaml_missing",
                    &format!("任务目录缺少 task.yaml: {tid}"),
                    Some(&tid),
                );
                continue;
            }
            checked_tasks += 1;

            let task_data = match read_yaml_file(&tfile).and_then(|v| validate_task_dict(&v)) {
                Ok(data) => {
                    let data_id = data.get("id").and_then(|v| v.as_str()).unwrap_or("");
                    if data_id != tid {
                        add_finding(
                            "error",
                            "task_id_mismatch",
                            &format!("目录名 {tid} 与 task.yaml 中 id={data_id} 不一致"),
                            Some(&tid),
                        );
                    }
                    Some(data)
                }
                Err(e) => {
                    add_finding("error", "task_schema_invalid", &e.message, Some(&tid));
                    None
                }
            };

            if let Some(ref td) = task_data {
                for field_name in ["docs", "materials"] {
                    if let Some(arr) = td.get(field_name).and_then(|v| v.as_array()) {
                        for ref_val in arr {
                            let ref_str = ref_val.as_str().unwrap_or("");
                            match ensure_within_workspace(ws_root, Path::new(ref_str)) {
                                Ok(resolved) => {
                                    if !resolved.exists() {
                                        add_finding(
                                            "error",
                                            "broken_reference",
                                            &format!("{field_name} 引用路径不存在: {ref_str}"),
                                            Some(&tid),
                                        );
                                    }
                                }
                                Err(e) => {
                                    add_finding(
                                        "error",
                                        "reference_out_of_workspace",
                                        &format!(
                                            "{field_name} 路径越界或非法 ({ref_str}): {}",
                                            e.message
                                        ),
                                        Some(&tid),
                                    );
                                }
                            }
                        }
                    }
                }
            }

            let dfile = child.join("delivery.yaml");
            if dfile.is_file() {
                if let Err(e) = read_yaml_file(&dfile).and_then(|v| validate_delivery_payload(&v)) {
                    add_finding("error", "delivery_schema_invalid", &e.message, Some(&tid));
                }

                match evaluate_actions_status(ws_root, &tid) {
                    Ok(actions) => {
                        for act in actions {
                            let st = act.get("status").and_then(|v| v.as_str()).unwrap_or("");
                            if matches!(st, "stale" | "missing_receipt" | "invalid_receipt") {
                                let aid = act.get("id").and_then(|v| v.as_str()).unwrap_or("");
                                let reason = act.get("reason").and_then(|v| v.as_str()).unwrap_or("");
                                add_finding(
                                    "error",
                                    &format!("evidence_{st}"),
                                    &format!("动作 {aid} 确认无效: {reason}"),
                                    Some(&tid),
                                );
                            }
                        }
                    }
                    Err(e) => {
                        add_finding("error", "evidence_check_failed", &e.message, Some(&tid));
                    }
                }

                match load_observation(ws_root, &tid) {
                    Ok(Some(obs)) => {
                        if let Some(items) = obs.get("items").and_then(|v| v.as_array()) {
                            for item_obs in items {
                                let iid = item_obs.get("id").and_then(|v| v.as_str()).unwrap_or("");
                                if item_obs
                                    .get("branch_mismatch")
                                    .and_then(|v| v.as_bool())
                                    .unwrap_or(false)
                                {
                                    let decl = item_obs
                                        .get("declared_source_branch")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("");
                                    let actual = item_obs
                                        .get("actual_branch")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("");
                                    add_finding(
                                        "error",
                                        "branch_mismatch",
                                        &format!("交付项 {iid} 分支不一致: 声明={decl} vs 实际={actual}"),
                                        Some(&tid),
                                    );
                                }
                                if let Some(oos) =
                                    item_obs.get("out_of_scope_paths").and_then(|v| v.as_array())
                                {
                                    if !oos.is_empty() {
                                        let joined: Vec<&str> =
                                            oos.iter().filter_map(|v| v.as_str()).collect();
                                        add_finding(
                                            "error",
                                            "out_of_scope_paths",
                                            &format!(
                                                "交付项 {iid} 存在允许范围外改动: {}",
                                                joined.join(", ")
                                            ),
                                            Some(&tid),
                                        );
                                    }
                                }
                            }
                        }
                    }
                    Ok(None) => {}
                    Err(e) => add_finding("error", "observation_invalid", &e.message, Some(&tid)),
                }
            }
        }
    }

    let error_count = findings
        .iter()
        .filter(|f| f.get("severity").and_then(|v| v.as_str()) == Some("error"))
        .count();
    let warning_count = findings
        .iter()
        .filter(|f| f.get("severity").and_then(|v| v.as_str()) == Some("warning"))
        .count();

    let mut out = Map::new();
    out.insert(
        "workspace".into(),
        Value::String(ws_root.to_string_lossy().into_owned()),
    );
    out.insert("healthy".into(), Value::Bool(error_count == 0));
    out.insert("checked_tasks".into(), Value::from(checked_tasks as u64));
    out.insert("error_count".into(), Value::from(error_count as u64));
    out.insert("warning_count".into(), Value::from(warning_count as u64));
    out.insert("findings".into(), Value::Array(findings));
    Value::Object(out)
}
