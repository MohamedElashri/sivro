use crate::{
    errors::{AppError, AppResult},
    policy::ResourcePolicy,
};
use fs2::FileExt;
use std::{
    fs::{self, File, OpenOptions},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

pub fn free_space(path: &Path, policy: &ResourcePolicy, required: u64) -> AppResult<()> {
    let available = fs2::available_space(path).map_err(AppError::io)?;
    if available < required.saturating_add(policy.disk_reserve_bytes) {
        return Err(AppError::limit(
            "Not enough free space for safe temporary work",
        ));
    }
    Ok(())
}

pub struct ScratchRoot {
    pub path: PathBuf,
    lease: Mutex<Option<File>>,
}
impl ScratchRoot {
    pub fn create(root: &Path, id: &str) -> AppResult<Arc<Self>> {
        fs::create_dir_all(root).map_err(AppError::io)?;
        if root
            .symlink_metadata()
            .map_err(AppError::io)?
            .file_type()
            .is_symlink()
        {
            return Err(AppError::invalid(
                "Scratch root must not be a symbolic link",
            ));
        }
        // A lease prevents startup recovery from deleting another live instance.
        for entry in fs::read_dir(root).map_err(AppError::io)?.flatten() {
            if !entry.file_name().to_string_lossy().starts_with("run-")
                || !entry.file_type().map(|t| t.is_dir()).unwrap_or(false)
            {
                continue;
            }
            let path = entry.path();
            if let Ok(lease) = OpenOptions::new()
                .read(true)
                .write(true)
                .open(path.join("lease"))
            {
                if FileExt::try_lock_exclusive(&lease).is_ok() {
                    drop(lease);
                    let _ = fs::remove_dir_all(&path);
                }
            }
        }
        let path = root.join(format!("run-{id}"));
        fs::create_dir(&path).map_err(AppError::io)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).map_err(AppError::io)?;
        }
        let lease = OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .open(path.join("lease"))
            .map_err(AppError::io)?;
        FileExt::try_lock_exclusive(&lease).map_err(AppError::io)?;
        Ok(Arc::new(Self {
            path,
            lease: Mutex::new(Some(lease)),
        }))
    }
    pub fn cleanup(&self) {
        if let Ok(mut lease) = self.lease.lock() {
            lease.take();
        }
        let _ = fs::remove_dir_all(&self.path);
    }
}
impl Drop for ScratchRoot {
    fn drop(&mut self) {
        self.cleanup();
    }
}

pub struct SessionScratch {
    pub path: PathBuf,
    _root: Arc<ScratchRoot>,
}
impl SessionScratch {
    pub fn create(root: Arc<ScratchRoot>, id: &str, policy: &ResourcePolicy) -> AppResult<Self> {
        free_space(&root.path, policy, 0)?;
        let path = root.path.join(id);
        fs::create_dir(&path).map_err(AppError::io)?;
        Ok(Self { path, _root: root })
    }
}
impl Drop for SessionScratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}
