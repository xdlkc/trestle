use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use regex::Regex;
use serde_json::{Map, Value};

use super::errors::WbError;
use super::io_utils::sha256_bytes;

pub const SCHEMA_TASK: i64 = 1;
pub const SCHEMA_DELIVERY: i64 = 1;
pub const SCHEMA_WORKSPACE: i64 = 1;
pub const SCHEMA_SESSION_TYPES: i64 = 1;

pub const VALID_STATUSES: &[&str] = &[
    "planning",
    "in_progress",
    "blocked",
    "review",
    "paused",
    "done",
    "cancelled",
];
pub const ACTIVE_STATUSES: &[&str] = &[
    "planning",
    "in_progress",
    "blocked",
    "review",
    "paused",
];
pub const VALID_CATEGORIES: &[&str] = &[
    "requirement",
    "cr",
    "oncall",
    "analysis",
    "unclassified",
];
pub const VALID_PRIORITIES: &[&str] = &["P0", "P1", "P2", "P3"];
pub const VALID_ACTION_KINDS: &[&str] = &[
    "auto_check",
    "deploy_check",
    "manual_review",
    "manual_action",
];

pub fn session_prefix_category(prefix: &str) -> Option<&'static str> {
    match prefix {
        "[FD]" => Some("requirement"),
        "[CR]" => Some("cr"),
        "[OC]" => Some("oncall"),
        "[DA]" => Some("analysis"),
        _ => None,
    }
}

fn task_id_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$").unwrap())
}

fn leading_bracket_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^(\[[^\]]*\])\s*(.*)$").unwrap())
}

pub fn validate_task_id(task_id: &str) -> Result<String, WbError> {
    if !task_id_regex().is_match(task_id) || task_id.contains("..") {
        return Err(WbError::validation(format!(
            "非法任务标识 {task_id:?}：仅允许字母、数字、点、下划线和连字符，且不能包含 '..'"
        )));
    }
    Ok(task_id.to_string())
}

pub fn validate_status(status: &str) -> Result<String, WbError> {
    if !VALID_STATUSES.contains(&status) {
        return Err(WbError::validation(format!(
            "非法任务状态 {status:?}，可选值: {}",
            VALID_STATUSES.join(", ")
        )));
    }
    Ok(status.to_string())
}

pub fn validate_category(category: &str) -> Result<String, WbError> {
    if !VALID_CATEGORIES.contains(&category) {
        return Err(WbError::validation(format!(
            "非法分类 {category:?}，可选值: {}",
            VALID_CATEGORIES.join(", ")
        )));
    }
    Ok(category.to_string())
}

pub fn validate_priority(priority: &str) -> Result<String, WbError> {
    if !VALID_PRIORITIES.contains(&priority) {
        return Err(WbError::validation(format!(
            "非法优先级 {priority:?}，可选值: {}",
            VALID_PRIORITIES.join(", ")
        )));
    }
    Ok(priority.to_string())
}

/// Validate session title and extract optional single prefix ([FD]/[CR]/[OC]/[DA]).
pub fn parse_and_validate_session_title(
    title: &str,
) -> Result<(String, Option<String>, Option<String>), WbError> {
    let cleaned = title.trim();
    if cleaned.is_empty() {
        return Err(WbError::validation("会话标题不能为空"));
    }
    let Some(caps) = leading_bracket_regex().captures(cleaned) else {
        return Ok((cleaned.to_string(), None, None));
    };
    let prefix = caps.get(1).map(|m| m.as_str()).unwrap_or("");
    let rest = caps.get(2).map(|m| m.as_str().trim()).unwrap_or("");

    let Some(cat) = session_prefix_category(prefix) else {
        return Err(WbError::validation(format!(
            "不支持的会话标题前缀 {prefix:?}，仅支持: [FD], [CR], [OC], [DA]"
        )));
    };
    if rest.is_empty() {
        return Err(WbError::validation(format!(
            "会话标题 {cleaned:?} 在前缀后缺少正文"
        )));
    }
    if leading_bracket_regex().is_match(rest) {
        return Err(WbError::validation(format!(
            "会话标题 {cleaned:?} 仅允许单一前缀，禁止叠加多个前缀"
        )));
    }
    Ok((
        format!("{prefix} {rest}"),
        Some(prefix.to_string()),
        Some(cat.to_string()),
    ))
}

pub fn validate_branch_name(branch: &str, field_name: &str) -> Result<String, WbError> {
    let b = branch.trim();
    if b.is_empty() {
        return Err(WbError::validation(format!("{field_name} 不能为空")));
    }
    if b != branch {
        return Err(WbError::validation(format!(
            "{field_name} 不能包含首尾空白: {branch:?}"
        )));
    }
    if b.starts_with('-') || b.starts_with('/') || b.starts_with('.') {
        return Err(WbError::validation(format!(
            "非法分支名 {branch:?}: 不能以 '-', '/' 或 '.' 开头"
        )));
    }
    if b.ends_with('/') || b.ends_with('.') || b.ends_with(".lock") {
        return Err(WbError::validation(format!(
            "非法分支名 {branch:?}: 不能以 '/', '.' 或 '.lock' 结尾"
        )));
    }
    for bad in ["..", "@{", "//", "/."] {
        if b.contains(bad) {
            return Err(WbError::validation(format!(
                "非法分支名 {branch:?}: 包含禁用序列 {bad:?}"
            )));
        }
    }
    for ch in b.chars() {
        if (ch as u32) < 32 || (ch as u32) == 127 || " ~^:?*[\\".contains(ch) {
            return Err(WbError::validation(format!(
                "非法分支名 {branch:?}: 包含非法字符 {ch:?}"
            )));
        }
    }
    if b == "@" {
        return Err(WbError::validation("非法分支名 '@'"));
    }
    Ok(b.to_string())
}

pub fn validate_relative_subpath(path_str: &str, field_name: &str) -> Result<String, WbError> {
    let trimmed = path_str.trim();
    if trimmed.is_empty() {
        return Err(WbError::validation(format!("{field_name} 不能为空")));
    }
    let normalized = trimmed.replace('\\', "/");
    if normalized.starts_with('/') {
        return Err(WbError::validation(format!(
            "{field_name} 必须为相对路径: {path_str:?}"
        )));
    }
    let parts: Vec<&str> = normalized.split('/').filter(|p| !p.is_empty()).collect();
    if parts.iter().any(|p| *p == "." || *p == "..") {
        return Err(WbError::validation(format!(
            "{field_name} 不能包含 '.' 或 '..' 路径跳转: {path_str:?}"
        )));
    }
    if parts.is_empty() {
        Ok(".".to_string())
    } else {
        Ok(parts.join("/"))
    }
}

fn canonicalize_value(val: &Value) -> Value {
    match val {
        Value::Array(arr) => Value::Array(arr.iter().map(canonicalize_value).collect()),
        Value::Object(map) => {
            let mut sorted = Map::new();
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            for k in keys {
                sorted.insert(k.clone(), canonicalize_value(&map[k]));
            }
            Value::Object(sorted)
        }
        other => other.clone(),
    }
}

pub fn compute_action_scope_sha256(action: &Map<String, Value>) -> String {
    let mut payload = Map::new();
    payload.insert(
        "id".to_string(),
        Value::String(action.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string()),
    );
    payload.insert(
        "item_id".to_string(),
        action.get("item_id").cloned().unwrap_or(Value::Null),
    );
    payload.insert(
        "kind".to_string(),
        Value::String(action.get("kind").and_then(|v| v.as_str()).unwrap_or("").to_string()),
    );
    payload.insert(
        "scope".to_string(),
        canonicalize_value(action.get("scope").unwrap_or(&Value::Object(Map::new()))),
    );
    payload.insert(
        "title".to_string(),
        Value::String(action.get("title").and_then(|v| v.as_str()).unwrap_or("").to_string()),
    );
    let compact = serde_json::to_string(&Value::Object(payload)).unwrap_or_default();
    sha256_bytes(compact.as_bytes())
}

fn path_parts(raw: &str) -> Vec<String> {
    raw.replace('\\', "/")
        .split('/')
        .filter(|p| !p.is_empty())
        .map(|s| s.to_string())
        .collect()
}

pub fn validate_delivery_payload(payload: &Value) -> Result<Value, WbError> {
    let obj = payload
        .as_object()
        .ok_or_else(|| WbError::validation("delivery.yaml 顶层必须为字典结构"))?;

    let items_raw = match obj.get("items") {
        None => Vec::new(),
        Some(v) => v
            .as_array()
            .ok_or_else(|| WbError::validation("delivery.items 必须为列表"))?
            .clone(),
    };

    let mut normalized_items: Vec<Value> = Vec::with_capacity(items_raw.len());
    let mut seen_ids: HashSet<String> = HashSet::new();

    for (idx, item_val) in items_raw.iter().enumerate() {
        let item = item_val
            .as_object()
            .ok_or_else(|| WbError::validation(format!("delivery.items[{idx}] 必须为字典")))?;
        let item_id = item
            .get("id")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| WbError::validation(format!("delivery.items[{idx}] 缺少有效 id")))?
            .to_string();

        if !seen_ids.insert(item_id.clone()) {
            return Err(WbError::validation(format!(
                "delivery.items 存在重复 id: {item_id:?}"
            )));
        }

        let repo = item
            .get("repo")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| WbError::validation(format!("交付项 {item_id:?} 缺少有效 repo")))?
            .to_string();
        if path_parts(&repo).iter().any(|p| p == "..") {
            return Err(WbError::validation(format!(
                "交付项 {item_id:?} 的 repo 不能包含 '..': {repo:?}"
            )));
        }

        let worktree: Value = match item.get("worktree") {
            None | Some(Value::Null) => Value::Null,
            Some(v) => {
                let s = v.as_str().map(str::trim).filter(|s| !s.is_empty()).ok_or_else(|| {
                    WbError::validation(format!("交付项 {item_id:?} 的 worktree 必须为非空字符串"))
                })?;
                Value::String(s.to_string())
            }
        };

        let src_branch = validate_branch_name(
            item.get("source_branch").and_then(|v| v.as_str()).unwrap_or(""),
            &format!("items[{item_id}].source_branch"),
        )?;
        let tgt_branch = validate_branch_name(
            item.get("target_branch").and_then(|v| v.as_str()).unwrap_or(""),
            &format!("items[{item_id}].target_branch"),
        )?;

        let compare_base: Value = match item.get("compare_base") {
            None | Some(Value::Null) => Value::Null,
            Some(v) => {
                let s = v.as_str().map(str::trim).unwrap_or("");
                if s.is_empty() {
                    Value::Null
                } else {
                    Value::String(s.to_string())
                }
            }
        };

        let raw_allowed = match item.get("allowed_paths") {
            None => Vec::new(),
            Some(v) => v
                .as_array()
                .ok_or_else(|| {
                    WbError::validation(format!("交付项 {item_id:?} 的 allowed_paths 必须为列表"))
                })?
                .clone(),
        };
        let mut allowed_paths = Vec::with_capacity(raw_allowed.len());
        for ap in raw_allowed {
            let s = ap.as_str().ok_or_else(|| {
                WbError::validation(format!("交付项 {item_id:?} 的 allowed_paths 元素必须为字符串"))
            })?;
            allowed_paths.push(Value::String(validate_relative_subpath(
                s,
                &format!("items[{item_id}].allowed_paths"),
            )?));
        }

        let raw_deps = match item.get("depends_on") {
            None => Vec::new(),
            Some(v) => v
                .as_array()
                .ok_or_else(|| {
                    WbError::validation(format!("交付项 {item_id:?} 的 depends_on 必须为列表"))
                })?
                .clone(),
        };
        let mut depends_on: Vec<String> = Vec::new();
        for dep in raw_deps {
            let s = dep
                .as_str()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| {
                    WbError::validation(format!("交付项 {item_id:?} 的 depends_on 包含空值或非字符串"))
                })?
                .to_string();
            if !depends_on.contains(&s) {
                depends_on.push(s);
            }
        }

        let mut norm_item = Map::new();
        norm_item.insert("id".to_string(), Value::String(item_id));
        norm_item.insert("repo".to_string(), Value::String(repo));
        norm_item.insert("worktree".to_string(), worktree);
        norm_item.insert("source_branch".to_string(), Value::String(src_branch));
        norm_item.insert("target_branch".to_string(), Value::String(tgt_branch));
        norm_item.insert("compare_base".to_string(), compare_base);
        norm_item.insert("allowed_paths".to_string(), Value::Array(allowed_paths));
        norm_item.insert(
            "depends_on".to_string(),
            Value::Array(depends_on.into_iter().map(Value::String).collect()),
        );
        normalized_items.push(Value::Object(norm_item));
    }

    // Validate depends_on targets and detect cycles via DFS
    let mut deps_map: HashMap<String, Vec<String>> = HashMap::new();
    for item_val in &normalized_items {
        let obj = item_val.as_object().unwrap();
        let id = obj["id"].as_str().unwrap().to_string();
        let deps: Vec<String> = obj["depends_on"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        deps_map.insert(id, deps);
    }

    for (id, deps) in &deps_map {
        for dep in deps {
            if !deps_map.contains_key(dep) {
                return Err(WbError::validation(format!(
                    "交付项 {id:?} 依赖的前置项 {dep:?} 不存在"
                )));
            }
        }
    }

    let mut visit_state: HashMap<String, u8> = HashMap::new();
    fn dfs(
        node: &str,
        deps_map: &HashMap<String, Vec<String>>,
        state: &mut HashMap<String, u8>,
        trail: &mut Vec<String>,
    ) -> Result<(), WbError> {
        let mark = *state.get(node).unwrap_or(&0);
        if mark == 1 {
            let pos = trail.iter().position(|n| n == node).unwrap_or(0);
            let mut cycle = trail[pos..].to_vec();
            cycle.push(node.to_string());
            return Err(WbError::validation(format!(
                "检测到交付项存在循环前置依赖: {}",
                cycle.join(" -> ")
            )));
        }
        if mark == 2 {
            return Ok(());
        }
        state.insert(node.to_string(), 1);
        trail.push(node.to_string());
        if let Some(deps) = deps_map.get(node) {
            for nxt in deps {
                dfs(nxt, deps_map, state, trail)?;
            }
        }
        trail.pop();
        state.insert(node.to_string(), 2);
        Ok(())
    }

    let mut keys: Vec<String> = deps_map.keys().cloned().collect();
    keys.sort();
    let mut trail = Vec::new();
    for k in keys {
        if *visit_state.get(&k).unwrap_or(&0) == 0 {
            dfs(&k, &deps_map, &mut visit_state, &mut trail)?;
        }
    }

    let actions_raw = match obj.get("actions") {
        None => Vec::new(),
        Some(v) => v
            .as_array()
            .ok_or_else(|| WbError::validation("delivery.actions 必须为列表"))?
            .clone(),
    };
    let mut normalized_actions: Vec<Value> = Vec::with_capacity(actions_raw.len());
    let mut seen_action_ids: HashSet<String> = HashSet::new();

    for (idx, act_val) in actions_raw.iter().enumerate() {
        let act = act_val
            .as_object()
            .ok_or_else(|| WbError::validation(format!("delivery.actions[{idx}] 必须为字典")))?;
        let act_id = act
            .get("id")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| WbError::validation(format!("delivery.actions[{idx}] 缺少有效 id")))?
            .to_string();
        if !seen_action_ids.insert(act_id.clone()) {
            return Err(WbError::validation(format!(
                "delivery.actions 存在重复 id: {act_id:?}"
            )));
        }

        let kind = act
            .get("kind")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if !VALID_ACTION_KINDS.contains(&kind) {
            return Err(WbError::validation(format!(
                "动作 {act_id:?} 的 kind={kind:?} 非法，可选值: {}",
                VALID_ACTION_KINDS.join(", ")
            )));
        }
        let title = act
            .get("title")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| WbError::validation(format!("动作 {act_id:?} 缺少有效 title")))?
            .to_string();

        let item_id: Value = match act.get("item_id") {
            None | Some(Value::Null) => Value::Null,
            Some(v) => {
                let s = v.as_str().map(str::trim).unwrap_or("");
                if !seen_ids.contains(s) {
                    return Err(WbError::validation(format!(
                        "动作 {act_id:?} 关联的交付项 {s:?} 不存在"
                    )));
                }
                Value::String(s.to_string())
            }
        };

        let scope = act
            .get("scope")
            .cloned()
            .unwrap_or_else(|| Value::Object(Map::new()));
        let required = act.get("required").and_then(|v| v.as_bool()).unwrap_or(true);

        let mut norm_act = Map::new();
        norm_act.insert("id".to_string(), Value::String(act_id));
        norm_act.insert("kind".to_string(), Value::String(kind.to_string()));
        norm_act.insert("title".to_string(), Value::String(title));
        norm_act.insert("item_id".to_string(), item_id);
        norm_act.insert("scope".to_string(), scope);
        norm_act.insert("required".to_string(), Value::Bool(required));
        let digest = compute_action_scope_sha256(&norm_act);
        norm_act.insert("scope_sha256".to_string(), Value::String(digest));
        normalized_actions.push(Value::Object(norm_act));
    }

    let rev = obj
        .get("revision")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let notes = obj
        .get("notes")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .unwrap_or_default();

    let mut out = Map::new();
    out.insert("schema".to_string(), Value::from(SCHEMA_DELIVERY));
    out.insert("revision".to_string(), Value::from(rev));
    out.insert("items".to_string(), Value::Array(normalized_items));
    out.insert("actions".to_string(), Value::Array(normalized_actions));
    out.insert("notes".to_string(), Value::String(notes));
    if let Some(ts) = obj.get("updated_at") {
        out.insert("updated_at".to_string(), ts.clone());
    }
    Ok(Value::Object(out))
}

pub fn validate_task_dict(data: &Value) -> Result<Value, WbError> {
    let obj = data
        .as_object()
        .ok_or_else(|| WbError::validation("task.yaml 顶层必须为字典"))?;
    let task_id = validate_task_id(obj.get("id").and_then(|v| v.as_str()).unwrap_or(""))?;
    let title = obj
        .get("title")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| WbError::validation(format!("任务 {task_id} 缺少有效 title")))?
        .to_string();

    let status = validate_status(
        obj.get("status").and_then(|v| v.as_str()).unwrap_or("planning"),
    )?;
    let priority = validate_priority(
        obj.get("priority").and_then(|v| v.as_str()).unwrap_or("P2"),
    )?;
    let category = validate_category(
        obj.get("category").and_then(|v| v.as_str()).unwrap_or("unclassified"),
    )?;

    let mut normalized = obj.clone();
    normalized.insert("id".to_string(), Value::String(task_id.clone()));
    normalized.insert("title".to_string(), Value::String(title));
    normalized.insert("status".to_string(), Value::String(status));
    normalized.insert("priority".to_string(), Value::String(priority));
    normalized.insert("category".to_string(), Value::String(category));

    for list_field in [
        "components",
        "milestones",
        "blockers",
        "next_steps",
        "docs",
        "materials",
        "links",
    ] {
        match normalized.get(list_field) {
            None => {
                normalized.insert(list_field.to_string(), Value::Array(Vec::new()));
            }
            Some(v) if v.is_array() => {}
            _ => {
                return Err(WbError::validation(format!(
                    "任务 {task_id} 字段 {list_field} 必须为列表"
                )));
            }
        }
    }

    if let Some(sessions) = normalized.get("sessions") {
        if !sessions.is_null() && !sessions.is_object() {
            return Err(WbError::validation(format!(
                "任务 {task_id} 字段 sessions 必须为字典"
            )));
        }
    }

    Ok(Value::Object(normalized))
}
