use std::path::Path;

use serde_json::{Map, Value};

use crate::core::errors::WbError;
use crate::core::io_utils::{atomic_write_yaml, read_yaml_file};
use crate::core::models::SCHEMA_WORKSPACE;

pub const DEFAULT_REPOS_ROOT: &str = "../repos";
pub const DEFAULT_TIMEZONE: &str = "Asia/Shanghai";
pub const DEFAULT_DASHBOARD_PORT: i64 = 8791;

pub fn default_config_value(name: &str) -> Value {
    let mut m = Map::new();
    m.insert("schema".into(), Value::from(SCHEMA_WORKSPACE));
    m.insert("name".into(), Value::String(name.to_string()));
    m.insert("repos_root".into(), Value::String(DEFAULT_REPOS_ROOT.into()));
    m.insert("timezone".into(), Value::String(DEFAULT_TIMEZONE.into()));
    m.insert("dashboard_port".into(), Value::from(DEFAULT_DASHBOARD_PORT));
    m.insert("extensions".into(), Value::Array(Vec::new()));
    Value::Object(m)
}

pub fn validate_workspace_config(raw: &Value) -> Result<Value, WbError> {
    let obj = raw
        .as_object()
        .ok_or_else(|| WbError::validation(".wb.yml 顶层必须为字典"))?;

    let schema = match obj.get("schema") {
        None => SCHEMA_WORKSPACE,
        Some(v) => v
            .as_i64()
            .filter(|&s| s >= 1)
            .ok_or_else(|| WbError::validation(format!(".wb.yml schema 非法: {v}")))?,
    };

    let name = obj
        .get("name")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| WbError::validation(".wb.yml 缺少有效 name"))?
        .to_string();

    let repos_root = match obj.get("repos_root") {
        None => DEFAULT_REPOS_ROOT.to_string(),
        Some(v) => v
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| WbError::validation(".wb.yml repos_root 必须为非空字符串"))?
            .to_string(),
    };

    let timezone = match obj.get("timezone") {
        None => DEFAULT_TIMEZONE.to_string(),
        Some(v) => v
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| WbError::validation(".wb.yml timezone 必须为非空字符串"))?
            .to_string(),
    };

    let port = match obj.get("dashboard_port") {
        None => DEFAULT_DASHBOARD_PORT,
        Some(v) => v
            .as_i64()
            .filter(|&p| (1..=65535).contains(&p))
            .ok_or_else(|| {
                WbError::validation(format!(".wb.yml dashboard_port 必须为 1..65535 整数: {v}"))
            })?,
    };

    let mut dedup_ext: Vec<String> = Vec::new();
    if let Some(ext_val) = obj.get("extensions") {
        let arr = ext_val
            .as_array()
            .ok_or_else(|| WbError::validation(".wb.yml extensions 必须为非空字符串列表"))?;
        for item in arr {
            let s = item
                .as_str()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| WbError::validation(".wb.yml extensions 必须为非空字符串列表"))?;
            if !dedup_ext.iter().any(|x| x == s) {
                dedup_ext.push(s.to_string());
            }
        }
    }

    let mut out = Map::new();
    out.insert("schema".into(), Value::from(schema));
    out.insert("name".into(), Value::String(name));
    out.insert("repos_root".into(), Value::String(repos_root));
    out.insert("timezone".into(), Value::String(timezone));
    out.insert("dashboard_port".into(), Value::from(port));
    out.insert(
        "extensions".into(),
        Value::Array(dedup_ext.into_iter().map(Value::String).collect()),
    );
    Ok(Value::Object(out))
}

pub fn load_workspace_config(ws_root: &Path) -> Result<Value, WbError> {
    let cfg_path = ws_root.join(".wb.yml");
    if !cfg_path.is_file() {
        return Err(WbError::not_found(format!(
            "缺少工作台配置文件: {}",
            cfg_path.display()
        )));
    }
    let raw = read_yaml_file(&cfg_path)?;
    validate_workspace_config(&raw)
}

pub fn save_workspace_config(ws_root: &Path, cfg: &Value) -> Result<Value, WbError> {
    let validated = validate_workspace_config(cfg)?;
    atomic_write_yaml(&ws_root.join(".wb.yml"), &validated)?;
    Ok(validated)
}
