use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::core::context::{build_context_data, format_context_full, format_context_plain};
use crate::core::delivery::{apply_delivery, load_delivery, pin_delivery};
use crate::core::errors::{EXIT_OK, EXIT_VALIDATION_ERROR, WbError};
use crate::core::evidence::{
    confirm_action, evaluate_actions_status, export_evidence, import_evidence,
};
use crate::core::io_utils::read_yaml_file;
use crate::core::review::prepare_review;
use crate::core::sync::{sync_all_tasks, sync_task};
use crate::core::task_store::{
    CreateTaskOpts, add_task_link, append_progress, compute_task_stats, create_task, list_tasks,
    manage_blockers, manage_components, manage_doc, manage_material, manage_milestones,
    manage_next_steps, record_session, update_task_status,
};
use crate::core::yaml::from_yaml_str;
use crate::dashboard::process::{get_dashboard_status, start_dashboard, stop_dashboard};
use crate::dashboard::server::serve_foreground;
use crate::extensions::registry::list_extensions;
use crate::workspace::config::load_workspace_config;
use crate::workspace::doctor::run_doctor;
use crate::workspace::init_ws::{InitOpts, init_workbench};
use crate::workspace::locator::locate_workspace;
use crate::workspace::upgrade::upgrade_workspace;

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn help_text() -> &'static str {
    "Trestle (wb) - 本地优先的通用个人工作台

用法:
  wb [--workspace <路径>] <子命令> [选项...]

全局选项:
  --workspace <路径>   指定工作台目录（优先级高于 WB_ROOT 与向上查找）
  -h, --help           显示帮助信息
  -V, --version        显示版本号

初始化与维护:
  init [路径]          初始化或非破坏接管工作台
  workspace upgrade    预览 (--dry-run) 或应用 (--apply) 工作台升级
  doctor               只读核查配置、数据格式、引用、分支与回执健康状态

任务与交接:
  new [<id>]           创建任务
  status <id> <st>     更新任务状态
  context <id>         输出任务交接上下文（默认 --plain <=80行，可选 --full / --json）
  progress <id> <msg>  追加只增不改时间顺序进展记录
  next <id> ...        维护下一步动作 (add/done/remove/clear)
  blocker <id> ...     维护阻塞项 (add/resolve/remove/clear)
  milestone <id> ...   维护里程碑 (add/done/complete/undone/reopen/remove)
  component <id> ...   维护活动标签 (add/remove)
  doc <id> ...         登记或移除文档 (add/remove)
  material <id> ...    登记或移除材料 (add/remove)
  link <id> <url>      登记外部链接
  task list|record|stats

交付与证据:
  delivery show|apply|pin
  action list|confirm
  sync [<id>] [--all]
  evidence import|export
  review prepare

看板与扩展:
  dashboard serve|status|stop
  extensions list
"
}

fn split_global_workspace(raw_args: Vec<String>) -> Result<(Option<String>, Vec<String>), WbError> {
    let mut workspace: Option<String> = None;
    let mut rest: Vec<String> = Vec::with_capacity(raw_args.len());
    let mut i = 0;
    while i < raw_args.len() {
        let arg = &raw_args[i];
        if arg == "--workspace" {
            i += 1;
            let val = raw_args
                .get(i)
                .ok_or_else(|| WbError::validation("--workspace 缺少路径参数"))?;
            workspace = Some(val.clone());
        } else if let Some(val) = arg.strip_prefix("--workspace=") {
            if val.is_empty() {
                return Err(WbError::validation("--workspace 路径不能为空"));
            }
            workspace = Some(val.to_string());
        } else {
            rest.push(arg.clone());
        }
        i += 1;
    }
    Ok((workspace, rest))
}

fn next_flag_value(args: &[String], idx: &mut usize, flag: &str) -> Result<String, WbError> {
    if let Some(val) = args[*idx].split_once('=').map(|(_, v)| v.to_string()) {
        return Ok(val);
    }
    *idx += 1;
    args.get(*idx)
        .cloned()
        .ok_or_else(|| WbError::validation(format!("选项 {flag} 缺少参数值")))
}

fn flag_name(arg: &str) -> &str {
    arg.split_once('=').map(|(k, _)| k).unwrap_or(arg)
}

fn unexpected_arg(arg: &str) -> WbError {
    if arg.starts_with('-') {
        WbError::validation(format!("未知选项: {arg}"))
    } else {
        WbError::validation(format!("多余的位置参数: {arg}"))
    }
}

fn require_pos<'a>(pos: &'a [String], idx: usize, name: &str) -> Result<&'a str, WbError> {
    pos.get(idx)
        .map(String::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| WbError::validation(format!("缺少必需参数: {name}")))
}

fn print_json(val: &Value) -> Result<(), WbError> {
    let text = serde_json::to_string_pretty(val)
        .map_err(|e| WbError::io(format!("序列化 JSON 输出失败: {e}")))?;
    println!("{text}");
    Ok(())
}

fn parse_pos_or_index(raw: &str) -> (&str, Option<usize>) {
    if !raw.is_empty() && raw.chars().all(|c| c.is_ascii_digit()) {
        if let Ok(n) = raw.parse::<usize>() {
            return (raw, Some(n));
        }
    }
    (raw, None)
}

fn resolve_ws(ws_flag: Option<&str>) -> Result<PathBuf, WbError> {
    locate_workspace(ws_flag, None)
}

fn handle_init(args: &[String]) -> Result<i32, WbError> {
    let mut pos: Vec<String> = Vec::new();
    let mut name: Option<String> = None;
    let mut repos_root: Option<String> = None;
    let mut timezone: Option<String> = None;
    let mut port: Option<i64> = None;
    let mut yes = false;
    let mut dry_run = false;
    let mut example = false;
    let mut agent_guide = false;
    let mut json_out = false;

    let mut i = 0;
    while i < args.len() {
        match flag_name(&args[i]) {
            "--name" => name = Some(next_flag_value(args, &mut i, "--name")?),
            "--repos-root" => repos_root = Some(next_flag_value(args, &mut i, "--repos-root")?),
            "--timezone" => timezone = Some(next_flag_value(args, &mut i, "--timezone")?),
            "--port" => {
                let raw = next_flag_value(args, &mut i, "--port")?;
                port = Some(
                    raw.parse::<i64>()
                        .map_err(|_| WbError::validation(format!("--port 必须为整数: {raw}")))?,
                );
            }
            "--yes" | "-y" => yes = true,
            "--dry-run" => dry_run = true,
            "--example" => example = true,
            "--agent-guide" => agent_guide = true,
            "--json" => json_out = true,
            "--help" | "-h" => {
                println!(
                    "用法: wb init [路径] [--name <名>] [--repos-root <目录>] [--timezone <时区>] [--port <端口>] [--yes] [--dry-run] [--example] [--agent-guide] [--json]"
                );
                return Ok(EXIT_OK);
            }
            other if other.starts_with('-') => return Err(unexpected_arg(&args[i])),
            _ => pos.push(args[i].clone()),
        }
        i += 1;
    }

    if pos.len() > 1 {
        return Err(unexpected_arg(&pos[1]));
    }
    let target = pos.first().map(String::as_str).unwrap_or(".");
    let res = init_workbench(InitOpts {
        target_path: target,
        name: name.as_deref(),
        repos_root: repos_root.as_deref(),
        timezone: timezone.as_deref(),
        port,
        yes,
        dry_run,
        example,
        agent_guide,
    })?;

    if json_out {
        print_json(&res)?;
    } else if dry_run {
        println!("Dry-run 初始化预览: {}", res["workspace"].as_str().unwrap_or(target));
        if let Some(plan) = res.get("plan").and_then(|v| v.as_array()) {
            for item in plan {
                println!(
                    "  [{}] {}",
                    item["action"].as_str().unwrap_or(""),
                    item["path"].as_str().unwrap_or("")
                );
            }
        }
    } else {
        println!(
            "工作台就绪: {}",
            res["workspace"].as_str().unwrap_or(target)
        );
    }
    Ok(EXIT_OK)
}

fn handle_workspace(ws_flag: Option<&str>, args: &[String]) -> Result<i32, WbError> {
    let sub = args.first().map(String::as_str).unwrap_or("");
    if sub != "upgrade" {
        return Err(WbError::validation(format!(
            "未知 workspace 子命令: {sub}（支持: upgrade）"
        )));
    }
    let mut dry_run = false;
    let mut apply = false;
    let mut json_out = false;
    for arg in &args[1..] {
        match arg.as_str() {
            "--dry-run" => dry_run = true,
            "--apply" => apply = true,
            "--json" => json_out = true,
            _ => return Err(unexpected_arg(arg)),
        }
    }
    if dry_run && apply {
        return Err(WbError::validation("--dry-run 与 --apply 不能同时指定"));
    }

    let ws = resolve_ws(ws_flag)?;
    let res = upgrade_workspace(&ws, apply)?;
    if json_out {
        print_json(&res)?;
    } else if apply {
        println!("工作台升级完成: {}", res["workspace"].as_str().unwrap_or(""));
        if let Some(b) = res.get("backup_dir").and_then(|v| v.as_str()) {
            println!("配置备份目录: {b}");
        }
    } else {
        println!("工作台升级预览 (--dry-run): {}", res["workspace"].as_str().unwrap_or(""));
        if let Some(changes) = res.get("planned_changes").and_then(|v| v.as_array()) {
            for c in changes {
                println!("  - {}", c.as_str().unwrap_or(""));
            }
        }
    }
    Ok(EXIT_OK)
}

fn handle_doctor(ws_flag: Option<&str>, args: &[String]) -> Result<i32, WbError> {
    let mut json_out = false;
    for arg in args {
        match arg.as_str() {
            "--json" => json_out = true,
            _ => return Err(unexpected_arg(arg)),
        }
    }
    let ws = resolve_ws(ws_flag)?;
    let report = run_doctor(&ws);
    if json_out {
        print_json(&report)?;
    } else {
        let healthy = report.get("healthy").and_then(|v| v.as_bool()).unwrap_or(false);
        let errors = report.get("error_count").and_then(|v| v.as_u64()).unwrap_or(0);
        let warnings = report.get("warning_count").and_then(|v| v.as_u64()).unwrap_or(0);
        println!(
            "工作台体检 [{}]: healthy={healthy}, errors={errors}, warnings={warnings}",
            report["workspace"].as_str().unwrap_or("")
        );
        if let Some(findings) = report.get("findings").and_then(|v| v.as_array()) {
            for f in findings {
                let sev = f.get("severity").and_then(|v| v.as_str()).unwrap_or("");
                let code = f.get("code").and_then(|v| v.as_str()).unwrap_or("");
                let msg = f.get("message").and_then(|v| v.as_str()).unwrap_or("");
                println!("  [{sev}] ({code}) {msg}");
            }
        }
    }
    if report.get("healthy").and_then(|v| v.as_bool()) == Some(true) {
        Ok(EXIT_OK)
    } else {
        Ok(EXIT_VALIDATION_ERROR)
    }
}

fn handle_new(ws_flag: Option<&str>, args: &[String]) -> Result<i32, WbError> {
    let mut pos: Vec<String> = Vec::new();
    let mut id_flag: Option<String> = None;
    let mut title: Option<String> = None;
    let mut goal = String::new();
    let mut status = "planning".to_string();
    let mut priority = "P2".to_string();
    let mut owner = String::new();
    let mut category = "unclassified".to_string();
    let mut components: Vec<String> = Vec::new();
    let mut session_id: Option<String> = None;
    let mut host: Option<String> = None;
    let mut session_title: Option<String> = None;
    let mut session_category: Option<String> = None;
    let mut json_out = false;

    let mut i = 0;
    while i < args.len() {
        match flag_name(&args[i]) {
            "--id" => id_flag = Some(next_flag_value(args, &mut i, "--id")?),
            "--title" => title = Some(next_flag_value(args, &mut i, "--title")?),
            "--goal" => goal = next_flag_value(args, &mut i, "--goal")?,
            "--status" => status = next_flag_value(args, &mut i, "--status")?,
            "--priority" => priority = next_flag_value(args, &mut i, "--priority")?,
            "--owner" => owner = next_flag_value(args, &mut i, "--owner")?,
            "--category" => category = next_flag_value(args, &mut i, "--category")?,
            "--component" => {
                let raw = next_flag_value(args, &mut i, "--component")?;
                for part in raw.split(',') {
                    let p = part.trim();
                    if !p.is_empty() {
                        components.push(p.to_string());
                    }
                }
            }
            "--session-id" => session_id = Some(next_flag_value(args, &mut i, "--session-id")?),
            "--host" | "--session-host" => host = Some(next_flag_value(args, &mut i, "--host")?),
            "--session-title" => {
                session_title = Some(next_flag_value(args, &mut i, "--session-title")?)
            }
            "--session-category" => {
                session_category = Some(next_flag_value(args, &mut i, "--session-category")?)
            }
            "--json" => json_out = true,
            other if other.starts_with('-') => return Err(unexpected_arg(&args[i])),
            _ => pos.push(args[i].clone()),
        }
        i += 1;
    }

    if pos.len() > 1 {
        return Err(unexpected_arg(&pos[1]));
    }
    let title_str = title.ok_or_else(|| WbError::validation("wb new 必须指定 --title"))?;
    let task_id = match (id_flag, pos.first()) {
        (Some(a), Some(b)) if a != *b => {
            return Err(WbError::validation("同时传入位置参数和 --id 且不一致"));
        }
        (Some(a), _) => a,
        (None, Some(b)) => b.clone(),
        (None, None) => {
            let stamp = chrono::Utc::now().format("%Y%m%d-%H%M%S");
            format!("TASK-{stamp}-{:04x}", fastrand::u16(..))
        }
    };

    let ws = resolve_ws(ws_flag)?;
    let data = create_task(
        &ws,
        CreateTaskOpts {
            task_id: &task_id,
            title: &title_str,
            goal: &goal,
            status: &status,
            priority: &priority,
            owner: &owner,
            category: &category,
            components: &components,
            origin_session_id: session_id.as_deref(),
            origin_host: host.as_deref(),
            origin_title: session_title.as_deref(),
            origin_category: session_category.as_deref(),
            timestamp: None,
        },
    )?;

    if json_out {
        print_json(&data)?;
    } else {
        println!(
            "已创建任务: {} ({})",
            data["id"].as_str().unwrap_or(""),
            data["title"].as_str().unwrap_or("")
        );
    }
    Ok(EXIT_OK)
}

fn handle_status(ws_flag: Option<&str>, args: &[String]) -> Result<i32, WbError> {
    let mut pos: Vec<String> = Vec::new();
    let mut note: Option<String> = None;
    let mut json_out = false;
    let mut i = 0;
    while i < args.len() {
        match flag_name(&args[i]) {
            "--note" => note = Some(next_flag_value(args, &mut i, "--note")?),
            "--json" => json_out = true,
            other if other.starts_with('-') => return Err(unexpected_arg(&args[i])),
            _ => pos.push(args[i].clone()),
        }
        i += 1;
    }
    if pos.len() > 2 {
        return Err(unexpected_arg(&pos[2]));
    }
    let task_id = require_pos(&pos, 0, "<task-id>")?;
    let status = require_pos(&pos, 1, "<status>")?;
    let ws = resolve_ws(ws_flag)?;
    let data = update_task_status(&ws, task_id, status, note.as_deref())?;
    if json_out {
        print_json(&data)?;
    } else {
        println!("{task_id}: status -> {status}");
    }
    Ok(EXIT_OK)
}

fn handle_context(ws_flag: Option<&str>, args: &[String]) -> Result<i32, WbError> {
    let mut pos: Vec<String> = Vec::new();
    let mut mode = "plain";
    let mut max_lines: usize = 80;
    let mut i = 0;
    while i < args.len() {
        match flag_name(&args[i]) {
            "--plain" => mode = "plain",
            "--full" => mode = "full",
            "--json" => mode = "json",
            "--max-lines" => {
                let raw = next_flag_value(args, &mut i, "--max-lines")?;
                max_lines = raw
                    .parse::<usize>()
                    .map_err(|_| WbError::validation(format!("--max-lines 必须为正整数: {raw}")))?;
            }
            other if other.starts_with('-') => return Err(unexpected_arg(&args[i])),
            _ => pos.push(args[i].clone()),
        }
        i += 1;
    }
    if pos.len() > 1 {
        return Err(unexpected_arg(&pos[1]));
    }
    let task_id = require_pos(&pos, 0, "<task-id>")?;
    let ws = resolve_ws(ws_flag)?;
    let ctx = build_context_data(&ws, task_id)?;
    match mode {
        "json" => print_json(&ctx)?,
        "full" => print!("{}", format_context_full(&ws, &ctx)),
        _ => print!("{}", format_context_plain(&ctx, max_lines)),
    }
    Ok(EXIT_OK)
}

fn handle_progress(ws_flag: Option<&str>, args: &[String]) -> Result<i32, WbError> {
    let mut pos: Vec<String> = Vec::new();
    let mut json_out = false;
    for arg in args {
        if arg == "--json" {
            json_out = true;
        } else if arg.starts_with('-') {
            return Err(unexpected_arg(arg));
        } else {
            pos.push(arg.clone());
        }
    }
    let task_id = require_pos(&pos, 0, "<task-id>")?;
    let message = require_pos(&pos, 1, "<message>")?;
    if pos.len() > 2 {
        return Err(unexpected_arg(&pos[2]));
    }
    let ws = resolve_ws(ws_flag)?;
    let data = append_progress(&ws, task_id, message)?;
    if json_out {
        print_json(&data)?;
    } else {
        println!("{task_id}: 已追加进展记录");
    }
    Ok(EXIT_OK)
}

fn handle_list_field(ws_flag: Option<&str>, field: &str, args: &[String]) -> Result<i32, WbError> {
    let mut pos: Vec<String> = Vec::new();
    let mut index_flag: Option<usize> = None;
    let mut json_out = false;
    let mut i = 0;
    while i < args.len() {
        match flag_name(&args[i]) {
            "--index" => {
                let raw = next_flag_value(args, &mut i, "--index")?;
                index_flag = Some(
                    raw.parse::<usize>()
                        .map_err(|_| WbError::validation(format!("--index 必须为非负整数: {raw}")))?,
                );
            }
            "--json" => json_out = true,
            other if other.starts_with('-') => return Err(unexpected_arg(&args[i])),
            _ => pos.push(args[i].clone()),
        }
        i += 1;
    }
    if pos.len() > 3 {
        return Err(unexpected_arg(&pos[3]));
    }
    let task_id = require_pos(&pos, 0, "<task-id>")?;
    let raw_op = require_pos(&pos, 1, "<action>")?;
    let arg_val = pos.get(2).map(String::as_str).unwrap_or("");
    let ws = resolve_ws(ws_flag)?;

    let data = match field {
        "next" => {
            let op = match raw_op {
                "done" => "remove",
                other => other,
            };
            let (val, parsed_idx) = if op == "remove" && index_flag.is_none() {
                parse_pos_or_index(arg_val)
            } else {
                (arg_val, index_flag)
            };
            if op == "add" && val.trim().is_empty() {
                return Err(WbError::validation("next add 需要提供内容"));
            }
            if op == "remove" && parsed_idx.is_none() && val.trim().is_empty() {
                return Err(WbError::validation("next done/remove 需要提供索引或内容"));
            }
            manage_next_steps(&ws, task_id, op, val, parsed_idx)?
        }
        "blocker" => {
            let op = match raw_op {
                "resolve" => "remove",
                other => other,
            };
            let (val, parsed_idx) = if op == "remove" && index_flag.is_none() {
                parse_pos_or_index(arg_val)
            } else {
                (arg_val, index_flag)
            };
            if op == "add" && val.trim().is_empty() {
                return Err(WbError::validation("blocker add 需要提供内容"));
            }
            if op == "remove" && parsed_idx.is_none() && val.trim().is_empty() {
                return Err(WbError::validation("blocker resolve/remove 需要提供索引或内容"));
            }
            manage_blockers(&ws, task_id, op, val, parsed_idx)?
        }
        "component" => {
            let val = require_pos(&pos, 2, "<name>")?;
            manage_components(&ws, task_id, raw_op, val)?
        }
        "doc" => {
            let val = require_pos(&pos, 2, "<path>")?;
            manage_doc(&ws, task_id, raw_op, val)?
        }
        "material" => {
            let val = require_pos(&pos, 2, "<path>")?;
            manage_material(&ws, task_id, raw_op, val)?
        }
        _ => unreachable!(),
    };

    if json_out {
        print_json(&data)?;
    } else {
        println!("{task_id}: {field} {raw_op} 完成");
    }
    Ok(EXIT_OK)
}

fn handle_milestone(ws_flag: Option<&str>, args: &[String]) -> Result<i32, WbError> {
    let mut pos: Vec<String> = Vec::new();
    let mut done_flag = false;
    let mut json_out = false;
    for arg in args {
        match arg.as_str() {
            "--done" => done_flag = true,
            "--json" => json_out = true,
            other if other.starts_with('-') => return Err(unexpected_arg(other)),
            _ => pos.push(arg.clone()),
        }
    }
    if pos.len() > 3 {
        return Err(unexpected_arg(&pos[3]));
    }
    let task_id = require_pos(&pos, 0, "<task-id>")?;
    let raw_op = require_pos(&pos, 1, "<action>")?;
    let title = require_pos(&pos, 2, "<title>")?;
    let op = match raw_op {
        "done" => "complete",
        "undone" => "reopen",
        other => other,
    };
    let ws = resolve_ws(ws_flag)?;
    let data = manage_milestones(&ws, task_id, op, title, done_flag)?;
    if json_out {
        print_json(&data)?;
    } else {
        println!("{task_id}: milestone {op} ({title})");
    }
    Ok(EXIT_OK)
}

fn handle_link(ws_flag: Option<&str>, args: &[String]) -> Result<i32, WbError> {
    let mut pos: Vec<String> = Vec::new();
    let mut json_out = false;
    for arg in args {
        if arg == "--json" {
            json_out = true;
        } else if arg.starts_with('-') {
            return Err(unexpected_arg(arg));
        } else {
            pos.push(arg.clone());
        }
    }
    if pos.len() > 2 {
        return Err(unexpected_arg(&pos[2]));
    }
    let task_id = require_pos(&pos, 0, "<task-id>")?;
    let url = require_pos(&pos, 1, "<url>")?;
    let ws = resolve_ws(ws_flag)?;
    let data = add_task_link(&ws, task_id, url)?;
    if json_out {
        print_json(&data)?;
    } else {
        println!("{task_id}: 已关联链接 {url}");
    }
    Ok(EXIT_OK)
}

fn handle_task(ws_flag: Option<&str>, args: &[String]) -> Result<i32, WbError> {
    let sub = args.first().map(String::as_str).unwrap_or("");
    let rest = if args.len() > 1 { &args[1..] } else { &[] };
    match sub {
        "list" => {
            let mut status: Option<String> = None;
            let mut category: Option<String> = None;
            let mut priority: Option<String> = None;
            let mut owner: Option<String> = None;
            let mut component: Option<String> = None;
            let mut json_out = false;
            let mut i = 0;
            while i < rest.len() {
                match flag_name(&rest[i]) {
                    "--status" => status = Some(next_flag_value(rest, &mut i, "--status")?),
                    "--category" => category = Some(next_flag_value(rest, &mut i, "--category")?),
                    "--priority" => priority = Some(next_flag_value(rest, &mut i, "--priority")?),
                    "--owner" => owner = Some(next_flag_value(rest, &mut i, "--owner")?),
                    "--component" => component = Some(next_flag_value(rest, &mut i, "--component")?),
                    "--json" => json_out = true,
                    _ => return Err(unexpected_arg(&rest[i])),
                }
                i += 1;
            }
            let ws = resolve_ws(ws_flag)?;
            let mut tasks = list_tasks(
                &ws,
                status.as_deref(),
                category.as_deref(),
                priority.as_deref(),
                owner.as_deref(),
            )?;
            if let Some(ref comp) = component {
                tasks.retain(|t| {
                    t.get("components")
                        .and_then(|v| v.as_array())
                        .map(|arr| arr.iter().any(|x| x.as_str() == Some(comp.as_str())))
                        .unwrap_or(false)
                });
            }
            if json_out {
                print_json(&Value::Array(tasks))?;
            } else if tasks.is_empty() {
                println!("（无匹配任务）");
            } else {
                for t in tasks {
                    println!(
                        "{} [{}] ({}, {}) - {}",
                        t["id"].as_str().unwrap_or(""),
                        t["status"].as_str().unwrap_or(""),
                        t["priority"].as_str().unwrap_or(""),
                        t["category"].as_str().unwrap_or(""),
                        t["title"].as_str().unwrap_or(""),
                    );
                }
            }
            Ok(EXIT_OK)
        }
        "record" => {
            let mut task_id: Option<String> = None;
            let mut session_id: Option<String> = None;
            let mut host = "localhost".to_string();
            let mut title: Option<String> = None;
            let mut category: Option<String> = None;
            let mut role = "associated".to_string();
            let mut json_out = false;
            let mut i = 0;
            while i < rest.len() {
                match flag_name(&rest[i]) {
                    "--task" | "--task-id" => {
                        task_id = Some(next_flag_value(rest, &mut i, "--task")?)
                    }
                    "--session-id" => {
                        session_id = Some(next_flag_value(rest, &mut i, "--session-id")?)
                    }
                    "--host" => host = next_flag_value(rest, &mut i, "--host")?,
                    "--title" => title = Some(next_flag_value(rest, &mut i, "--title")?),
                    "--category" => category = Some(next_flag_value(rest, &mut i, "--category")?),
                    "--role" => role = next_flag_value(rest, &mut i, "--role")?,
                    "--json" => json_out = true,
                    _ => return Err(unexpected_arg(&rest[i])),
                }
                i += 1;
            }
            let sid = session_id
                .ok_or_else(|| WbError::validation("wb task record 必须提供 --session-id"))?;
            let stitle =
                title.ok_or_else(|| WbError::validation("wb task record 必须提供 --title"))?;
            let ws = resolve_ws(ws_flag)?;
            let entry = record_session(
                &ws,
                task_id.as_deref(),
                &sid,
                &host,
                &stitle,
                category.as_deref(),
                &role,
            )?;
            if json_out {
                print_json(&entry)?;
            } else {
                println!(
                    "已登记会话: {}@{} [{}] {}",
                    entry["session_id"].as_str().unwrap_or(""),
                    entry["host"].as_str().unwrap_or(""),
                    entry["category"].as_str().unwrap_or(""),
                    entry["title"].as_str().unwrap_or(""),
                );
            }
            Ok(EXIT_OK)
        }
        "stats" => {
            let mut json_out = false;
            for arg in rest {
                if arg == "--json" {
                    json_out = true;
                } else {
                    return Err(unexpected_arg(arg));
                }
            }
            let ws = resolve_ws(ws_flag)?;
            let stats = compute_task_stats(&ws)?;
            if json_out {
                print_json(&stats)?;
            } else {
                println!(
                    "任务总数: {} | 会话总数: {}",
                    stats["total_tasks"].as_i64().unwrap_or(0),
                    stats["total_sessions"].as_i64().unwrap_or(0),
                );
                println!("按状态: {}", stats["by_status"]);
                println!("按主类: {}", stats["by_category"]);
            }
            Ok(EXIT_OK)
        }
        _ => Err(WbError::validation(format!(
            "未知 task 子命令: {sub}（支持: list, record, stats）"
        ))),
    }
}

fn read_structured_input(path_str: &str) -> Result<Value, WbError> {
    let raw = if path_str == "-" {
        let mut buf = String::new();
        io::stdin()
            .read_to_string(&mut buf)
            .map_err(|e| WbError::io(format!("读取标准输入失败: {e}")))?;
        buf
    } else {
        let p = Path::new(path_str);
        if p.extension().and_then(|s| s.to_str()) == Some("json") {
            let s = fs::read_to_string(p)
                .map_err(|e| WbError::io(format!("读取文件失败 {}: {e}", p.display())))?;
            return serde_json::from_str(&s)
                .map_err(|e| WbError::validation(format!("JSON 解析失败: {e}")));
        }
        return read_yaml_file(p);
    };

    if raw.trim_start().starts_with('{') {
        serde_json::from_str(&raw).map_err(|e| WbError::validation(format!("JSON 解析失败: {e}")))
    } else {
        from_yaml_str(&raw)
    }
}

fn handle_delivery(ws_flag: Option<&str>, args: &[String]) -> Result<i32, WbError> {
    let sub = args.first().map(String::as_str).unwrap_or("");
    let rest = if args.len() > 1 { &args[1..] } else { &[] };
    match sub {
        "show" => {
            let mut pos: Vec<String> = Vec::new();
            let mut json_out = false;
            for arg in rest {
                if arg == "--json" {
                    json_out = true;
                } else if arg.starts_with('-') {
                    return Err(unexpected_arg(arg));
                } else {
                    pos.push(arg.clone());
                }
            }
            if pos.len() > 1 {
                return Err(unexpected_arg(&pos[1]));
            }
            let task_id = require_pos(&pos, 0, "<task-id>")?;
            let ws = resolve_ws(ws_flag)?;
            let deliv = load_delivery(&ws, task_id, false)?.unwrap_or(Value::Null);
            if json_out {
                print_json(&deliv)?;
            } else if deliv.is_null() {
                println!("{task_id}: 尚未配置 delivery.yaml");
            } else {
                println!(
                    "{}",
                    crate::core::yaml::to_yaml_string(&deliv).trim_end()
                );
            }
            Ok(EXIT_OK)
        }
        "apply" => {
            let mut pos: Vec<String> = Vec::new();
            let mut file_path: Option<String> = None;
            let mut expected_rev: Option<i64> = None;
            let mut json_out = false;
            let mut i = 0;
            while i < rest.len() {
                match flag_name(&rest[i]) {
                    "--file" | "-f" => file_path = Some(next_flag_value(rest, &mut i, "--file")?),
                    "--expected-revision" | "--rev" => {
                        let raw = next_flag_value(rest, &mut i, "--expected-revision")?;
                        expected_rev = Some(
                            raw.parse::<i64>()
                                .map_err(|_| WbError::validation(format!("修订号必须为整数: {raw}")))?,
                        );
                    }
                    "--json" => json_out = true,
                    other if other.starts_with('-') => return Err(unexpected_arg(&rest[i])),
                    _ => pos.push(rest[i].clone()),
                }
                i += 1;
            }
            if pos.len() > 1 {
                return Err(unexpected_arg(&pos[1]));
            }
            let task_id = require_pos(&pos, 0, "<task-id>")?;
            let fpath = file_path
                .ok_or_else(|| WbError::validation("wb delivery apply 必须指定 --file"))?;
            let payload = read_structured_input(&fpath)?;
            let ws = resolve_ws(ws_flag)?;
            let updated = apply_delivery(&ws, task_id, &payload, expected_rev)?;
            if json_out {
                print_json(&updated)?;
            } else {
                println!(
                    "{task_id}: delivery 已更新至 revision={}",
                    updated["revision"].as_i64().unwrap_or(0)
                );
            }
            Ok(EXIT_OK)
        }
        "pin" => {
            let mut pos: Vec<String> = Vec::new();
            let mut item_id: Option<String> = None;
            let mut commit: Option<String> = None;
            let mut expected_rev: Option<i64> = None;
            let mut json_out = false;
            let mut i = 0;
            while i < rest.len() {
                match flag_name(&rest[i]) {
                    "--item" => item_id = Some(next_flag_value(rest, &mut i, "--item")?),
                    "--commit" => commit = Some(next_flag_value(rest, &mut i, "--commit")?),
                    "--expected-revision" | "--rev" => {
                        let raw = next_flag_value(rest, &mut i, "--expected-revision")?;
                        expected_rev = Some(
                            raw.parse::<i64>()
                                .map_err(|_| WbError::validation(format!("修订号必须为整数: {raw}")))?,
                        );
                    }
                    "--json" => json_out = true,
                    other if other.starts_with('-') => return Err(unexpected_arg(&rest[i])),
                    _ => pos.push(rest[i].clone()),
                }
                i += 1;
            }
            if pos.len() > 1 {
                return Err(unexpected_arg(&pos[1]));
            }
            let task_id = require_pos(&pos, 0, "<task-id>")?;
            let ws = resolve_ws(ws_flag)?;
            let updated = pin_delivery(
                &ws,
                task_id,
                item_id.as_deref(),
                commit.as_deref(),
                expected_rev,
            )?;
            if json_out {
                print_json(&updated)?;
            } else {
                println!(
                    "{task_id}: 已固定 compare_base (revision={})",
                    updated["revision"].as_i64().unwrap_or(0)
                );
            }
            Ok(EXIT_OK)
        }
        _ => Err(WbError::validation(format!(
            "未知 delivery 子命令: {sub}（支持: show, apply, pin）"
        ))),
    }
}

fn handle_action(ws_flag: Option<&str>, args: &[String]) -> Result<i32, WbError> {
    let sub = args.first().map(String::as_str).unwrap_or("");
    let rest = if args.len() > 1 { &args[1..] } else { &[] };
    match sub {
        "list" => {
            let mut pos: Vec<String> = Vec::new();
            let mut json_out = false;
            for arg in rest {
                if arg == "--json" {
                    json_out = true;
                } else if arg.starts_with('-') {
                    return Err(unexpected_arg(arg));
                } else {
                    pos.push(arg.clone());
                }
            }
            if pos.len() > 1 {
                return Err(unexpected_arg(&pos[1]));
            }
            let task_id = require_pos(&pos, 0, "<task-id>")?;
            let ws = resolve_ws(ws_flag)?;
            let acts = evaluate_actions_status(&ws, task_id)?;
            if json_out {
                print_json(&Value::Array(acts))?;
            } else if acts.is_empty() {
                println!("{task_id}: 无待确认动作");
            } else {
                for a in acts {
                    println!(
                        "{} [{}] status={} scope_sha256={} - {}",
                        a["id"].as_str().unwrap_or(""),
                        a["kind"].as_str().unwrap_or(""),
                        a["status"].as_str().unwrap_or(""),
                        a["scope_sha256"].as_str().unwrap_or(""),
                        a["reason"].as_str().unwrap_or(""),
                    );
                }
            }
            Ok(EXIT_OK)
        }
        "confirm" => {
            let mut pos: Vec<String> = Vec::new();
            let mut by: Option<String> = None;
            let mut source: Option<String> = None;
            let mut digest: Option<String> = None;
            let mut note = String::new();
            let mut json_out = false;
            let mut i = 0;
            while i < rest.len() {
                match flag_name(&rest[i]) {
                    "--by" => by = Some(next_flag_value(rest, &mut i, "--by")?),
                    "--source" => source = Some(next_flag_value(rest, &mut i, "--source")?),
                    "--digest" => digest = Some(next_flag_value(rest, &mut i, "--digest")?),
                    "--note" => note = next_flag_value(rest, &mut i, "--note")?,
                    "--json" => json_out = true,
                    other if other.starts_with('-') => return Err(unexpected_arg(&rest[i])),
                    _ => pos.push(rest[i].clone()),
                }
                i += 1;
            }
            if pos.len() > 2 {
                return Err(unexpected_arg(&pos[2]));
            }
            let task_id = require_pos(&pos, 0, "<task-id>")?;
            let action_id = require_pos(&pos, 1, "<action-id>")?;
            let by_s = by.ok_or_else(|| WbError::validation("必须显式传入确认人 (--by)"))?;
            let src_s = source.ok_or_else(|| WbError::validation("必须显式传入来源说明 (--source)"))?;
            let dig_s = digest.ok_or_else(|| WbError::validation("必须显式传入动作范围校验值 (--digest)"))?;
            let ws = resolve_ws(ws_flag)?;
            let receipt = confirm_action(&ws, task_id, action_id, &by_s, &src_s, &dig_s, &note)?;
            if json_out {
                print_json(&receipt)?;
            } else {
                println!("{task_id}/{action_id}: 已签发确认回执");
            }
            Ok(EXIT_OK)
        }
        _ => Err(WbError::validation(format!(
            "未知 action 子命令: {sub}（支持: list, confirm）"
        ))),
    }
}

fn handle_sync(ws_flag: Option<&str>, args: &[String]) -> Result<i32, WbError> {
    let mut pos: Vec<String> = Vec::new();
    let mut all = false;
    let mut json_out = false;
    for arg in args {
        match arg.as_str() {
            "--all" => all = true,
            "--json" => json_out = true,
            other if other.starts_with('-') => return Err(unexpected_arg(other)),
            _ => pos.push(arg.clone()),
        }
    }
    if pos.len() > 1 {
        return Err(unexpected_arg(&pos[1]));
    }
    let ws = resolve_ws(ws_flag)?;
    let cfg = load_workspace_config(&ws)?;
    let repos_root = cfg["repos_root"].as_str().unwrap_or("../repos");

    if all || pos.is_empty() {
        let res = sync_all_tasks(&ws, repos_root)?;
        if json_out {
            print_json(&Value::Array(res))?;
        } else {
            println!("已完成全部任务只读同步，共 {} 项", res.len());
        }
    } else {
        let obs = sync_task(&ws, &pos[0], repos_root)?;
        if json_out {
            print_json(&obs)?;
        } else {
            println!(
                "{}: 同步完成 (git_available={})",
                pos[0],
                obs["git_available"].as_bool().unwrap_or(false)
            );
        }
    }
    Ok(EXIT_OK)
}

fn handle_evidence(ws_flag: Option<&str>, args: &[String]) -> Result<i32, WbError> {
    let sub = args.first().map(String::as_str).unwrap_or("");
    let rest = if args.len() > 1 { &args[1..] } else { &[] };
    match sub {
        "import" => {
            let mut pos: Vec<String> = Vec::new();
            let mut name: Option<String> = None;
            let mut note = String::new();
            let mut json_out = false;
            let mut i = 0;
            while i < rest.len() {
                match flag_name(&rest[i]) {
                    "--name" => name = Some(next_flag_value(rest, &mut i, "--name")?),
                    "--note" => note = next_flag_value(rest, &mut i, "--note")?,
                    "--json" => json_out = true,
                    other if other.starts_with('-') => return Err(unexpected_arg(&rest[i])),
                    _ => pos.push(rest[i].clone()),
                }
                i += 1;
            }
            if pos.len() > 2 {
                return Err(unexpected_arg(&pos[2]));
            }
            let task_id = require_pos(&pos, 0, "<task-id>")?;
            let src_file = require_pos(&pos, 1, "<file>")?;
            let ws = resolve_ws(ws_flag)?;
            let res = import_evidence(&ws, task_id, Path::new(src_file), name.as_deref(), &note)?;
            if json_out {
                print_json(&res)?;
            } else {
                println!(
                    "{task_id}: 已导入证据 {} (sha256={})",
                    res["file"].as_str().unwrap_or(""),
                    res["sha256"].as_str().unwrap_or("")
                );
            }
            Ok(EXIT_OK)
        }
        "export" => {
            let mut pos: Vec<String> = Vec::new();
            let mut out_dir: Option<String> = None;
            let mut json_out = false;
            let mut i = 0;
            while i < rest.len() {
                match flag_name(&rest[i]) {
                    "--out" => out_dir = Some(next_flag_value(rest, &mut i, "--out")?),
                    "--json" => json_out = true,
                    other if other.starts_with('-') => return Err(unexpected_arg(&rest[i])),
                    _ => pos.push(rest[i].clone()),
                }
                i += 1;
            }
            if pos.len() > 2 {
                return Err(unexpected_arg(&pos[2]));
            }
            let task_id = require_pos(&pos, 0, "<task-id>")?;
            let out_str = match (out_dir, pos.get(1)) {
                (Some(a), None) => a,
                (None, Some(b)) => b.clone(),
                (Some(_), Some(b)) => return Err(unexpected_arg(b)),
                (None, None) => {
                    return Err(WbError::validation("wb evidence export 必须指定 --out <目录>"));
                }
            };
            let ws = resolve_ws(ws_flag)?;
            let res = export_evidence(&ws, task_id, Path::new(&out_str))?;
            if json_out {
                print_json(&res)?;
            } else {
                println!(
                    "{task_id}: 已导出证据至 {out_str} (共 {} 个文件)",
                    res["files"].as_array().map(|a| a.len()).unwrap_or(0)
                );
            }
            Ok(EXIT_OK)
        }
        _ => Err(WbError::validation(format!(
            "未知 evidence 子命令: {sub}（支持: import, export）"
        ))),
    }
}

fn handle_review(ws_flag: Option<&str>, args: &[String]) -> Result<i32, WbError> {
    let sub = args.first().map(String::as_str).unwrap_or("");
    if sub != "prepare" {
        return Err(WbError::validation(format!(
            "未知 review 子命令: {sub}（支持: prepare）"
        )));
    }
    let rest = &args[1..];
    let mut pos: Vec<String> = Vec::new();
    let mut url: Option<String> = None;
    let mut report: Option<String> = None;
    let mut json_out = false;
    let mut i = 0;
    while i < rest.len() {
        match flag_name(&rest[i]) {
            "--url" => url = Some(next_flag_value(rest, &mut i, "--url")?),
            "--report" => report = Some(next_flag_value(rest, &mut i, "--report")?),
            "--json" => json_out = true,
            other if other.starts_with('-') => return Err(unexpected_arg(&rest[i])),
            _ => pos.push(rest[i].clone()),
        }
        i += 1;
    }
    if pos.len() > 1 {
        return Err(unexpected_arg(&pos[1]));
    }
    let task_id = require_pos(&pos, 0, "<task-id>")?;
    let url_s = url.ok_or_else(|| WbError::validation("wb review prepare 必须指定 --url"))?;
    let rep_s = report.unwrap_or_else(|| format!("analysis/cr-{task_id}.md"));
    let ws = resolve_ws(ws_flag)?;
    let res = prepare_review(&ws, task_id, &url_s, &rep_s)?;
    if json_out {
        print_json(&res)?;
    } else {
        println!("{}", res["prompt"].as_str().unwrap_or(""));
    }
    Ok(EXIT_OK)
}

fn handle_dashboard(ws_flag: Option<&str>, args: &[String]) -> Result<i32, WbError> {
    let sub = args.first().map(String::as_str).unwrap_or("");
    let rest = if args.len() > 1 { &args[1..] } else { &[] };
    match sub {
        "serve" => {
            let mut port_flag: Option<u16> = None;
            let mut foreground = false;
            let mut json_out = false;
            let mut i = 0;
            while i < rest.len() {
                match flag_name(&rest[i]) {
                    "--port" => {
                        let raw = next_flag_value(rest, &mut i, "--port")?;
                        port_flag = Some(raw.parse::<u16>().map_err(|_| {
                            WbError::validation(format!("--port 必须为 0..65535 整数: {raw}"))
                        })?);
                    }
                    "--foreground" => foreground = true,
                    "--json" => json_out = true,
                    _ => return Err(unexpected_arg(&rest[i])),
                }
                i += 1;
            }
            let ws = resolve_ws(ws_flag)?;
            let cfg = load_workspace_config(&ws)?;
            let port = port_flag.unwrap_or_else(|| {
                cfg.get("dashboard_port")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(8791) as u16
            });
            let res = start_dashboard(&ws, port, foreground)?;
            if json_out {
                print_json(&res)?;
            } else if !foreground {
                println!(
                    "看板已在后台运行: {} (pid={}, instance_id={})",
                    res["url"].as_str().unwrap_or(""),
                    res["pid"].as_i64().unwrap_or(0),
                    res["instance_id"].as_str().unwrap_or("")
                );
            }
            Ok(EXIT_OK)
        }
        "__daemon" => {
            let mut port: u16 = 8791;
            let mut i = 0;
            while i < rest.len() {
                match flag_name(&rest[i]) {
                    "--port" => {
                        let raw = next_flag_value(rest, &mut i, "--port")?;
                        port = raw.parse::<u16>().map_err(|_| {
                            WbError::validation(format!("--port 必须为 0..65535 整数: {raw}"))
                        })?;
                    }
                    _ => return Err(unexpected_arg(&rest[i])),
                }
                i += 1;
            }
            let ws = resolve_ws(ws_flag)?;
            serve_foreground(&ws, port)?;
            Ok(EXIT_OK)
        }
        "status" => {
            let mut json_out = false;
            for arg in rest {
                if arg == "--json" {
                    json_out = true;
                } else {
                    return Err(unexpected_arg(arg));
                }
            }
            let ws = resolve_ws(ws_flag)?;
            let st = get_dashboard_status(&ws)?;
            if json_out {
                print_json(&st)?;
            } else if st.get("running").and_then(|v| v.as_bool()) == Some(true) {
                println!(
                    "运行中: {} (pid={}, instance_id={})",
                    st["url"].as_str().unwrap_or(""),
                    st["pid"].as_i64().unwrap_or(0),
                    st["instance_id"].as_str().unwrap_or("")
                );
            } else {
                println!(
                    "未运行: {}",
                    st["reason"].as_str().unwrap_or("无活跃实例")
                );
            }
            Ok(EXIT_OK)
        }
        "stop" => {
            let mut json_out = false;
            for arg in rest {
                if arg == "--json" {
                    json_out = true;
                } else {
                    return Err(unexpected_arg(arg));
                }
            }
            let ws = resolve_ws(ws_flag)?;
            let res = stop_dashboard(&ws)?;
            if json_out {
                print_json(&res)?;
            } else {
                println!(
                    "看板已停止 (instance_id={})",
                    res["instance_id"].as_str().unwrap_or("")
                );
            }
            Ok(EXIT_OK)
        }
        _ => Err(WbError::validation(format!(
            "未知 dashboard 子命令: {sub}（支持: serve, status, stop）"
        ))),
    }
}

fn handle_extensions(ws_flag: Option<&str>, args: &[String]) -> Result<i32, WbError> {
    let sub = args.first().map(String::as_str).unwrap_or("");
    if sub != "list" {
        return Err(WbError::validation(format!(
            "未知 extensions 子命令: {sub}（支持: list）"
        )));
    }
    let mut json_out = false;
    for arg in &args[1..] {
        if arg == "--json" {
            json_out = true;
        } else {
            return Err(unexpected_arg(arg));
        }
    }
    let ws = resolve_ws(ws_flag)?;
    let res = list_extensions(&ws)?;
    if json_out {
        print_json(&res)?;
    } else {
        let exts = res
            .get("extensions")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        if exts.is_empty() {
            println!("（未发现任何可选扩展清单）");
        } else {
            for e in exts {
                println!(
                    "{} | enabled={} | loaded={} | status={}{}",
                    e["name"].as_str().unwrap_or(""),
                    e["enabled"].as_bool().unwrap_or(false),
                    e["loaded"].as_bool().unwrap_or(false),
                    e["status"].as_str().unwrap_or(""),
                    e.get("error")
                        .and_then(|x| x.as_str())
                        .map(|msg| format!(" ({msg})"))
                        .unwrap_or_default()
                );
            }
        }
    }
    Ok(EXIT_OK)
}

fn dispatch(raw_args: Vec<String>) -> Result<i32, WbError> {
    let (ws_flag, args) = split_global_workspace(raw_args)?;
    if args.is_empty() {
        print!("{}", help_text());
        return Ok(EXIT_OK);
    }

    let cmd = args[0].as_str();
    let rest = &args[1..];
    match cmd {
        "-h" | "--help" | "help" => {
            print!("{}", help_text());
            Ok(EXIT_OK)
        }
        "-V" | "--version" => {
            println!("wb {VERSION}");
            Ok(EXIT_OK)
        }
        "init" => handle_init(rest),
        "workspace" => handle_workspace(ws_flag.as_deref(), rest),
        "doctor" => handle_doctor(ws_flag.as_deref(), rest),
        "new" => handle_new(ws_flag.as_deref(), rest),
        "status" => handle_status(ws_flag.as_deref(), rest),
        "context" => handle_context(ws_flag.as_deref(), rest),
        "progress" => handle_progress(ws_flag.as_deref(), rest),
        "next" => handle_list_field(ws_flag.as_deref(), "next", rest),
        "blocker" => handle_list_field(ws_flag.as_deref(), "blocker", rest),
        "component" => handle_list_field(ws_flag.as_deref(), "component", rest),
        "doc" => handle_list_field(ws_flag.as_deref(), "doc", rest),
        "material" => handle_list_field(ws_flag.as_deref(), "material", rest),
        "milestone" => handle_milestone(ws_flag.as_deref(), rest),
        "link" => handle_link(ws_flag.as_deref(), rest),
        "task" => handle_task(ws_flag.as_deref(), rest),
        "delivery" => handle_delivery(ws_flag.as_deref(), rest),
        "action" => handle_action(ws_flag.as_deref(), rest),
        "sync" => handle_sync(ws_flag.as_deref(), rest),
        "evidence" => handle_evidence(ws_flag.as_deref(), rest),
        "review" => handle_review(ws_flag.as_deref(), rest),
        "dashboard" => handle_dashboard(ws_flag.as_deref(), rest),
        "extensions" => handle_extensions(ws_flag.as_deref(), rest),
        _ => Err(WbError::validation(format!("未知命令: {cmd}"))),
    }
}

pub fn run_with_args<I, S>(args: I) -> i32
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    let raw: Vec<String> = args.into_iter().map(Into::into).collect();
    match dispatch(raw) {
        Ok(code) => code,
        Err(err) => {
            eprintln!("错误: {}", err.message);
            err.exit_code
        }
    }
}

pub fn run_from_env() -> i32 {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    run_with_args(raw)
}
