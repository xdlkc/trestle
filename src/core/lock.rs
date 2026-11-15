use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use super::errors::WbError;

static THREAD_LOCKS: OnceLock<Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>> = OnceLock::new();

fn thread_lock_for(path: &Path) -> Arc<Mutex<()>> {
    let key = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let map = THREAD_LOCKS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut guard = map.lock().expect("thread lock map poisoned");
    guard
        .entry(key)
        .or_insert_with(|| Arc::new(Mutex::new(())))
        .clone()
}

/// Acquire an exclusive persistent lock on `lock_path` without ever deleting the lock file.
pub fn with_persistent_lock<T, F>(lock_path: &Path, action: F) -> Result<T, WbError>
where
    F: FnOnce(&Path) -> Result<T, WbError>,
{
    if let Some(parent) = lock_path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| WbError::io(format!("无法创建锁目录 {}: {e}", parent.display())))?;
    }

    let tlock = thread_lock_for(lock_path);
    let _tguard = tlock.lock().expect("task mutex poisoned");

    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(lock_path)
        .map_err(|e| WbError::io(format!("无法打开锁文件 {}: {e}", lock_path.display())))?;

    let fd = file.as_raw_fd();
    let rc = unsafe { libc::flock(fd, libc::LOCK_EX) };
    if rc != 0 {
        let err = std::io::Error::last_os_error();
        return Err(WbError::io(format!(
            "无法获取文件锁 {}: {err}",
            lock_path.display()
        )));
    }

    let result = action(lock_path);

    unsafe {
        libc::flock(fd, libc::LOCK_UN);
    }
    drop(file);
    result
}

pub fn with_task_lock<T, F>(task_dir: &Path, action: F) -> Result<T, WbError>
where
    F: FnOnce(&Path) -> Result<T, WbError>,
{
    let lock_path = task_dir.join(".lock");
    with_persistent_lock(&lock_path, action)
}

pub fn with_workspace_lock<T, F>(ws_root: &Path, action: F) -> Result<T, WbError>
where
    F: FnOnce(&Path) -> Result<T, WbError>,
{
    let lock_path = ws_root.join(".wb-runtime").join("locks").join("workspace.lock");
    with_persistent_lock(&lock_path, action)
}
