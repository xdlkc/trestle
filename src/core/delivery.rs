use std::collections::HashMap;
use std::fs;
use std::path::Path;

use serde_json::Value;

use super::errors::WbError;
use super::io_utils::{atomic_write_yaml, now_iso, read_json_file, read_yaml_file};
use super::lock::with_task_lock;
use super::models::validate_delivery_payload;
use super::task_store::require_task_dir;

pub fn load_delivery(ws_root: &Path, task_id: &str, required: bool) -> Result<Option<Value>, WbError> {
    let tdir = require_task_dir(ws_root, task_id)?;
    let dpath = tdir.join("delivery.yaml");
    if !dpath.is_file() {
        if required {
            return Err(WbError::not_found(format!(
                "任务 {task_id} 尚未配置 delivery.yaml"
            )));
        }
        return Ok(None);
    }
    let raw = read_yaml_file(&dpath)?;
    Ok(Some(validate_delivery_payload(&raw)?))
}

pub fn apply_delivery(
    ws_root: &Path,
    task_id: &str,
    payload: &Value,
    expected_revision: Option<i64>,
) -> Result<Value, WbError> {
    let tdir = require_task_dir(ws_root, task_id)?;
    let payload_obj = payload
        .as_object()
        .ok_or_else(|| WbError::validation("delivery 声明必须为字典结构"))?;

    with_task_lock(&tdir, |_| {
        let dpath = tdir.join("delivery.yaml");
        let current_rev = if dpath.is_file() {
            let existing = validate_delivery_payload(&read_yaml_file(&dpath)?)?;
            existing
                .get("revision")
                .and_then(|v| v.as_i64())
                .unwrap_or(0)
        } else {
            0
        };

        let cand_rev = if let Some(exp) = expected_revision {
            exp
        } else if let Some(exp) = payload_obj.get("expected_revision").and_then(|v| v.as_i64()) {
            exp
        } else if let Some(rev) = payload_obj.get("revision").and_then(|v| v.as_i64()) {
            rev
        } else if current_rev == 0 {
            0
        } else {
            return Err(WbError::conflict(format!(
                "修订号冲突：当前 delivery.yaml 修订号为 {current_rev}，提交时必须指定匹配的 expected_revision/revision"
            )));
        };

        if cand_rev != current_rev {
            return Err(WbError::conflict(format!(
                "修订号冲突：期望 {cand_rev}，但当前最新修订号为 {current_rev}"
            )));
        }

        let mut normalized = validate_delivery_payload(payload)?;
        let new_rev = current_rev + 1;
        let obj = normalized.as_object_mut().unwrap();
        obj.insert("revision".into(), Value::from(new_rev));
        obj.insert("updated_at".into(), Value::String(now_iso()));

        let history_dir = tdir.join("delivery-history");
        fs::create_dir_all(&history_dir)
            .map_err(|e| WbError::io(format!("创建历史快照目录失败: {e}")))?;
        let hist_file = history_dir.join(format!("rev-{new_rev:04}.yaml"));
        if hist_file.exists() {
            return Err(WbError::conflict(format!(
                "历史快照文件已存在，拒绝覆盖: {}",
                hist_file.display()
            )));
        }

        atomic_write_yaml(&hist_file, &normalized)?;
        atomic_write_yaml(&dpath, &normalized)?;
        Ok(normalized)
    })
}

pub fn pin_delivery(
    ws_root: &Path,
    task_id: &str,
    item_id: Option<&str>,
    commit: Option<&str>,
    expected_revision: Option<i64>,
) -> Result<Value, WbError> {
    let tdir = require_task_dir(ws_root, task_id)?;
    with_task_lock(&tdir, |_| {
        let dpath = tdir.join("delivery.yaml");
        if !dpath.is_file() {
            return Err(WbError::not_found(format!(
                "任务 {task_id} 尚未配置 delivery.yaml"
            )));
        }
        let mut current = validate_delivery_payload(&read_yaml_file(&dpath)?)?;
        let current_rev = current
            .get("revision")
            .and_then(|v| v.as_i64())
            .unwrap_or(0);

        if let Some(exp) = expected_revision {
            if exp != current_rev {
                return Err(WbError::conflict(format!(
                    "修订号冲突：期望 {exp}，当前为 {current_rev}"
                )));
            }
        }

        let mut obs_map: HashMap<String, Value> = HashMap::new();
        let obs_path = ws_root
            .join(".wb-runtime")
            .join("observations")
            .join(format!("{task_id}.json"));
        if obs_path.is_file() {
            if let Ok(obs_data) = read_json_file(&obs_path) {
                if let Some(arr) = obs_data.get("items").and_then(|v| v.as_array()) {
                    for it in arr {
                        if let Some(id) = it.get("id").and_then(|v| v.as_str()) {
                            obs_map.insert(id.to_string(), it.clone());
                        }
                    }
                }
            }
        }

        let mut updated_count = 0usize;
        let items = current
            .as_object_mut()
            .unwrap()
            .get_mut("items")
            .unwrap()
            .as_array_mut()
            .unwrap();

        for item in items.iter_mut() {
            let item_obj = item.as_object_mut().unwrap();
            let cur_id = item_obj.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
            if let Some(target_id) = item_id {
                if cur_id != target_id {
                    continue;
                }
            }
            let pin_commit = if let Some(c) = commit {
                c.trim().to_string()
            } else {
                obs_map
                    .get(&cur_id)
                    .and_then(|o| o.get("head_commit"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string()
            };
            if pin_commit.is_empty() {
                return Err(WbError::not_found(format!(
                    "交付项 {cur_id} 缺少可用提交记录，请先运行 `wb sync {task_id}` 或传入 --commit"
                )));
            }
            item_obj.insert("compare_base".into(), Value::String(pin_commit));
            updated_count += 1;
        }

        if updated_count == 0 {
            return Err(WbError::not_found(format!(
                "任务 {task_id} 中未找到交付项: {}",
                item_id.unwrap_or("")
            )));
        }

        let new_rev = current_rev + 1;
        let obj = current.as_object_mut().unwrap();
        obj.insert("revision".into(), Value::from(new_rev));
        obj.insert("updated_at".into(), Value::String(now_iso()));

        let history_dir = tdir.join("delivery-history");
        fs::create_dir_all(&history_dir)
            .map_err(|e| WbError::io(format!("创建历史快照目录失败: {e}")))?;
        let hist_file = history_dir.join(format!("rev-{new_rev:04}.yaml"));
        if hist_file.exists() {
            return Err(WbError::conflict(format!(
                "历史快照文件已存在: {}",
                hist_file.display()
            )));
        }

        atomic_write_yaml(&hist_file, &current)?;
        atomic_write_yaml(&dpath, &current)?;
        Ok(current)
    })
}
