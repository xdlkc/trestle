use std::path::Path;

use serde_json::{Map, Value};

use super::errors::WbError;
use super::io_utils::{atomic_write_text, ensure_within_workspace, to_workspace_relpath};
use super::task_store::{add_task_link, load_task, manage_material};

pub const REVIEW_DIMENSIONS: &[(&str, &str)] = &[
    (
        "1. 功能与范围",
        "核对改动是否完整覆盖目标需求，是否存在范围蔓延或无关修改。",
    ),
    (
        "2. 问题与解决效果",
        "评估根因定位是否准确，方案是否从根本上解决问题而非掩盖表象。",
    ),
    (
        "3. 兼容性",
        "检查接口、配置、数据格式、旧版本客户端或历史数据的向后兼容性。",
    ),
    (
        "4. 稳定性",
        "检查并发安全、锁粒度、幂等性、资源释放、超时与重试退避行为。",
    ),
    (
        "5. 性能与成本",
        "评估时间/空间复杂度、I/O 放大、内存峰值及潜在吞吐瓶颈。",
    ),
    (
        "6. 异常防御",
        "检查边界条件、空值/非法输入校验、错误传播与失败回滚路径。",
    ),
    (
        "7. 职责边界",
        "核对模块划分是否清晰，是否存在跨层耦合或重复逻辑。",
    ),
    (
        "8. 验证充分性",
        "审查单元/集成测试覆盖、异常路径验证及回归验证证据是否真实充分。",
    ),
];

pub fn derive_review_artifact_paths(
    ws_root: &Path,
    report_path: &str,
) -> Result<(String, String, String), WbError> {
    if report_path.trim().is_empty() {
        return Err(WbError::validation("--report 不能为空"));
    }
    let resolved_report = ensure_within_workspace(ws_root, Path::new(report_path))?;
    if resolved_report.is_dir() {
        return Err(WbError::validation(format!(
            "--report 必须是文件路径而非目录: {report_path}"
        )));
    }

    let stem = resolved_report
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "review".to_string());
    let suffix = resolved_report
        .extension()
        .map(|s| format!(".{}", s.to_string_lossy()))
        .unwrap_or_else(|| ".md".to_string());
    let parent = resolved_report.parent().unwrap_or(ws_root);

    let summary_path =
        ensure_within_workspace(ws_root, &parent.join(format!("{stem}.summary{suffix}")))?;
    let comments_path =
        ensure_within_workspace(ws_root, &parent.join(format!("{stem}.comments{suffix}")))?;

    Ok((
        to_workspace_relpath(ws_root, &resolved_report)?,
        to_workspace_relpath(ws_root, &summary_path)?,
        to_workspace_relpath(ws_root, &comments_path)?,
    ))
}

pub fn build_review_prompt(
    task: &Value,
    url: &str,
    report_rel: &str,
    summary_rel: &str,
    comments_rel: &str,
) -> String {
    let tid = task.get("id").and_then(|v| v.as_str()).unwrap_or("");
    let title = task.get("title").and_then(|v| v.as_str()).unwrap_or("");
    let goal = task
        .get("goal")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("（未填写）");

    let mut lines = vec![
        format!("# 代码评审提示词（任务 {tid}）"),
        String::new(),
        format!("- 任务标题：{title}"),
        format!("- 评审链接：{url}"),
        format!("- 任务目标：{goal}"),
        String::new(),
        "## 一、工程评审八大维度".to_string(),
    ];
    for (dim, desc) in REVIEW_DIMENSIONS {
        lines.push(format!("- **{dim}**：{desc}"));
    }
    lines.extend([
        String::new(),
        "## 二、三份结论一致的交付物要求".to_string(),
        format!("1. **详细分析报告**：`{report_rel}`（逐维度展开证据、风险等级与具体代码位置）"),
        format!("2. **一屏摘要**：`{summary_rel}`（总览结论、阻塞项与关键修改建议，控制在 1 屏内）"),
        format!("3. **待发评论草稿**：`{comments_rel}`（可直接人工核对后发布的逐条评审意见）"),
        String::new(),
        "> 约束：三份文件的总体评审结论（通过 / 需修改 / 阻塞）及关键问题清单必须保持严格一致；本准备命令不自动向外发送评论，也不修改代码仓库。".to_string(),
    ]);
    let mut out = lines.join("\n");
    out.push('\n');
    out
}

pub fn prepare_review(
    ws_root: &Path,
    task_id: &str,
    url: &str,
    report_path: &str,
) -> Result<Value, WbError> {
    let clean_url = url.trim();
    if !(clean_url.starts_with("http://") || clean_url.starts_with("https://")) {
        return Err(WbError::validation(format!(
            "评审链接必须为 http(s) URL: {url:?}"
        )));
    }

    let task = load_task(ws_root, task_id)?;
    let (report_rel, summary_rel, comments_rel) =
        derive_review_artifact_paths(ws_root, report_path)?;

    let stubs = [
        (
            report_rel.as_str(),
            format!("# 代码评审详细分析：{task_id}\n\n- 评审链接：{clean_url}\n\n## 八维评审记录\n"),
        ),
        (
            summary_rel.as_str(),
            format!("# 代码评审一屏摘要：{task_id}\n\n- 评审链接：{clean_url}\n- 总体结论：（待填写）\n"),
        ),
        (
            comments_rel.as_str(),
            format!("# 待发评论草稿：{task_id}\n\n- 评审链接：{clean_url}\n\n## 行级/全局评论草稿\n"),
        ),
    ];

    for (rel, body) in stubs {
        let full = ensure_within_workspace(ws_root, Path::new(rel))?;
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| WbError::io(format!("创建评审目录失败: {e}")))?;
        }
        if !full.exists() {
            atomic_write_text(&full, &body)?;
        }
    }

    add_task_link(ws_root, task_id, clean_url)?;
    manage_material(ws_root, task_id, "add", &report_rel)?;
    manage_material(ws_root, task_id, "add", &summary_rel)?;
    manage_material(ws_root, task_id, "add", &comments_rel)?;

    let prompt = build_review_prompt(&task, clean_url, &report_rel, &summary_rel, &comments_rel);

    let mut artifacts = Map::new();
    artifacts.insert("report".into(), Value::String(report_rel));
    artifacts.insert("summary".into(), Value::String(summary_rel));
    artifacts.insert("comments".into(), Value::String(comments_rel));

    let mut res = Map::new();
    res.insert("task_id".into(), Value::String(task_id.to_string()));
    res.insert("url".into(), Value::String(clean_url.to_string()));
    res.insert("artifacts".into(), Value::Object(artifacts));
    res.insert(
        "dimensions".into(),
        Value::Array(
            REVIEW_DIMENSIONS
                .iter()
                .map(|(t, _)| Value::String((*t).to_string()))
                .collect(),
        ),
    );
    res.insert("prompt".into(), Value::String(prompt));
    Ok(Value::Object(res))
}
