use std::collections::HashMap;
use std::fs;
use std::path::Path;

use serde_json::{Map, Value};

use super::delivery::load_delivery;
use super::errors::WbError;
use super::evidence::evaluate_actions_status;
use super::sync::load_observation;
use super::task_store::{load_task, require_task_dir};

pub fn build_context_data(ws_root: &Path, task_id: &str) -> Result<Value, WbError> {
    let tdir = require_task_dir(ws_root, task_id)?;
    let task = load_task(ws_root, task_id)?;
    let delivery = load_delivery(ws_root, task_id, false)?;
    let actions = evaluate_actions_status(ws_root, task_id)?;
    let observation = load_observation(ws_root, task_id)?;

    let mut obs_by_item: Map<String, Value> = Map::new();
    if let Some(ref obs) = observation {
        if let Some(items) = obs.get("items").and_then(|v| v.as_array()) {
            for item_obs in items {
                if let Some(id) = item_obs.get("id").and_then(|v| v.as_str()) {
                    obs_by_item.insert(id.to_string(), item_obs.clone());
                }
            }
        }
    }

    let mut inconsistencies: Vec<String> = Vec::new();
    for (item_id, item_obs) in &obs_by_item {
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
            inconsistencies.push(format!("分支不一致 [{item_id}]: 声明={decl} vs 实际={actual}"));
        }
        if let Some(oos) = item_obs.get("out_of_scope_paths").and_then(|v| v.as_array()) {
            if !oos.is_empty() {
                let paths: Vec<&str> = oos.iter().filter_map(|v| v.as_str()).collect();
                inconsistencies.push(format!("越界修改 [{item_id}]: {}", paths.join(", ")));
            }
        }
        if let Some(st) = item_obs.get("status").and_then(|v| v.as_str()) {
            if st == "repo_missing" || st == "not_a_git_repo" {
                let err = item_obs.get("error").and_then(|v| v.as_str()).unwrap_or(st);
                inconsistencies.push(format!("仓库异常 [{item_id}]: {err}"));
            }
        }
    }

    for act in &actions {
        let st = act.get("status").and_then(|v| v.as_str()).unwrap_or("");
        if matches!(st, "stale" | "missing_receipt" | "invalid_receipt") {
            let aid = act.get("id").and_then(|v| v.as_str()).unwrap_or("");
            let reason = act.get("reason").and_then(|v| v.as_str()).unwrap_or("");
            inconsistencies.push(format!("证据异常 [{aid}]: {reason}"));
        }
    }

    let blockers = task
        .get("blockers")
        .cloned()
        .unwrap_or_else(|| Value::Array(Vec::new()));
    let next_steps_top3: Vec<Value> = task
        .get("next_steps")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().take(3).cloned().collect())
        .unwrap_or_default();

    let mut out = Map::new();
    out.insert("task".into(), task);
    out.insert("delivery".into(), delivery.unwrap_or(Value::Null));
    out.insert("actions".into(), Value::Array(actions));
    out.insert("observation".into(), observation.unwrap_or(Value::Null));
    out.insert("obs_by_item".into(), Value::Object(obs_by_item));
    out.insert("blockers".into(), blockers);
    out.insert(
        "inconsistencies".into(),
        Value::Array(inconsistencies.into_iter().map(Value::String).collect()),
    );
    out.insert("next_steps_top3".into(), Value::Array(next_steps_top3));
    out.insert(
        "progress_path".into(),
        Value::String(tdir.join("progress.md").to_string_lossy().into_owned()),
    );
    Ok(Value::Object(out))
}

pub fn format_context_plain(ctx: &Value, max_lines: usize) -> String {
    let task = ctx.get("task").and_then(|v| v.as_object()).unwrap();
    let tid = task.get("id").and_then(|v| v.as_str()).unwrap_or("");
    let title = task.get("title").and_then(|v| v.as_str()).unwrap_or("");
    let status = task.get("status").and_then(|v| v.as_str()).unwrap_or("");
    let priority = task.get("priority").and_then(|v| v.as_str()).unwrap_or("");
    let category = task.get("category").and_then(|v| v.as_str()).unwrap_or("");
    let owner = task
        .get("owner")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("-");
    let goal = task
        .get("goal")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("（未填写）");
    let updated_at = task.get("updated_at").and_then(|v| v.as_str()).unwrap_or("-");

    let header_lines = vec![
        format!("=== 任务交接简报: {tid} ==="),
        format!("标题: {title}"),
        format!("状态: {status} | 优先级: {priority} | 主类: {category} | 负责人: {owner}"),
        format!("目标: {goal}"),
        format!("更新时间: {updated_at}"),
    ];

    let obs_by_item: HashMap<String, &Value> = ctx
        .get("obs_by_item")
        .and_then(|v| v.as_object())
        .map(|m| m.iter().map(|(k, v)| (k.clone(), v)).collect())
        .unwrap_or_default();

    let mut delivery_lines = vec![String::new(), "[交付项与仓库观察]".to_string()];
    let deliv = ctx.get("delivery").and_then(|v| v.as_object());
    let deliv_items = deliv
        .and_then(|d| d.get("items"))
        .and_then(|v| v.as_array())
        .filter(|arr| !arr.is_empty());

    if let Some(items) = deliv_items {
        let rev = deliv
            .and_then(|d| d.get("revision"))
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        delivery_lines.push(format!("交付修订号: rev-{rev}"));
        for item in items {
            let iid = item.get("id").and_then(|v| v.as_str()).unwrap_or("");
            let repo = item.get("repo").and_then(|v| v.as_str()).unwrap_or("");
            let src = item.get("source_branch").and_then(|v| v.as_str()).unwrap_or("");
            let tgt = item.get("target_branch").and_then(|v| v.as_str()).unwrap_or("");
            let obs_desc = if let Some(obs) = obs_by_item.get(iid) {
                let st = obs.get("status").and_then(|v| v.as_str()).unwrap_or("");
                if st == "git_unavailable" {
                    "Git不可用".to_string()
                } else if let Some(ab) = obs.get("actual_branch").and_then(|v| v.as_str()) {
                    let hc = obs.get("head_commit").and_then(|v| v.as_str()).unwrap_or("");
                    let short_hc: String = hc.chars().take(12).collect();
                    let dirty = obs
                        .get("dirty_file_count")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);
                    format!("实际分支={ab}, head={short_hc}, 未保存={dirty}")
                } else {
                    format!("状态={st}")
                }
            } else {
                "未同步".to_string()
            };
            let deps = item
                .get("depends_on")
                .and_then(|v| v.as_array())
                .filter(|arr| !arr.is_empty())
                .map(|arr| {
                    let ds: Vec<&str> = arr.iter().filter_map(|x| x.as_str()).collect();
                    format!(" depends_on={ds:?}")
                })
                .unwrap_or_default();
            delivery_lines.push(format!("- {iid} ({repo}): {src} -> {tgt}{deps} | {obs_desc}"));
        }
    } else {
        delivery_lines.push("- （尚未配置交付项）".to_string());
    }

    let mut evidence_lines = vec![String::new(), "[证据与动作确认状态]".to_string()];
    let actions = ctx
        .get("actions")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    if actions.is_empty() {
        evidence_lines.push("- （无已声明的交付动作）".to_string());
    } else {
        for act in actions {
            let valid = act.get("valid").and_then(|v| v.as_bool()).unwrap_or(false);
            let st = act.get("status").and_then(|v| v.as_str()).unwrap_or("");
            let flag = if valid {
                "VALID".to_string()
            } else {
                st.to_ascii_uppercase()
            };
            let aid = act.get("id").and_then(|v| v.as_str()).unwrap_or("");
            let kind = act.get("kind").and_then(|v| v.as_str()).unwrap_or("");
            let atitle = act.get("title").and_then(|v| v.as_str()).unwrap_or("");
            let reason = act.get("reason").and_then(|v| v.as_str()).unwrap_or("");
            evidence_lines.push(format!("- {aid} [{kind}] {atitle}: {flag} ({reason})"));
        }
    }

    let mut issue_lines = vec![String::new(), "[阻塞与不一致项]".to_string()];
    let blockers = ctx
        .get("blockers")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let inconsistencies = ctx
        .get("inconsistencies")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    if blockers.is_empty() && inconsistencies.is_empty() {
        issue_lines.push("- （无阻塞与不一致项）".to_string());
    } else {
        for b in blockers {
            issue_lines.push(format!("- [阻塞] {}", b.as_str().unwrap_or("")));
        }
        for inc in inconsistencies {
            issue_lines.push(format!("- [不一致] {}", inc.as_str().unwrap_or("")));
        }
    }

    let mut next_lines = vec![String::new(), "[下一步动作（最多3项）]".to_string()];
    let next_steps = ctx
        .get("next_steps_top3")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    if next_steps.is_empty() {
        next_lines.push("- （暂无下一步动作）".to_string());
    } else {
        for (idx, step) in next_steps.iter().enumerate() {
            next_lines.push(format!("{}. {}", idx + 1, step.as_str().unwrap_or("")));
        }
    }

    let mut all_lines = Vec::new();
    all_lines.extend(header_lines.clone());
    all_lines.extend(delivery_lines.clone());
    all_lines.extend(evidence_lines.clone());
    all_lines.extend(issue_lines.clone());
    all_lines.extend(next_lines.clone());

    if all_lines.len() <= max_lines {
        let mut out = all_lines.join("\n");
        out.push('\n');
        return out;
    }

    let mut fixed_tail = Vec::new();
    fixed_tail.extend(issue_lines);
    fixed_tail.extend(next_lines);

    let budget = max_lines
        .saturating_sub(header_lines.len())
        .saturating_sub(fixed_tail.len())
        .saturating_sub(1);

    let mut middle = delivery_lines;
    middle.extend(evidence_lines);

    let mut trimmed = Vec::new();
    if budget > 0 {
        trimmed.extend(header_lines);
        trimmed.extend(middle.into_iter().take(budget));
        trimmed.push("...（部分条目已折叠，使用 --full 查看完整信息）".to_string());
        trimmed.extend(fixed_tail);
    } else {
        trimmed.extend(header_lines);
        trimmed.extend(fixed_tail);
    }
    trimmed.truncate(max_lines);
    let mut out = trimmed.join("\n");
    out.push('\n');
    out
}

pub fn format_context_full(ws_root: &Path, ctx: &Value) -> String {
    let task = ctx.get("task").and_then(|v| v.as_object()).unwrap();
    let tid = task.get("id").and_then(|v| v.as_str()).unwrap_or("");
    let base = format_context_plain(ctx, 10000);
    let mut extra = vec![base.trim_end_matches('\n').to_string()];

    extra.push(String::new());
    extra.push("[里程碑与标签]".to_string());
    let comps: Vec<&str> = task
        .get("components")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_str()).collect())
        .unwrap_or_default();
    extra.push(format!(
        "标签: {}",
        if comps.is_empty() {
            "（无）".to_string()
        } else {
            comps.join(", ")
        }
    ));

    if let Some(ms_arr) = task.get("milestones").and_then(|v| v.as_array()) {
        for ms in ms_arr {
            if let Some(obj) = ms.as_object() {
                let mark = if obj.get("done").and_then(|v| v.as_bool()).unwrap_or(false) {
                    "x"
                } else {
                    " "
                };
                extra.push(format!(
                    "- [{mark}] {}",
                    obj.get("title").and_then(|v| v.as_str()).unwrap_or("")
                ));
            } else {
                extra.push(format!("- [ ] {}", ms.as_str().unwrap_or("")));
            }
        }
    }

    extra.push(String::new());
    extra.push("[关联文档与材料]".to_string());
    for (label, key) in [("文档", "docs"), ("材料", "materials"), ("链接", "links")] {
        let vals: Vec<&str> = task
            .get(key)
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|x| x.as_str()).collect())
            .unwrap_or_default();
        extra.push(format!(
            "{label}: {}",
            if vals.is_empty() {
                "（无）".to_string()
            } else {
                vals.join(", ")
            }
        ));
    }

    extra.push(String::new());
    extra.push("[来源会话]".to_string());
    if let Some(sessions) = task.get("sessions").and_then(|v| v.as_object()) {
        if let Some(origin) = sessions.get("origin").and_then(|v| v.as_object()) {
            extra.push(format!(
                "- [origin] {}@{} ({}): {}",
                origin.get("session_id").and_then(|v| v.as_str()).unwrap_or(""),
                origin.get("host").and_then(|v| v.as_str()).unwrap_or(""),
                origin.get("category").and_then(|v| v.as_str()).unwrap_or(""),
                origin.get("title").and_then(|v| v.as_str()).unwrap_or(""),
            ));
        }
        if let Some(assocs) = sessions.get("associated").and_then(|v| v.as_array()) {
            for a in assocs {
                if let Some(obj) = a.as_object() {
                    extra.push(format!(
                        "- [associated] {}@{} ({}): {}",
                        obj.get("session_id").and_then(|v| v.as_str()).unwrap_or(""),
                        obj.get("host").and_then(|v| v.as_str()).unwrap_or(""),
                        obj.get("category").and_then(|v| v.as_str()).unwrap_or(""),
                        obj.get("title").and_then(|v| v.as_str()).unwrap_or(""),
                    ));
                }
            }
        }
    }

    extra.push(String::new());
    extra.push("[全部下一步]".to_string());
    if let Some(steps) = task.get("next_steps").and_then(|v| v.as_array()) {
        for (idx, step) in steps.iter().enumerate() {
            extra.push(format!("{}. {}", idx + 1, step.as_str().unwrap_or("")));
        }
    }

    let prog_path = ws_root.join("tasks").join(tid).join("progress.md");
    if let Ok(raw) = fs::read_to_string(&prog_path) {
        let lines: Vec<&str> = raw.lines().collect();
        let start = lines.len().saturating_sub(30);
        extra.push(String::new());
        extra.push("[最近进展（尾部最多30行）]".to_string());
        for ln in &lines[start..] {
            extra.push((*ln).to_string());
        }
    }

    let mut out = extra.join("\n");
    out.push('\n');
    out
}
