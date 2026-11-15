use std::fs;
use std::path::Path;

use chrono::Utc;
use serde_json::{json, Map, Value};

use crate::core::errors::WbError;
use crate::core::io_utils::{atomic_write_json, atomic_write_text, atomic_write_yaml, read_yaml_file};
use crate::core::lock::with_workspace_lock;
use crate::core::models::{SCHEMA_SESSION_TYPES, SCHEMA_WORKSPACE};
use crate::core::task_store::rebuild_index;

use super::config::{
    validate_workspace_config, DEFAULT_DASHBOARD_PORT, DEFAULT_REPOS_ROOT, DEFAULT_TIMEZONE,
};
use super::init_ws::{TEMPLATE_TASKS_README, TEMPLATE_WORKBENCH_MD, WORKBENCH_GITIGNORE};

pub fn upgrade_workspace(ws_root: &Path, apply: bool) -> Result<Value, WbError> {
    let cfg_path = ws_root.join(".wb.yml");
    let raw_cfg = if cfg_path.is_file() {
        let v = read_yaml_file(&cfg_path)?;
        if v.is_null() {
            Value::Object(Map::new())
        } else {
            v
        }
    } else {
        Value::Object(Map::new())
    };

    let raw_obj = raw_cfg
        .as_object()
        .ok_or_else(|| WbError::validation(".wb.yml 格式无效，无法升级"))?;

    let mut merged = Map::new();
    merged.insert("schema".into(), Value::from(SCHEMA_WORKSPACE));
    merged.insert("name".into(), Value::String("my-workbench".into()));
    merged.insert("repos_root".into(), Value::String(DEFAULT_REPOS_ROOT.into()));
    merged.insert("timezone".into(), Value::String(DEFAULT_TIMEZONE.into()));
    merged.insert("dashboard_port".into(), Value::from(DEFAULT_DASHBOARD_PORT));
    merged.insert("extensions".into(), Value::Array(Vec::new()));
    for (k, v) in raw_obj {
        merged.insert(k.clone(), v.clone());
    }
    merged.insert("schema".into(), Value::from(SCHEMA_WORKSPACE));
    let upgraded_cfg = validate_workspace_config(&Value::Object(merged))?;

    let mut planned_changes: Vec<String> = Vec::new();
    if Value::Object(raw_obj.clone()) != upgraded_cfg {
        planned_changes.push("更新 .wb.yml 补全默认配置键".to_string());
    }
    let wb_md_path = ws_root.join("WORKBENCH.md");
    if !wb_md_path.is_file()
        || fs::read_to_string(&wb_md_path).ok().as_deref() != Some(TEMPLATE_WORKBENCH_MD)
    {
        planned_changes.push("同步最新通用 WORKBENCH.md".to_string());
    }
    let tr_path = ws_root.join("tasks").join("README.md");
    if !tr_path.is_file()
        || fs::read_to_string(&tr_path).ok().as_deref() != Some(TEMPLATE_TASKS_README)
    {
        planned_changes.push("同步 tasks/README.md".to_string());
    }
    let gi_path = ws_root.join(".gitignore");
    if !gi_path.is_file()
        || !fs::read_to_string(&gi_path)
            .map(|s| s.contains(".wb-runtime/"))
            .unwrap_or(false)
    {
        planned_changes.push("补充 .gitignore 忽略规则".to_string());
    }
    let st_path = ws_root.join("tasks").join("session-types.json");
    if !st_path.is_file() {
        planned_changes.push("补建 tasks/session-types.json".to_string());
    }
    planned_changes.push("重建 tasks/INDEX.md".to_string());

    if !apply {
        return Ok(json!({
            "workspace": ws_root.to_string_lossy(),
            "applied": false,
            "dry_run": true,
            "planned_changes": planned_changes,
            "backup_dir": Value::Null,
        }));
    }

    with_workspace_lock(ws_root, |_| {
        let stamp = Utc::now().format("%Y%m%dT%H%M%SZ").to_string();
        let backups_root = ws_root.join(".wb-runtime").join("backups");
        let mut backup_dir = backups_root.join(&stamp);
        let mut suffix = 1usize;
        while backup_dir.exists() {
            backup_dir = backups_root.join(format!("{stamp}-{suffix}"));
            suffix += 1;
        }
        fs::create_dir_all(&backup_dir)
            .map_err(|e| WbError::io(format!("创建备份目录失败: {e}")))?;

        for rel in [".wb.yml", "WORKBENCH.md", ".gitignore"] {
            let src = ws_root.join(rel);
            if src.is_file() {
                let dst = backup_dir.join(rel);
                fs::copy(&src, &dst)
                    .map_err(|e| WbError::io(format!("备份文件失败 {}: {e}", src.display())))?;
            }
        }

        for dir in ["tasks", "docs", "analysis"] {
            fs::create_dir_all(ws_root.join(dir))
                .map_err(|e| WbError::io(format!("创建目录失败 {dir}: {e}")))?;
        }

        atomic_write_yaml(&cfg_path, &upgraded_cfg)?;
        atomic_write_text(&wb_md_path, TEMPLATE_WORKBENCH_MD)?;
        atomic_write_text(&tr_path, TEMPLATE_TASKS_README)?;
        if !gi_path.is_file() {
            atomic_write_text(&gi_path, WORKBENCH_GITIGNORE)?;
        } else {
            let existing = fs::read_to_string(&gi_path)
                .map_err(|e| WbError::io(format!("读取 .gitignore 失败: {e}")))?;
            if !existing.contains(".wb-runtime/") {
                let updated = format!("{}\n.wb-runtime/\ntasks/*/.lock\n", existing.trim_end_matches('\n'));
                atomic_write_text(&gi_path, &updated)?;
            }
        }
        if !st_path.is_file() {
            atomic_write_json(
                &st_path,
                &json!({"schema": SCHEMA_SESSION_TYPES, "sessions": []}),
            )?;
        }

        rebuild_index(ws_root)?;

        Ok(json!({
            "workspace": ws_root.to_string_lossy(),
            "applied": true,
            "dry_run": false,
            "planned_changes": planned_changes,
            "backup_dir": backup_dir.to_string_lossy(),
        }))
    })
}
