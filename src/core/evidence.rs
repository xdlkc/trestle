use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use super::delivery::load_delivery;
use super::errors::WbError;
use super::io_utils::{atomic_write_bytes, atomic_write_json, now_iso, read_json_file, sha256_file};
use super::lock::with_task_lock;
use super::task_store::require_task_dir;

fn evidence_dir(tdir: &Path) -> Result<PathBuf, WbError> {
    let edir = tdir.join("evidence");
    fs::create_dir_all(&edir)
        .map_err(|e| WbError::io(format!("创建 evidence 目录失败: {e}")))?;
    Ok(edir)
}

fn load_manifest(edir: &Path) -> Result<Value, WbError> {
    let mpath = edir.join("manifest.json");
    if !mpath.is_file() {
        let mut m = Map::new();
        m.insert("schema".into(), Value::from(1));
        m.insert("confirmations".into(), Value::Object(Map::new()));
        m.insert("artifacts".into(), Value::Array(Vec::new()));
        return Ok(Value::Object(m));
    }
    let mut data = read_json_file(&mpath)?;
    let obj = data
        .as_object_mut()
        .ok_or_else(|| WbError::validation(format!("evidence/manifest.json 格式无效: {}", mpath.display())))?;
    if !obj.contains_key("schema") {
        obj.insert("schema".into(), Value::from(1));
    }
    if !obj.contains_key("confirmations") {
        obj.insert("confirmations".into(), Value::Object(Map::new()));
    }
    if !obj.contains_key("artifacts") {
        obj.insert("artifacts".into(), Value::Array(Vec::new()));
    }
    Ok(data)
}

pub fn confirm_action(
    ws_root: &Path,
    task_id: &str,
    action_id: &str,
    confirmed_by: &str,
    source: &str,
    digest: &str,
    note: &str,
) -> Result<Value, WbError> {
    if confirmed_by.trim().is_empty() {
        return Err(WbError::validation("必须显式传入确认人 (--by)"));
    }
    if source.trim().is_empty() {
        return Err(WbError::validation("必须显式传入来源说明 (--source)"));
    }
    if digest.trim().is_empty() {
        return Err(WbError::validation("必须显式传入动作范围校验值 (--digest)"));
    }

    let tdir = require_task_dir(ws_root, task_id)?;
    with_task_lock(&tdir, |_| {
        let delivery = load_delivery(ws_root, task_id, true)?.unwrap();
        let actions = delivery
            .get("actions")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();

        let target_action = actions
            .into_iter()
            .find(|a| a.get("id").and_then(|v| v.as_str()) == Some(action_id))
            .ok_or_else(|| {
                WbError::not_found(format!(
                    "任务 {task_id} 的交付声明中不存在动作: {action_id}"
                ))
            })?;

        let expected_digest = target_action
            .get("scope_sha256")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if digest.trim() != expected_digest {
            return Err(WbError::conflict(format!(
                "动作 {action_id} 的范围校验值不匹配：期望 {expected_digest}，收到 {}",
                digest.trim()
            )));
        }

        let ts = now_iso();
        let mut receipt = Map::new();
        receipt.insert("schema".into(), Value::from(1));
        receipt.insert("task_id".into(), Value::String(task_id.to_string()));
        receipt.insert("action_id".into(), Value::String(action_id.to_string()));
        receipt.insert("kind".into(), target_action["kind"].clone());
        receipt.insert("title".into(), target_action["title"].clone());
        receipt.insert(
            "confirmed_by".into(),
            Value::String(confirmed_by.trim().to_string()),
        );
        receipt.insert("source".into(), Value::String(source.trim().to_string()));
        receipt.insert(
            "scope_sha256".into(),
            Value::String(expected_digest.to_string()),
        );
        receipt.insert(
            "delivery_revision".into(),
            delivery
                .get("revision")
                .cloned()
                .unwrap_or_else(|| Value::from(1)),
        );
        receipt.insert("confirmed_at".into(), Value::String(ts.clone()));
        receipt.insert("note".into(), Value::String(note.trim().to_string()));
        let receipt_val = Value::Object(receipt);

        let edir = evidence_dir(&tdir)?;
        let receipt_name = format!("confirm-{action_id}.json");
        atomic_write_json(&edir.join(&receipt_name), &receipt_val)?;

        let mut manifest = load_manifest(&edir)?;
        let mut conf_entry = Map::new();
        conf_entry.insert("receipt_file".into(), Value::String(receipt_name));
        conf_entry.insert(
            "scope_sha256".into(),
            Value::String(expected_digest.to_string()),
        );
        conf_entry.insert(
            "confirmed_by".into(),
            Value::String(confirmed_by.trim().to_string()),
        );
        conf_entry.insert("confirmed_at".into(), Value::String(ts));
        manifest
            .as_object_mut()
            .unwrap()
            .get_mut("confirmations")
            .unwrap()
            .as_object_mut()
            .unwrap()
            .insert(action_id.to_string(), Value::Object(conf_entry));
        atomic_write_json(&edir.join("manifest.json"), &manifest)?;

        Ok(receipt_val)
    })
}

pub fn evaluate_actions_status(ws_root: &Path, task_id: &str) -> Result<Vec<Value>, WbError> {
    let tdir = require_task_dir(ws_root, task_id)?;
    let Some(delivery) = load_delivery(ws_root, task_id, false)? else {
        return Ok(Vec::new());
    };

    let edir = tdir.join("evidence");
    let manifest = if edir.is_dir() {
        load_manifest(&edir).unwrap_or_else(|_| Value::Object(Map::new()))
    } else {
        Value::Object(Map::new())
    };
    let prior_confirmations = manifest
        .get("confirmations")
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default();

    let actions = delivery
        .get("actions")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    let mut results = Vec::with_capacity(actions.len());
    for act in actions {
        let act_id = act.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let expected_sha = act
            .get("scope_sha256")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let receipt_path = edir.join(format!("confirm-{act_id}.json"));

        let mut entry = Map::new();
        entry.insert("id".into(), Value::String(act_id.clone()));
        entry.insert("kind".into(), act.get("kind").cloned().unwrap_or(Value::Null));
        entry.insert("title".into(), act.get("title").cloned().unwrap_or(Value::Null));
        entry.insert(
            "item_id".into(),
            act.get("item_id").cloned().unwrap_or(Value::Null),
        );
        entry.insert(
            "required".into(),
            act.get("required").cloned().unwrap_or(Value::Bool(true)),
        );
        entry.insert("scope_sha256".into(), Value::String(expected_sha.clone()));
        entry.insert("valid".into(), Value::Bool(false));
        entry.insert("status".into(), Value::String("unconfirmed".into()));
        entry.insert("reason".into(), Value::String("尚无确认回执".into()));
        entry.insert("receipt".into(), Value::Null);

        if !receipt_path.is_file() {
            if prior_confirmations.contains_key(&act_id) {
                entry.insert("status".into(), Value::String("missing_receipt".into()));
                entry.insert(
                    "reason".into(),
                    Value::String(format!("确认回执文件丢失: evidence/confirm-{act_id}.json")),
                );
            }
            results.push(Value::Object(entry));
            continue;
        }

        let receipt = match read_json_file(&receipt_path) {
            Ok(r) => r,
            Err(e) => {
                entry.insert("status".into(), Value::String("invalid_receipt".into()));
                entry.insert(
                    "reason".into(),
                    Value::String(format!("回执文件解析失败: {}", e.message)),
                );
                results.push(Value::Object(entry));
                continue;
            }
        };

        let by_ok = receipt
            .get("confirmed_by")
            .and_then(|v| v.as_str())
            .map(|s| !s.trim().is_empty())
            .unwrap_or(false);
        let src_ok = receipt
            .get("source")
            .and_then(|v| v.as_str())
            .map(|s| !s.trim().is_empty())
            .unwrap_or(false);
        if !receipt.is_object() || !by_ok || !src_ok {
            entry.insert("status".into(), Value::String("invalid_receipt".into()));
            entry.insert("reason".into(), Value::String("回执缺少确认人或来源说明".into()));
            results.push(Value::Object(entry));
            continue;
        }

        entry.insert("receipt".into(), receipt.clone());
        let rec_sha = receipt
            .get("scope_sha256")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if rec_sha != expected_sha {
            entry.insert("status".into(), Value::String("stale".into()));
            entry.insert(
                "reason".into(),
                Value::String(format!(
                    "动作范围已变更，原确认失效（回执={rec_sha}, 当前={expected_sha}）"
                )),
            );
            results.push(Value::Object(entry));
            continue;
        }

        let confirmed_by = receipt["confirmed_by"].as_str().unwrap_or("");
        let source = receipt["source"].as_str().unwrap_or("");
        entry.insert("valid".into(), Value::Bool(true));
        entry.insert("status".into(), Value::String("valid".into()));
        entry.insert(
            "reason".into(),
            Value::String(format!("已由 {confirmed_by} 确认（{source}）")),
        );
        results.push(Value::Object(entry));
    }

    Ok(results)
}

pub fn import_evidence(
    ws_root: &Path,
    task_id: &str,
    source_file: &Path,
    name: Option<&str>,
    note: &str,
) -> Result<Value, WbError> {
    if !source_file.is_file() {
        return Err(WbError::not_found(format!(
            "待导入的证据文件不存在: {}",
            source_file.display()
        )));
    }
    let target_name = match name {
        Some(n) => Path::new(n)
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default(),
        None => source_file
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default(),
    };
    if target_name.is_empty()
        || target_name == "."
        || target_name == ".."
        || target_name.starts_with("confirm-")
    {
        return Err(WbError::validation(format!(
            "非法的证据文件名: {target_name:?}"
        )));
    }

    let tdir = require_task_dir(ws_root, task_id)?;
    with_task_lock(&tdir, |_| {
        let edir = evidence_dir(&tdir)?;
        let dest = edir.join(&target_name);
        let data = fs::read(source_file).map_err(|e| {
            WbError::io(format!("读取证据源文件失败 {}: {e}", source_file.display()))
        })?;
        atomic_write_bytes(&dest, &data)?;
        let digest = sha256_file(&dest)?;
        let ts = now_iso();

        let mut manifest = load_manifest(&edir)?;
        let mut artifacts: Vec<Value> = manifest
            .get("artifacts")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter(|a| a.get("file").and_then(|v| v.as_str()) != Some(&target_name))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();

        let mut rec = Map::new();
        rec.insert("file".into(), Value::String(target_name));
        rec.insert("sha256".into(), Value::String(digest));
        rec.insert("bytes".into(), Value::from(data.len() as u64));
        rec.insert("imported_at".into(), Value::String(ts));
        rec.insert("note".into(), Value::String(note.trim().to_string()));
        let rec_val = Value::Object(rec);
        artifacts.push(rec_val.clone());

        manifest
            .as_object_mut()
            .unwrap()
            .insert("artifacts".into(), Value::Array(artifacts));
        atomic_write_json(&edir.join("manifest.json"), &manifest)?;
        Ok(rec_val)
    })
}

pub fn export_evidence(ws_root: &Path, task_id: &str, out_dir: &Path) -> Result<Value, WbError> {
    let tdir = require_task_dir(ws_root, task_id)?;
    let edir = tdir.join("evidence");
    fs::create_dir_all(out_dir)
        .map_err(|e| WbError::io(format!("无法创建导出目录 {}: {e}", out_dir.display())))?;

    let mut exported = Vec::new();
    if edir.is_dir() {
        let mut items: Vec<PathBuf> = fs::read_dir(&edir)
            .map_err(|e| WbError::io(format!("读取证据目录失败 {}: {e}", edir.display())))?
            .filter_map(|e| e.ok().map(|x| x.path()))
            .filter(|p| p.is_file())
            .collect();
        items.sort();

        for item in items {
            let fname = item.file_name().unwrap().to_string_lossy().into_owned();
            let target = out_dir.join(&fname);
            fs::copy(&item, &target).map_err(|e| {
                WbError::io(format!("导出证据文件失败 {}: {e}", item.display()))
            })?;
            let meta = fs::metadata(&target)
                .map_err(|e| WbError::io(format!("获取文件元数据失败 {}: {e}", target.display())))?;
            let mut m = Map::new();
            m.insert("file".into(), Value::String(fname));
            m.insert("sha256".into(), Value::String(sha256_file(&target)?));
            m.insert("bytes".into(), Value::from(meta.len()));
            exported.push(Value::Object(m));
        }
    }

    let mut summary = Map::new();
    summary.insert("schema".into(), Value::from(1));
    summary.insert("task_id".into(), Value::String(task_id.to_string()));
    summary.insert("exported_at".into(), Value::String(now_iso()));
    summary.insert("files".into(), Value::Array(exported));
    let summary_val = Value::Object(summary);
    atomic_write_json(&out_dir.join("export-manifest.json"), &summary_val)?;
    Ok(summary_val)
}
