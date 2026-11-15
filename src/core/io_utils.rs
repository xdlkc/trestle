use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};

use chrono::{SecondsFormat, Utc};
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use tempfile::NamedTempFile;

use super::errors::WbError;
use super::yaml::{from_yaml_str, to_yaml_string};

pub fn now_iso() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}

pub fn atomic_write_bytes(path: &Path, data: &[u8]) -> Result<(), WbError> {
    let parent = path
        .parent()
        .ok_or_else(|| WbError::io(format!("路径缺少父目录: {}", path.display())))?;
    fs::create_dir_all(parent)
        .map_err(|e| WbError::io(format!("无法创建目录 {}: {e}", parent.display())))?;

    let mut tmp = NamedTempFile::new_in(parent)
        .map_err(|e| WbError::io(format!("创建临时文件失败 {}: {e}", path.display())))?;
    tmp.write_all(data)
        .map_err(|e| WbError::io(format!("写入临时文件失败 {}: {e}", path.display())))?;
    tmp.as_file()
        .sync_all()
        .map_err(|e| WbError::io(format!("同步文件刷盘失败 {}: {e}", path.display())))?;
    tmp.persist(path)
        .map_err(|e| WbError::io(format!("原子替换文件失败 {}: {}", path.display(), e.error)))?;
    Ok(())
}

pub fn atomic_write_text(path: &Path, content: &str) -> Result<(), WbError> {
    atomic_write_bytes(path, content.as_bytes())
}

pub fn atomic_write_yaml(path: &Path, data: &Value) -> Result<(), WbError> {
    let rendered = to_yaml_string(data);
    atomic_write_text(path, &rendered)
}

pub fn atomic_write_json<T: Serialize + ?Sized>(path: &Path, data: &T) -> Result<(), WbError> {
    let mut rendered = serde_json::to_string_pretty(data)
        .map_err(|e| WbError::validation(format!("JSON 序列化失败: {e}")))?;
    rendered.push('\n');
    atomic_write_text(path, &rendered)
}

pub fn read_yaml_file(path: &Path) -> Result<Value, WbError> {
    let raw = fs::read_to_string(path)
        .map_err(|e| WbError::io(format!("读取 YAML 文件失败 {}: {e}", path.display())))?;
    from_yaml_str(&raw).map_err(|e| {
        WbError::validation(format!("YAML 格式无效 {}: {}", path.display(), e.message))
    })
}

pub fn read_json_file(path: &Path) -> Result<Value, WbError> {
    let raw = fs::read_to_string(path)
        .map_err(|e| WbError::io(format!("读取 JSON 文件失败 {}: {e}", path.display())))?;
    serde_json::from_str(&raw).map_err(|e| {
        WbError::validation(format!("JSON 格式无效 {}: {e}", path.display()))
    })
}

fn lexical_normalize(path: &Path) -> PathBuf {
    let mut stack: Vec<Component<'_>> = Vec::new();
    for comp in path.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                if matches!(stack.last(), Some(Component::Normal(_))) {
                    stack.pop();
                } else if !matches!(stack.last(), Some(Component::RootDir)) {
                    stack.push(comp);
                }
            }
            _ => stack.push(comp),
        }
    }
    stack.iter().collect()
}

/// Resolve `candidate` relative to `ws_root` (following symlinks on existing ancestors)
/// and reject any path escaping `ws_root`.
pub fn ensure_within_workspace(ws_root: &Path, candidate: &Path) -> Result<PathBuf, WbError> {
    let root_canon = fs::canonicalize(ws_root)
        .map_err(|e| WbError::validation(format!("无法解析工作台路径 {}: {e}", ws_root.display())))?;

    let joined = if candidate.is_absolute() {
        candidate.to_path_buf()
    } else {
        root_canon.join(candidate)
    };
    let normalized = lexical_normalize(&joined);

    // Canonicalize the deepest existing ancestor (or the full path if it exists / is a symlink)
    let resolved = if normalized.exists() || fs::symlink_metadata(&normalized).is_ok() {
        fs::canonicalize(&normalized).map_err(|e| {
            WbError::validation(format!("无法解析路径 {}: {e}", candidate.display()))
        })?
    } else {
        let mut tail: Vec<std::ffi::OsString> = Vec::new();
        let mut probe = normalized.as_path();
        while !probe.exists() && fs::symlink_metadata(probe).is_err() {
            if let Some(name) = probe.file_name() {
                tail.push(name.to_os_string());
            } else {
                break;
            }
            match probe.parent() {
                Some(p) => probe = p,
                None => break,
            }
        }
        let mut base = fs::canonicalize(probe).map_err(|e| {
            WbError::validation(format!("无法解析路径 {}: {e}", candidate.display()))
        })?;
        for seg in tail.into_iter().rev() {
            base.push(seg);
        }
        base
    };

    if !resolved.starts_with(&root_canon) {
        return Err(WbError::validation(format!(
            "路径越界（必须位于当前工作台目录内）: {}",
            candidate.display()
        )));
    }
    Ok(resolved)
}

pub fn to_workspace_relpath(ws_root: &Path, candidate: &Path) -> Result<String, WbError> {
    let root_canon = fs::canonicalize(ws_root)
        .map_err(|e| WbError::validation(format!("无法解析工作台路径 {}: {e}", ws_root.display())))?;
    let resolved = ensure_within_workspace(ws_root, candidate)?;
    let rel = resolved.strip_prefix(&root_canon).map_err(|_| {
        WbError::validation(format!(
            "路径越界（必须位于当前工作台目录内）: {}",
            candidate.display()
        ))
    })?;
    let parts: Vec<String> = rel
        .components()
        .filter_map(|c| match c {
            Component::Normal(s) => Some(s.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect();
    Ok(parts.join("/"))
}

pub fn sha256_bytes(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    let digest = hasher.finalize();
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn sha256_file(path: &Path) -> Result<String, WbError> {
    let mut file = File::open(path)
        .map_err(|e| WbError::io(format!("计算文件校验值失败 {}: {e}", path.display())))?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 65536];
    loop {
        let n = file
            .read(&mut buf)
            .map_err(|e| WbError::io(format!("读取文件失败 {}: {e}", path.display())))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    let digest = hasher.finalize();
    Ok(digest.iter().map(|b| format!("{b:02x}")).collect())
}
