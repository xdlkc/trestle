use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use chrono::{DateTime, Utc};
use serde_json::{Map, Value};

use crate::core::delivery::load_delivery;
use crate::core::errors::WbError;
use crate::core::evidence::evaluate_actions_status;
use crate::core::io_utils::{
    atomic_write_bytes, now_iso, read_json_file, read_yaml_file, to_workspace_relpath,
};
use crate::core::models::{ACTIVE_STATUSES, validate_task_dict};
use crate::core::sync::load_observation;
use crate::workspace::config::load_workspace_config;

use super::redaction::{redact_text, redact_value};

pub const MAX_SINGLE_FILE_BYTES: u64 = 2 * 1024 * 1024; // 2 MiB
pub const MAX_TASK_BYTES: u64 = 10 * 1024 * 1024; // 10 MiB
pub const MAX_WORKSPACE_SCAN_BYTES: u64 = 100 * 1024 * 1024; // 100 MiB
pub const MAX_SNAPSHOT_BYTES: usize = 25 * 1024 * 1024; // 25 MiB

#[derive(Clone, Debug, PartialEq, Eq)]
struct FileSig {
    exists: bool,
    len: u64,
    modified: Option<SystemTime>,
}

fn file_sig(path: &Path) -> FileSig {
    match fs::metadata(path) {
        Ok(m) => FileSig {
            exists: true,
            len: m.len(),
            modified: m.modified().ok(),
        },
        Err(_) => FileSig {
            exists: false,
            len: 0,
            modified: None,
        },
    }
}

fn collect_watched_sigs(ws_root: &Path) -> BTreeMap<PathBuf, FileSig> {
    let mut map = BTreeMap::new();
    let cfg = ws_root.join(".wb.yml");
    map.insert(cfg.clone(), file_sig(&cfg));
    let t_root = ws_root.join("tasks");
    let ledger = t_root.join("session-types.json");
    map.insert(ledger.clone(), file_sig(&ledger));

    if let Ok(entries) = fs::read_dir(&t_root) {
        for entry in entries.flatten() {
            let p = entry.path();
            if !p.is_dir() {
                continue;
            }
            let tid = p
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            for rel in ["task.yaml", "progress.md", "delivery.yaml"] {
                let fp = p.join(rel);
                map.insert(fp.clone(), file_sig(&fp));
            }
            let obs = ws_root
                .join(".wb-runtime")
                .join("observations")
                .join(format!("{tid}.json"));
            map.insert(obs.clone(), file_sig(&obs));
        }
    }
    map
}

fn make_issue(code: &str, task_id: Option<&str>, file: &str, message: &str) -> Value {
    let mut m = Map::new();
    m.insert("code".into(), Value::String(code.to_string()));
    m.insert(
        "task_id".into(),
        task_id
            .map(|s| Value::String(s.to_string()))
            .unwrap_or(Value::Null),
    );
    m.insert("file".into(), Value::String(file.to_string()));
    m.insert("message".into(), Value::String(redact_text(message)));
    Value::Object(m)
}

fn check_file_size(
    ws_root: &Path,
    path: &Path,
    task_id: Option<&str>,
    task_bytes: &mut u64,
    total_bytes: &mut u64,
    issues: &mut Vec<Value>,
) -> Option<u64> {
    let meta = fs::metadata(path).ok()?;
    let size = meta.len();
    let rel = to_workspace_relpath(ws_root, path).unwrap_or_else(|_| path.display().to_string());

    if size > MAX_SINGLE_FILE_BYTES {
        issues.push(make_issue(
            "FILE_TOO_LARGE",
            task_id,
            &rel,
            &format!(
                "文件超过单文件 2MiB 上限 ({size} > {MAX_SINGLE_FILE_BYTES})，已跳过"
            ),
        ));
        return None;
    }
    if *task_bytes + size > MAX_TASK_BYTES {
        issues.push(make_issue(
            "TASK_SIZE_LIMIT",
            task_id,
            &rel,
            &format!("任务累计读取体积超过 10MiB 上限，已跳过后续文件"),
        ));
        return None;
    }
    if *total_bytes + size > MAX_WORKSPACE_SCAN_BYTES {
        issues.push(make_issue(
            "WORKSPACE_SCAN_LIMIT",
            task_id,
            &rel,
            "工作台扫描累计体积超过 100MiB 上限，已停止扫描",
        ));
        return None;
    }
    *task_bytes += size;
    *total_bytes += size;
    Some(size)
}

fn compute_stale_days(updated_at: &str) -> i64 {
    if let Ok(dt) = DateTime::parse_from_rfc3339(updated_at) {
        let diff = Utc::now().signed_duration_since(dt.with_timezone(&Utc));
        return diff.num_days().max(0);
    }
    0
}

fn parse_progress_entries(
    raw: &str,
    task_id: &str,
    task_title: &str,
) -> Vec<Value> {
    let mut entries = Vec::new();
    let mut current_ts: Option<String> = None;
    let mut current_status = String::new();
    let mut body_lines: Vec<String> = Vec::new();

    let flush = |ts: &Option<String>,
                 st: &str,
                 lines: &mut Vec<String>,
                 out: &mut Vec<Value>| {
        if let Some(timestamp) = ts {
            let summary = lines
                .iter()
                .map(|l| l.trim().trim_start_matches('-').trim())
                .filter(|l| !l.is_empty())
                .collect::<Vec<_>>()
                .join(" ");
            let date = timestamp.get(0..10).unwrap_or("").to_string();
            let mut item = Map::new();
            item.insert("date".into(), Value::String(date));
            item.insert("timestamp".into(), Value::String(timestamp.clone()));
            item.insert("task_id".into(), Value::String(task_id.to_string()));
            item.insert("task_title".into(), Value::String(task_title.to_string()));
            item.insert("status".into(), Value::String(st.to_string()));
            item.insert("summary".into(), Value::String(summary));
            out.push(Value::Object(item));
            lines.clear();
        }
    };

    for line in raw.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("## ") {
            flush(&current_ts, &current_status, &mut body_lines, &mut entries);
            if let (Some(lb), Some(rb)) = (rest.rfind('['), rest.rfind(']')) {
                if rb > lb {
                    current_ts = Some(rest[..lb].trim().to_string());
                    current_status = rest[lb + 1..rb].trim().to_string();
                    continue;
                }
            }
            current_ts = Some(rest.trim().to_string());
            current_status.clear();
        } else if current_ts.is_some() && !trimmed.is_empty() {
            body_lines.push(trimmed.to_string());
        }
    }
    flush(&current_ts, &current_status, &mut body_lines, &mut entries);
    entries
}

pub fn load_existing_snapshot(ws_root: &Path) -> Option<Value> {
    let p = ws_root.join(".wb-runtime").join("snapshot.json");
    if !p.is_file() {
        return None;
    }
    read_json_file(&p).ok()
}

pub fn collect_snapshot(ws_root: &Path) -> Result<Value, WbError> {
    collect_snapshot_with_hook(ws_root, None::<fn()>)
}

pub fn collect_snapshot_with_hook<F>(
    ws_root: &Path,
    mut mid_hook: Option<F>,
) -> Result<Value, WbError>
where
    F: FnMut(),
{
    let pre_sigs = collect_watched_sigs(ws_root);
    let mut issues: Vec<Value> = Vec::new();
    let mut total_bytes: u64 = 0;

    let ws_cfg = match load_workspace_config(ws_root) {
        Ok(c) => c,
        Err(e) => {
            issues.push(make_issue(
                "WORKSPACE_CONFIG_ERROR",
                None,
                ".wb.yml",
                &e.message,
            ));
            Value::Object(Map::new())
        }
    };

    let mut tasks_out: Vec<Value> = Vec::new();
    let mut repos_out: Vec<Value> = Vec::new();
    let mut timeline_out: Vec<Value> = Vec::new();
    let mut by_category: BTreeMap<String, i64> = BTreeMap::new();
    let mut by_status: BTreeMap<String, i64> = BTreeMap::new();
    let mut active_count: i64 = 0;
    let mut blocked_count: i64 = 0;
    let mut review_count: i64 = 0;
    let mut stale_7d_count: i64 = 0;

    let t_root = ws_root.join("tasks");
    if t_root.is_dir() {
        let mut dirs: Vec<PathBuf> = fs::read_dir(&t_root)
            .map_err(|e| WbError::io(format!("读取 tasks 目录失败: {e}")))?
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect();
        dirs.sort();

        for tdir in dirs {
            if total_bytes >= MAX_WORKSPACE_SCAN_BYTES {
                break;
            }
            let tid = tdir
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            let tfile = tdir.join("task.yaml");
            if !tfile.is_file() {
                continue;
            }

            let mut task_bytes: u64 = 0;
            if check_file_size(
                ws_root,
                &tfile,
                Some(&tid),
                &mut task_bytes,
                &mut total_bytes,
                &mut issues,
            )
            .is_none()
            {
                continue;
            }

            let mut task_val = match read_yaml_file(&tfile).and_then(|v| validate_task_dict(&v)) {
                Ok(v) => v,
                Err(e) => {
                    let rel = to_workspace_relpath(ws_root, &tfile)
                        .unwrap_or_else(|_| format!("tasks/{tid}/task.yaml"));
                    issues.push(make_issue("TASK_INVALID", Some(&tid), &rel, &e.message));
                    continue;
                }
            };

            let task_id = task_val
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or(&tid)
                .to_string();
            let task_title = task_val
                .get("title")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let task_status = task_val
                .get("status")
                .and_then(|v| v.as_str())
                .unwrap_or("planning")
                .to_string();
            let task_cat = task_val
                .get("category")
                .and_then(|v| v.as_str())
                .unwrap_or("unclassified")
                .to_string();
            let updated_at = task_val
                .get("updated_at")
                .and_then(|v| v.as_str())
                .unwrap_or("");

            *by_status.entry(task_status.clone()).or_insert(0) += 1;
            *by_category.entry(task_cat).or_insert(0) += 1;

            let is_active = ACTIVE_STATUSES.contains(&task_status.as_str());
            let stale_days = compute_stale_days(updated_at);
            let is_stale_7d = is_active && stale_days >= 7;

            if is_active {
                active_count += 1;
            }
            let blockers_non_empty = task_val
                .get("blockers")
                .and_then(|v| v.as_array())
                .map(|a| !a.is_empty())
                .unwrap_or(false);
            if task_status == "blocked" || blockers_non_empty {
                blocked_count += 1;
            }
            if task_status == "review" {
                review_count += 1;
            }
            if is_stale_7d {
                stale_7d_count += 1;
            }

            let actions_status = match evaluate_actions_status(ws_root, &task_id) {
                Ok(a) => a,
                Err(e) => {
                    issues.push(make_issue(
                        "DELIVERY_OR_EVIDENCE_INVALID",
                        Some(&task_id),
                        &format!("tasks/{task_id}/delivery.yaml"),
                        &e.message,
                    ));
                    Vec::new()
                }
            };

            let deliv_file = tdir.join("delivery.yaml");
            let deliv = if deliv_file.is_file() {
                if check_file_size(
                    ws_root,
                    &deliv_file,
                    Some(&task_id),
                    &mut task_bytes,
                    &mut total_bytes,
                    &mut issues,
                )
                .is_some()
                {
                    match load_delivery(ws_root, &task_id, false) {
                        Ok(d) => d,
                        Err(e) => {
                            issues.push(make_issue(
                                "DELIVERY_INVALID",
                                Some(&task_id),
                                &format!("tasks/{task_id}/delivery.yaml"),
                                &e.message,
                            ));
                            None
                        }
                    }
                } else {
                    None
                }
            } else {
                None
            };

            let obs_file = ws_root
                .join(".wb-runtime")
                .join("observations")
                .join(format!("{task_id}.json"));
            let obs = if obs_file.is_file() {
                if check_file_size(
                    ws_root,
                    &obs_file,
                    Some(&task_id),
                    &mut task_bytes,
                    &mut total_bytes,
                    &mut issues,
                )
                .is_some()
                {
                    match load_observation(ws_root, &task_id) {
                        Ok(o) => o,
                        Err(e) => {
                            issues.push(make_issue(
                                "OBSERVATION_INVALID",
                                Some(&task_id),
                                &format!(".wb-runtime/observations/{task_id}.json"),
                                &e.message,
                            ));
                            None
                        }
                    }
                } else {
                    None
                }
            } else {
                None
            };

            if let Some(ref d) = deliv {
                if let Some(items) = d.get("items").and_then(|v| v.as_array()) {
                    let obs_items = obs
                        .as_ref()
                        .and_then(|o| o.get("items"))
                        .and_then(|v| v.as_array());
                    let task_links = task_val
                        .get("links")
                        .cloned()
                        .unwrap_or_else(|| Value::Array(Vec::new()));
                    for item in items {
                        let iid = item.get("id").and_then(|v| v.as_str()).unwrap_or("");
                        let matched_obs = obs_items.and_then(|arr| {
                            arr.iter()
                                .find(|o| o.get("id").and_then(|v| v.as_str()) == Some(iid))
                        });
                        let mut r = Map::new();
                        r.insert("task_id".into(), Value::String(task_id.clone()));
                        r.insert("task_status".into(), Value::String(task_status.clone()));
                        r.insert("item_id".into(), Value::String(iid.to_string()));
                        r.insert("repo".into(), item.get("repo").cloned().unwrap_or(Value::Null));
                        r.insert(
                            "worktree".into(),
                            item.get("worktree").cloned().unwrap_or(Value::Null),
                        );
                        r.insert(
                            "source_branch".into(),
                            item.get("source_branch").cloned().unwrap_or(Value::Null),
                        );
                        r.insert(
                            "target_branch".into(),
                            item.get("target_branch").cloned().unwrap_or(Value::Null),
                        );
                        r.insert(
                            "compare_base".into(),
                            item.get("compare_base").cloned().unwrap_or(Value::Null),
                        );
                        r.insert(
                            "actual_branch".into(),
                            matched_obs
                                .and_then(|o| o.get("actual_branch").cloned())
                                .unwrap_or(Value::Null),
                        );
                        r.insert(
                            "head_commit".into(),
                            matched_obs
                                .and_then(|o| o.get("head_commit").cloned())
                                .unwrap_or(Value::Null),
                        );
                        r.insert(
                            "dirty_file_count".into(),
                            matched_obs
                                .and_then(|o| o.get("dirty_file_count").cloned())
                                .unwrap_or_else(|| Value::from(0)),
                        );
                        r.insert(
                            "branch_mismatch".into(),
                            matched_obs
                                .and_then(|o| o.get("branch_mismatch").cloned())
                                .unwrap_or(Value::Bool(false)),
                        );
                        r.insert(
                            "out_of_scope_paths".into(),
                            matched_obs
                                .and_then(|o| o.get("out_of_scope_paths").cloned())
                                .unwrap_or_else(|| Value::Array(Vec::new())),
                        );
                        r.insert("links".into(), task_links.clone());
                        repos_out.push(Value::Object(r));
                    }
                }
            }

            let prog_file = tdir.join("progress.md");
            if prog_file.is_file()
                && check_file_size(
                    ws_root,
                    &prog_file,
                    Some(&task_id),
                    &mut task_bytes,
                    &mut total_bytes,
                    &mut issues,
                )
                .is_some()
            {
                if let Ok(raw_prog) = fs::read_to_string(&prog_file) {
                    let mut parsed = parse_progress_entries(&raw_prog, &task_id, &task_title);
                    timeline_out.append(&mut parsed);
                }
            }

            if let Some(obj) = task_val.as_object_mut() {
                obj.insert("stale_days".into(), Value::from(stale_days));
                obj.insert("is_stale_7d".into(), Value::Bool(is_stale_7d));
                obj.insert("actions_status".into(), Value::Array(actions_status));
            }
            tasks_out.push(task_val);
        }
    }

    let sessions_out = {
        let ledger_path = t_root.join("session-types.json");
        if ledger_path.is_file() {
            let mut dummy_task_bytes = 0;
            if check_file_size(
                ws_root,
                &ledger_path,
                None,
                &mut dummy_task_bytes,
                &mut total_bytes,
                &mut issues,
            )
            .is_some()
            {
                match read_json_file(&ledger_path) {
                    Ok(v) => v
                        .get("sessions")
                        .and_then(|s| s.as_array())
                        .cloned()
                        .unwrap_or_default(),
                    Err(e) => {
                        issues.push(make_issue(
                            "SESSION_LEDGER_INVALID",
                            None,
                            "tasks/session-types.json",
                            &e.message,
                        ));
                        Vec::new()
                    }
                }
            } else {
                Vec::new()
            }
        } else {
            Vec::new()
        }
    };

    if let Some(ref mut hook) = mid_hook {
        hook();
    }
    if let Ok(ms_str) = std::env::var("WB_COLLECTOR_DELAY_MS") {
        if let Ok(ms) = ms_str.parse::<u64>() {
            if ms > 0 {
                std::thread::sleep(Duration::from_millis(ms));
            }
        }
    }

    let post_sigs = collect_watched_sigs(ws_root);
    if pre_sigs != post_sigs {
        let mut res = Map::new();
        res.insert("updated".into(), Value::Bool(false));
        res.insert(
            "aborted_reason".into(),
            Value::String("采集中途工作台文件发生变更，已保留旧快照".into()),
        );
        res.insert(
            "snapshot".into(),
            load_existing_snapshot(ws_root).unwrap_or(Value::Null),
        );
        return Ok(Value::Object(res));
    }

    timeline_out.sort_by(|a, b| {
        let ta = a.get("timestamp").and_then(|v| v.as_str()).unwrap_or("");
        let tb = b.get("timestamp").and_then(|v| v.as_str()).unwrap_or("");
        tb.cmp(ta)
    });

    let mut ov = Map::new();
    ov.insert("total".into(), Value::from(tasks_out.len() as i64));
    ov.insert("active".into(), Value::from(active_count));
    ov.insert("blocked".into(), Value::from(blocked_count));
    ov.insert("review".into(), Value::from(review_count));
    ov.insert("stale_7d".into(), Value::from(stale_7d_count));
    ov.insert(
        "by_category".into(),
        Value::Object(
            by_category
                .into_iter()
                .map(|(k, v)| (k, Value::from(v)))
                .collect(),
        ),
    );
    ov.insert(
        "by_status".into(),
        Value::Object(
            by_status
                .into_iter()
                .map(|(k, v)| (k, Value::from(v)))
                .collect(),
        ),
    );

    let generated_at = now_iso();
    let mut snap_map = Map::new();
    snap_map.insert("schema".into(), Value::from(1));
    snap_map.insert("generated_at".into(), Value::String(generated_at.clone()));
    snap_map.insert("workspace".into(), ws_cfg);
    snap_map.insert("overview".into(), Value::Object(ov));
    snap_map.insert("tasks".into(), Value::Array(tasks_out));
    snap_map.insert("sessions".into(), Value::Array(sessions_out));
    snap_map.insert("repos".into(), Value::Array(repos_out));
    snap_map.insert("timeline".into(), Value::Array(timeline_out));
    snap_map.insert("data_issues".into(), Value::Array(issues));

    let mut redacted_snap = redact_value(&Value::Object(snap_map));
    let mut bytes = serde_json::to_vec_pretty(&redacted_snap)
        .map_err(|e| WbError::io(format!("序列化快照失败: {e}")))?;

    if bytes.len() > MAX_SNAPSHOT_BYTES {
        if let Some(obj) = redacted_snap.as_object_mut() {
            if let Some(tl) = obj.get_mut("timeline").and_then(|v| v.as_array_mut()) {
                tl.truncate(200);
            }
            if let Some(iss) = obj.get_mut("data_issues").and_then(|v| v.as_array_mut()) {
                iss.push(make_issue(
                    "SNAPSHOT_TRIMMED",
                    None,
                    ".wb-runtime/snapshot.json",
                    "快照体积接近 25MiB 上限，已截断部分历史时间线",
                ));
            }
        }
        bytes = serde_json::to_vec_pretty(&redacted_snap)
            .map_err(|e| WbError::io(format!("序列化快照失败: {e}")))?;
        if bytes.len() > MAX_SNAPSHOT_BYTES {
            let mut res = Map::new();
            res.insert("updated".into(), Value::Bool(false));
            res.insert(
                "aborted_reason".into(),
                Value::String("快照体积超过 25MiB 上限，已保留旧快照".into()),
            );
            res.insert(
                "snapshot".into(),
                load_existing_snapshot(ws_root).unwrap_or(Value::Null),
            );
            return Ok(Value::Object(res));
        }
    }

    let snap_path = ws_root.join(".wb-runtime").join("snapshot.json");
    atomic_write_bytes(&snap_path, &bytes)?;

    let mut res = Map::new();
    res.insert("updated".into(), Value::Bool(true));
    res.insert("aborted_reason".into(), Value::Null);
    res.insert("generated_at".into(), Value::String(generated_at));
    res.insert("snapshot".into(), redacted_snap);
    Ok(Value::Object(res))
}
