use std::fs;
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};

use crate::core::delivery::apply_delivery;
use crate::core::errors::WbError;
use crate::core::evidence::confirm_action;
use crate::core::io_utils::{atomic_write_json, atomic_write_text, atomic_write_yaml};
use crate::core::models::SCHEMA_SESSION_TYPES;
use crate::core::task_store::{
    append_progress, create_task, manage_components, manage_doc, manage_milestones,
    manage_next_steps, rebuild_index, update_task_status, CreateTaskOpts,
};

use super::config::{
    validate_workspace_config, DEFAULT_DASHBOARD_PORT, DEFAULT_REPOS_ROOT, DEFAULT_TIMEZONE,
};

pub const TEMPLATE_WORKBENCH_MD: &str = include_str!("../templates/WORKBENCH.md");
pub const TEMPLATE_TASKS_README: &str = include_str!("../templates/tasks_readme.md");
pub const EXAMPLE_TASK_ID: &str = "TASK-EXAMPLE-001";

pub const WORKBENCH_GITIGNORE: &str = "# Personal Workbench runtime artifacts
.wb-runtime/
tasks/*/.lock
";

pub const AGENTS_GUIDE_TEMPLATE: &str = "# AGENTS.md

请先阅读本工作台的通用协作规范：[WORKBENCH.md](./WORKBENCH.md)。

- 使用 `wb context <task-id> --plain` 获取秒级交接简报。
- 使用 `wb sync <task-id>` 只读采集外部代码仓库状态。
- 严禁手工编辑 `tasks/INDEX.md`，严禁在本目录复制外部业务源码。
";

fn prompt_with_default(prompt: &str, default: &str) -> String {
    print!("{prompt} [{default}]: ");
    let _ = io::stdout().flush();
    let mut input = String::new();
    match io::stdin().read_line(&mut input) {
        Ok(0) | Err(_) => default.to_string(),
        Ok(_) => {
            let trimmed = input.trim();
            if trimmed.is_empty() {
                default.to_string()
            } else {
                trimmed.to_string()
            }
        }
    }
}

pub fn create_example_task(ws_root: &Path) -> Result<bool, WbError> {
    let tdir = ws_root.join("tasks").join(EXAMPLE_TASK_ID);
    if tdir.join("task.yaml").is_file() {
        return Ok(false);
    }

    let doc_path = ws_root.join("docs").join("example-design.md");
    if !doc_path.exists() {
        atomic_write_text(
            &doc_path,
            "# 示例任务方案说明\n\n这是一个由 `wb init --example` 生成的合成示例文档，仅用于演示工作台目录结构。\n",
        )?;
    }

    let comps = vec!["cli".to_string(), "docs".to_string()];
    create_task(
        ws_root,
        CreateTaskOpts {
            task_id: EXAMPLE_TASK_ID,
            title: "[示例] 通用工作台任务交接演示",
            goal: "演示任务登记、交付声明、动作确认回执与 80 行交接简报的完整闭环。",
            status: "planning",
            priority: "P1",
            owner: "workbench-user",
            category: "requirement",
            components: &comps,
            origin_session_id: Some("example-session-001"),
            origin_host: Some("localhost"),
            origin_title: Some("[FD] 示例任务初始化会话"),
            origin_category: Some("requirement"),
            timestamp: None,
        },
    )?;

    update_task_status(ws_root, EXAMPLE_TASK_ID, "in_progress", Some("开始示例演练"))?;
    append_progress(ws_root, EXAMPLE_TASK_ID, "完成基础设计文档与交付项初始化")?;
    manage_milestones(ws_root, EXAMPLE_TASK_ID, "add", "完成方案说明", true)?;
    manage_milestones(ws_root, EXAMPLE_TASK_ID, "add", "完成自动检查确认", false)?;
    manage_components(ws_root, EXAMPLE_TASK_ID, "add", "example")?;
    manage_doc(ws_root, EXAMPLE_TASK_ID, "add", "docs/example-design.md")?;
    manage_next_steps(
        ws_root,
        EXAMPLE_TASK_ID,
        "add",
        "运行 `wb context TASK-EXAMPLE-001 --plain` 查看交接简报",
        None,
    )?;
    manage_next_steps(
        ws_root,
        EXAMPLE_TASK_ID,
        "add",
        "运行 `wb doctor` 检查工作台健康状态",
        None,
    )?;

    let delivery_payload = json!({
        "items": [
            {
                "id": "item-core",
                "repo": "sample-service",
                "source_branch": "feat/example-handoff",
                "target_branch": "main",
                "allowed_paths": ["src/", "tests/"],
                "depends_on": []
            }
        ],
        "actions": [
            {
                "id": "act-unit-tests",
                "kind": "auto_check",
                "title": "离线单元测试通过",
                "item_id": "item-core",
                "scope": {"command": "cargo test --offline", "paths": ["tests/"]}
            },
            {
                "id": "act-manual-review",
                "kind": "manual_review",
                "title": "人工核对交付范围",
                "item_id": "item-core",
                "scope": {"checklist": ["范围无越界", "回执已归档"]}
            }
        ],
        "notes": "示例交付声明"
    });
    let delivery = apply_delivery(ws_root, EXAMPLE_TASK_ID, &delivery_payload, None)?;

    let first_action = &delivery["actions"].as_array().unwrap()[0];
    let action_id = first_action["id"].as_str().unwrap();
    let digest = first_action["scope_sha256"].as_str().unwrap();
    confirm_action(
        ws_root,
        EXAMPLE_TASK_ID,
        action_id,
        "workbench-user",
        "example-bootstrap",
        digest,
        "示例初始化自动签发回执",
    )?;
    Ok(true)
}

#[derive(Debug, Default)]
pub struct InitOpts<'a> {
    pub target_path: &'a str,
    pub name: Option<&'a str>,
    pub repos_root: Option<&'a str>,
    pub timezone: Option<&'a str>,
    pub port: Option<i64>,
    pub yes: bool,
    pub dry_run: bool,
    pub example: bool,
    pub agent_guide: bool,
}

fn absolutize(path: &Path) -> PathBuf {
    if path.exists() {
        fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
    } else if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    }
}

pub fn init_workbench(opts: InitOpts<'_>) -> Result<Value, WbError> {
    let raw_target = Path::new(if opts.target_path.is_empty() {
        "."
    } else {
        opts.target_path
    });
    let ws_root = absolutize(raw_target);

    let fallback_name = ws_root
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "my-workbench".to_string());

    let mut chosen_name = opts.name.map(str::to_string).unwrap_or(fallback_name);
    let mut chosen_repos = opts
        .repos_root
        .map(str::to_string)
        .unwrap_or_else(|| DEFAULT_REPOS_ROOT.to_string());
    let mut chosen_tz = opts
        .timezone
        .map(str::to_string)
        .unwrap_or_else(|| DEFAULT_TIMEZONE.to_string());
    let mut chosen_port = opts.port.unwrap_or(DEFAULT_DASHBOARD_PORT);

    if !opts.yes && !opts.dry_run && io::stdin().is_terminal() {
        if opts.name.is_none() {
            chosen_name = prompt_with_default("工作台名称", &chosen_name);
        }
        if opts.repos_root.is_none() {
            chosen_repos = prompt_with_default("外部代码根目录 (repos_root)", &chosen_repos);
        }
        if opts.timezone.is_none() {
            chosen_tz = prompt_with_default("时区", &chosen_tz);
        }
        if opts.port.is_none() {
            let raw_port = prompt_with_default("本地看板默认端口", &chosen_port.to_string());
            chosen_port = raw_port
                .parse::<i64>()
                .map_err(|_| WbError::validation(format!("端口必须为整数: {raw_port:?}")))?;
        }
    }

    let cfg = validate_workspace_config(&json!({
        "schema": 1,
        "name": chosen_name,
        "repos_root": chosen_repos,
        "timezone": chosen_tz,
        "dashboard_port": chosen_port,
        "extensions": [],
    }))?;

    let planned_dirs = vec![
        ws_root.clone(),
        ws_root.join("tasks"),
        ws_root.join("docs"),
        ws_root.join("analysis"),
        ws_root.join(".wb-runtime"),
        ws_root.join(".wb-runtime").join("locks"),
        ws_root.join(".wb-runtime").join("observations"),
        ws_root.join(".wb-runtime").join("dashboard"),
        ws_root.join(".wb-runtime").join("backups"),
    ];

    let mut file_specs: Vec<(PathBuf, &str)> = vec![
        (ws_root.join(".wb.yml"), "config"),
        (ws_root.join(".gitignore"), "gitignore"),
        (ws_root.join("WORKBENCH.md"), "workbench_md"),
        (ws_root.join("tasks").join("README.md"), "tasks_readme"),
        (ws_root.join("tasks").join("session-types.json"), "session_types"),
    ];
    if opts.agent_guide {
        file_specs.push((ws_root.join("AGENTS.md"), "agents_md"));
    }

    let mut plan: Vec<Value> = Vec::new();
    for d in &planned_dirs {
        let act = if d.exists() { "keep_dir" } else { "create_dir" };
        plan.push(json!({"action": act, "path": d.to_string_lossy()}));
    }
    for (fpath, kind) in &file_specs {
        let act = if fpath.exists() {
            "preserve_existing".to_string()
        } else {
            format!("create_{kind}")
        };
        plan.push(json!({"action": act, "path": fpath.to_string_lossy()}));
    }
    let index_path = ws_root.join("tasks").join("INDEX.md");
    plan.push(json!({
        "action": if index_path.exists() { "preserve_existing" } else { "generate_index" },
        "path": index_path.to_string_lossy()
    }));
    if opts.example {
        let ex_task = ws_root.join("tasks").join(EXAMPLE_TASK_ID).join("task.yaml");
        plan.push(json!({
            "action": if ex_task.exists() { "preserve_existing" } else { "create_example_task" },
            "path": ex_task.to_string_lossy()
        }));
    }

    if opts.dry_run {
        return Ok(json!({
            "workspace": ws_root.to_string_lossy(),
            "dry_run": true,
            "config": cfg,
            "plan": plan,
        }));
    }

    for d in &planned_dirs {
        fs::create_dir_all(d)
            .map_err(|e| WbError::io(format!("无法创建目录 {}: {e}", d.display())))?;
    }
    let ws_canon = fs::canonicalize(&ws_root).unwrap_or(ws_root);

    if !ws_canon.join(".wb.yml").exists() {
        atomic_write_yaml(&ws_canon.join(".wb.yml"), &cfg)?;
    }
    if !ws_canon.join(".gitignore").exists() {
        atomic_write_text(&ws_canon.join(".gitignore"), WORKBENCH_GITIGNORE)?;
    }
    if !ws_canon.join("WORKBENCH.md").exists() {
        atomic_write_text(&ws_canon.join("WORKBENCH.md"), TEMPLATE_WORKBENCH_MD)?;
    }
    if !ws_canon.join("tasks").join("README.md").exists() {
        atomic_write_text(
            &ws_canon.join("tasks").join("README.md"),
            TEMPLATE_TASKS_README,
        )?;
    }
    if !ws_canon.join("tasks").join("session-types.json").exists() {
        atomic_write_json(
            &ws_canon.join("tasks").join("session-types.json"),
            &json!({"schema": SCHEMA_SESSION_TYPES, "sessions": []}),
        )?;
    }
    if opts.agent_guide && !ws_canon.join("AGENTS.md").exists() {
        atomic_write_text(&ws_canon.join("AGENTS.md"), AGENTS_GUIDE_TEMPLATE)?;
    }
    if !ws_canon.join("tasks").join("INDEX.md").exists() {
        rebuild_index(&ws_canon)?;
    }

    let mut example_created = false;
    if opts.example {
        example_created = create_example_task(&ws_canon)?;
    }

    let mut res = Map::new();
    res.insert(
        "workspace".into(),
        Value::String(ws_canon.to_string_lossy().into_owned()),
    );
    res.insert("dry_run".into(), Value::Bool(false));
    res.insert("config".into(), cfg);
    res.insert("example_created".into(), Value::Bool(example_created));
    res.insert("plan".into(), Value::Array(plan));
    Ok(Value::Object(res))
}
