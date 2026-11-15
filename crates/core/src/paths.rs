use std::path::{Path, PathBuf};

/// Everything Kiln writes lives under one directory so uninstalling is deleting it.
pub fn kiln_dir() -> PathBuf {
    home().join(".kiln")
}

pub fn home() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("."))
}

pub fn global_config_path() -> PathBuf {
    kiln_dir().join("config.toml")
}

pub fn project_config_path(cwd: &Path) -> PathBuf {
    cwd.join(".kiln").join("config.toml")
}

pub fn sessions_dir() -> PathBuf {
    kiln_dir().join("sessions")
}
