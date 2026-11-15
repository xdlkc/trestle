use std::fs;
use std::path::{Path, PathBuf};

use crate::core::errors::WbError;

pub fn locate_workspace(cli_workspace: Option<&str>, cwd: Option<&Path>) -> Result<PathBuf, WbError> {
    if let Some(ws_str) = cli_workspace {
        let trimmed = ws_str.trim();
        if !trimmed.is_empty() {
            let cand = PathBuf::from(trimmed);
            let resolved = fs::canonicalize(&cand).unwrap_or(cand);
            if !resolved.join(".wb.yml").is_file() {
                return Err(WbError::not_found(format!(
                    "指定的工作台目录无效（未找到 .wb.yml）: {}，请检查路径或运行 `wb init`",
                    resolved.display()
                )));
            }
            return Ok(resolved);
        }
    }

    if let Ok(env_root) = std::env::var("WB_ROOT") {
        let trimmed = env_root.trim();
        if !trimmed.is_empty() {
            let cand = PathBuf::from(trimmed);
            let resolved = fs::canonicalize(&cand).unwrap_or(cand);
            if !resolved.join(".wb.yml").is_file() {
                return Err(WbError::not_found(format!(
                    "环境变量 WB_ROOT 指向的目录无效（未找到 .wb.yml）: {}，请运行 `wb init`",
                    resolved.display()
                )));
            }
            return Ok(resolved);
        }
    }

    let start_owned = match cwd {
        Some(c) => c.to_path_buf(),
        None => std::env::current_dir()
            .map_err(|e| WbError::io(format!("获取当前工作目录失败: {e}")))?,
    };
    let start = fs::canonicalize(&start_owned).unwrap_or(start_owned);

    let mut cur: Option<&Path> = Some(start.as_path());
    while let Some(dir) = cur {
        if dir.join(".wb.yml").is_file() {
            return Ok(dir.to_path_buf());
        }
        cur = dir.parent();
    }

    Err(WbError::not_found(
        "未在当前目录及其上级目录中找到 .wb.yml；请先运行 `wb init` 初始化工作台，或通过 `--workspace` / `WB_ROOT` 指定路径。",
    ))
}
