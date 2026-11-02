//! Per-user folders for the app's own files (recovery journals, recent files), following each
//! platform's convention. Written here rather than taken from the `dirs` crate, whose
//! dependency chain includes an MPL-2.0 crate.

use std::path::PathBuf;

fn env(name: &str) -> Option<PathBuf> {
    std::env::var_os(name).filter(|v| !v.is_empty()).map(PathBuf::from)
}

fn home() -> Option<PathBuf> {
    if cfg!(windows) { env("USERPROFILE") } else { env("HOME") }
}

/// Machine-local app data: `%LOCALAPPDATA%`, `~/Library/Application Support`, or
/// `$XDG_DATA_HOME` (default `~/.local/share`).
pub fn data_local_dir() -> Option<PathBuf> {
    if cfg!(windows) {
        env("LOCALAPPDATA")
    } else if cfg!(target_os = "macos") {
        home().map(|h| h.join("Library").join("Application Support"))
    } else {
        env("XDG_DATA_HOME").or_else(|| home().map(|h| h.join(".local").join("share")))
    }
}

/// Settings: `%APPDATA%`, `~/Library/Application Support`, or `$XDG_CONFIG_HOME` (default
/// `~/.config`).
pub fn config_dir() -> Option<PathBuf> {
    if cfg!(windows) {
        env("APPDATA")
    } else if cfg!(target_os = "macos") {
        home().map(|h| h.join("Library").join("Application Support"))
    } else {
        env("XDG_CONFIG_HOME").or_else(|| home().map(|h| h.join(".config")))
    }
}
