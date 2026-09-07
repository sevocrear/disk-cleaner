use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub apply: bool,
    #[serde(default)]
    pub yes: bool,
    #[serde(default = "default_docker_days")]
    pub docker_unused_days: u32,
    #[serde(default = "default_file_days")]
    pub file_unused_days: u32,
    #[serde(default = "default_app_days")]
    pub app_unused_days: u32,
    #[serde(default = "default_min_size")]
    pub min_file_size: u64,
    #[serde(default = "default_min_size")]
    pub min_dupe_size: u64,
    #[serde(default = "default_dedupe_keep")]
    pub dedupe_keep: String,
    #[serde(default)]
    pub extra_roots: Vec<PathBuf>,
    /// Mount points included in Deep clean file/dupe/media scans. Default: `/` only.
    /// Other mounts (e.g. `/media/...`) are walked when selected; `/` never walks the whole tree.
    #[serde(default = "default_scan_mounts")]
    pub scan_mounts: Vec<PathBuf>,
    #[serde(default)]
    pub dedupe_roots: Vec<PathBuf>,
    #[serde(default)]
    pub protect_globs: Vec<String>,
    #[serde(default)]
    pub protect_apps: Vec<String>,
    #[serde(default)]
    pub skip_docker: bool,
    #[serde(default)]
    pub skip_caches: bool,
    #[serde(default)]
    pub skip_files: bool,
    #[serde(default)]
    pub skip_apps: bool,
    #[serde(default)]
    pub skip_dupes: bool,
    #[serde(default)]
    pub skip_media: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub only: Option<String>,
    #[serde(default = "default_journal")]
    pub journal_vacuum: String,
    #[serde(default)]
    pub include_docker_volumes: bool,
    #[serde(default)]
    pub include_hf_cache: bool,
    #[serde(default)]
    pub include_opt_apps: bool,
    #[serde(default = "default_confirm")]
    pub confirm_above: u64,
    #[serde(default)]
    pub cross_fs: bool,
    #[serde(default)]
    pub home: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub report_dir: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_roots: Option<Vec<PathBuf>>,
    #[serde(default = "default_cmd_timeout")]
    pub cmd_timeout_sec: u64,
    #[serde(default = "default_docker_timeout")]
    pub docker_timeout_sec: u64,
    #[serde(default)]
    pub include_pkg_managers: bool,
    /// GUI: move to trash instead of permanent delete
    #[serde(default = "default_true")]
    pub gui_use_trash: bool,
}

fn default_docker_days() -> u32 {
    14
}
fn default_file_days() -> u32 {
    60
}
fn default_app_days() -> u32 {
    60
}
fn default_min_size() -> u64 {
    1024 * 1024
}
fn default_dedupe_keep() -> String {
    "newest".into()
}
fn default_scan_mounts() -> Vec<PathBuf> {
    vec![PathBuf::from("/")]
}
fn default_journal() -> String {
    "7d".into()
}
fn default_confirm() -> u64 {
    5 * 1024 * 1024 * 1024
}
fn default_cmd_timeout() -> u64 {
    120
}
fn default_docker_timeout() -> u64 {
    600
}
fn default_true() -> bool {
    true
}

impl Default for Config {
    fn default() -> Self {
        Self {
            apply: false,
            yes: false,
            docker_unused_days: default_docker_days(),
            file_unused_days: default_file_days(),
            app_unused_days: default_app_days(),
            min_file_size: default_min_size(),
            min_dupe_size: default_min_size(),
            dedupe_keep: default_dedupe_keep(),
            extra_roots: vec![],
            scan_mounts: default_scan_mounts(),
            dedupe_roots: vec![],
            protect_globs: vec![],
            protect_apps: vec![],
            skip_docker: false,
            skip_caches: false,
            skip_files: false,
            skip_apps: false,
            skip_dupes: false,
            skip_media: false,
            only: None,
            journal_vacuum: default_journal(),
            include_docker_volumes: false,
            include_hf_cache: false,
            include_opt_apps: false,
            confirm_above: default_confirm(),
            cross_fs: false,
            home: dirs::home_dir().unwrap_or_else(|| PathBuf::from("/")),
            report_dir: None,
            file_roots: None,
            cmd_timeout_sec: default_cmd_timeout(),
            docker_timeout_sec: default_docker_timeout(),
            include_pkg_managers: false,
            gui_use_trash: true,
        }
    }
}

impl Config {
    pub fn config_path() -> PathBuf {
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("disk-cleaner")
            .join("config.toml")
    }

    pub fn load() -> Self {
        let path = Self::config_path();
        if let Ok(text) = std::fs::read_to_string(&path) {
            if let Ok(mut cfg) = toml::from_str::<Config>(&text) {
                if cfg.home.as_os_str().is_empty() {
                    cfg.home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));
                }
                return cfg;
            }
        }
        Config::default()
    }

    pub fn save(&self) -> std::io::Result<()> {
        let path = Self::config_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = toml::to_string_pretty(self).map_err(|e| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string())
        })?;
        std::fs::write(path, text)
    }
}
