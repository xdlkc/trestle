use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Map, Value};

use super::delivery::load_delivery;
use super::errors::WbError;
use super::io_utils::{atomic_write_json, now_iso, read_json_file, sha256_file};
use super::task_store::{list_tasks, require_task_dir};

pub fn is_git_available() -> bool {
    if std::env::var("WB_DISABLE_GIT").as_deref() == Ok("1") {
        return false;
    }
    Command::new("git")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn run_git_bytes(repo_dir: &Path, args: &[&str]) -> (i32, Vec<u8>, String) {
    match Command::new("git").arg("-C").arg(repo_dir).args(args).output() {
        Ok(out) => (
            out.status.code().unwrap_or(1),
            out.stdout,
            String::from_utf8_lossy(&out.stderr).into_owned(),
        ),
        Err(e) => (1, Vec::new(), e.to_string()),
    }
}

fn run_git_text(repo_dir: &Path, args: &[&str]) -> (i32, String, String) {
    let (code, stdout, stderr) = run_git_bytes(repo_dir, args);
    (code, String::from_utf8_lossy(&stdout).into_owned(), stderr)
}

fn path_matches_allowed(rel_path: &str, allowed_paths: &[String]) -> bool {
    if allowed_paths.is_empty() {
        return true;
    }
    let norm = rel_path.trim_matches('/');
    for allowed in allowed_paths {
        let a = allowed.trim_matches('/');
        if a == "." || norm == a || norm.starts_with(&format!("{a}/")) {
            return true;
        }
    }
    false
}

fn parse_porcelain_z(raw_stdout: &[u8]) -> Vec<String> {
    let mut paths: Vec<String> = Vec::new();
    let tokens: Vec<&[u8]> = raw_stdout.split(|&b| b == 0).collect();
    let mut idx = 0usize;
    while idx < tokens.len() {
        let entry = tokens[idx];
        idx += 1;
        if entry.len() < 4 {
            continue;
        }
        let xy_first = entry[0];
        let path = String::from_utf8_lossy(&entry[3..]).into_owned();
        if (xy_first == b'R' || xy_first == b'C') && idx < tokens.len() {
            idx += 1;
        }
        if !path.is_empty() && !paths.contains(&path) {
            paths.push(path);
        }
    }
    paths
}

fn resolve_repo_dir(ws_root: &Path, repos_root_cfg: &str, item: &Value) -> PathBuf {
    let base_raw = Path::new(repos_root_cfg);
    let base = if base_raw.is_absolute() {
        base_raw.to_path_buf()
    } else {
        ws_root.join(base_raw)
    };
    let base_canon = std::fs::canonicalize(&base).unwrap_or(base);
    let target = item
        .get("worktree")
        .and_then(|v| v.as_str())
        .or_else(|| item.get("repo").and_then(|v| v.as_str()))
        .unwrap_or("");
    let t_path = Path::new(target);
    let joined = if t_path.is_absolute() {
        t_path.to_path_buf()
    } else {
        base_canon.join(t_path)
    };
    std::fs::canonicalize(&joined).unwrap_or(joined)
}

fn observe_item(ws_root: &Path, repos_root_cfg: &str, item: &Value, git_ok: bool) -> Value {
    let repo_dir = resolve_repo_dir(ws_root, repos_root_cfg, item);
    let item_id = item.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let repo = item.get("repo").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let worktree = item.get("worktree").cloned().unwrap_or(Value::Null);
    let src_branch = item
        .get("source_branch")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let tgt_branch = item
        .get("target_branch")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let compare_base = item.get("compare_base").cloned().unwrap_or(Value::Null);

    let mut obs = Map::new();
    obs.insert("id".into(), Value::String(item_id));
    obs.insert("repo".into(), Value::String(repo));
    obs.insert("worktree".into(), worktree);
    obs.insert(
        "resolved_path".into(),
        Value::String(repo_dir.to_string_lossy().into_owned()),
    );
    obs.insert("declared_source_branch".into(), Value::String(src_branch.clone()));
    obs.insert("declared_target_branch".into(), Value::String(tgt_branch));
    obs.insert("compare_base".into(), compare_base.clone());
    obs.insert("git_available".into(), Value::Bool(git_ok));
    obs.insert("status".into(), Value::String("ok".into()));
    obs.insert("actual_branch".into(), Value::Null);
    obs.insert("head_commit".into(), Value::Null);
    obs.insert("dirty_file_count".into(), Value::from(0));
    obs.insert("modified_paths".into(), Value::Array(Vec::new()));
    obs.insert("file_sha256".into(), Value::Object(Map::new()));
    obs.insert("branch_mismatch".into(), Value::Bool(false));
    obs.insert("out_of_scope_paths".into(), Value::Array(Vec::new()));
    obs.insert("error".into(), Value::Null);

    if !repo_dir.exists() {
        obs.insert("status".into(), Value::String("repo_missing".into()));
        obs.insert(
            "error".into(),
            Value::String(format!("代码目录不存在: {}", repo_dir.display())),
        );
        return Value::Object(obs);
    }

    if !git_ok {
        obs.insert("status".into(), Value::String("git_unavailable".into()));
        return Value::Object(obs);
    }

    let (rc, out, err) = run_git_text(&repo_dir, &["rev-parse", "--is-inside-work-tree"]);
    if rc != 0 || out.trim() != "true" {
        obs.insert("status".into(), Value::String("not_a_git_repo".into()));
        let msg = if err.trim().is_empty() {
            format!("目录不是有效的 Git 仓库: {}", repo_dir.display())
        } else {
            err.trim().to_string()
        };
        obs.insert("error".into(), Value::String(msg));
        return Value::Object(obs);
    }

    let mut actual_branch: Option<String> = None;
    let (rc_b, out_b, _) = run_git_text(&repo_dir, &["rev-parse", "--abbrev-ref", "HEAD"]);
    if rc_b == 0 {
        let b = out_b.trim().to_string();
        obs.insert("actual_branch".into(), Value::String(b.clone()));
        actual_branch = Some(b);
    }

    let mut head_commit: Option<String> = None;
    let (rc_h, out_h, _) = run_git_text(&repo_dir, &["rev-parse", "HEAD"]);
    if rc_h == 0 {
        let h = out_h.trim().to_string();
        obs.insert("head_commit".into(), Value::String(h.clone()));
        head_commit = Some(h);
    }

    let (rc_s, raw_s, _) = run_git_bytes(&repo_dir, &["status", "--porcelain=v1", "-z", "-uall"]);
    let modified = if rc_s == 0 {
        parse_porcelain_z(&raw_s)
    } else {
        Vec::new()
    };

    obs.insert("dirty_file_count".into(), Value::from(modified.len() as u64));
    obs.insert(
        "modified_paths".into(),
        Value::Array(modified.iter().cloned().map(Value::String).collect()),
    );

    let mut changed_since_base: Vec<String> = Vec::new();
    if let (Some(base_sha), Some(_head)) = (compare_base.as_str(), head_commit.as_ref()) {
        let range = format!("{base_sha}..HEAD");
        let (rc_d, out_d, _) = run_git_text(&repo_dir, &["diff", "--name-only", &range]);
        if rc_d == 0 {
            for ln in out_d.lines() {
                let s = ln.trim();
                if !s.is_empty() {
                    changed_since_base.push(s.to_string());
                }
            }
        }
    }

    let mut all_touched: Vec<String> = Vec::new();
    for p in modified.iter().chain(changed_since_base.iter()) {
        if !all_touched.contains(p) {
            all_touched.push(p.clone());
        }
    }

    let allowed: Vec<String> = item
        .get("allowed_paths")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();

    let out_of_scope: Vec<Value> = all_touched
        .iter()
        .filter(|p| !path_matches_allowed(p, &allowed))
        .cloned()
        .map(Value::String)
        .collect();
    obs.insert("out_of_scope_paths".into(), Value::Array(out_of_scope));

    if let Some(ab) = actual_branch {
        if ab != src_branch {
            obs.insert("branch_mismatch".into(), Value::Bool(true));
        }
    }

    let mut digests = Map::new();
    let mut candidates = modified;
    for ap in &allowed {
        if ap != "." && repo_dir.join(ap).is_file() && !candidates.contains(ap) {
            candidates.push(ap.clone());
        }
    }
    for rel in candidates {
        let fpath = repo_dir.join(&rel);
        if fpath.is_file() {
            if let Ok(hex) = sha256_file(&fpath) {
                digests.insert(rel, Value::String(hex));
            }
        }
    }
    obs.insert("file_sha256".into(), Value::Object(digests));

    Value::Object(obs)
}

pub fn sync_task(ws_root: &Path, task_id: &str, repos_root_cfg: &str) -> Result<Value, WbError> {
    require_task_dir(ws_root, task_id)?;
    let delivery = load_delivery(ws_root, task_id, false)?;
    let git_ok = is_git_available();

    let mut items_obs = Vec::new();
    if let Some(deliv) = delivery {
        if let Some(items) = deliv.get("items").and_then(|v| v.as_array()) {
            for item in items {
                items_obs.push(observe_item(ws_root, repos_root_cfg, item, git_ok));
            }
        }
    }

    let mut observation = Map::new();
    observation.insert("schema".into(), Value::from(1));
    observation.insert("task_id".into(), Value::String(task_id.to_string()));
    observation.insert("synced_at".into(), Value::String(now_iso()));
    observation.insert("git_available".into(), Value::Bool(git_ok));
    observation.insert("items".into(), Value::Array(items_obs));
    let obs_val = Value::Object(observation);

    let obs_path = ws_root
        .join(".wb-runtime")
        .join("observations")
        .join(format!("{task_id}.json"));
    atomic_write_json(&obs_path, &obs_val)?;
    Ok(obs_val)
}

pub fn load_observation(ws_root: &Path, task_id: &str) -> Result<Option<Value>, WbError> {
    let obs_path = ws_root
        .join(".wb-runtime")
        .join("observations")
        .join(format!("{task_id}.json"));
    if !obs_path.is_file() {
        return Ok(None);
    }
    Ok(Some(read_json_file(&obs_path)?))
}

pub fn sync_all_tasks(ws_root: &Path, repos_root_cfg: &str) -> Result<Vec<Value>, WbError> {
    let mut results = Vec::new();
    for task in list_tasks(ws_root, None, None, None, None)? {
        if let Some(tid) = task.get("id").and_then(|v| v.as_str()) {
            results.push(sync_task(ws_root, tid, repos_root_cfg)?);
        }
    }
    Ok(results)
}
