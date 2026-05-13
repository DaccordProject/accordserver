use anyhow::{Context, Result};
use directories::ProjectDirs;
use std::path::PathBuf;

/// Resolves platform-appropriate locations for runtime state.
///
/// macOS:   ~/Library/Application Support/gg.daccord.Accord/
/// Linux:   $XDG_DATA_HOME/accord/  (typically ~/.local/share/accord/)
/// Windows: %APPDATA%\Accord\Accord\
#[derive(Debug, Clone)]
pub struct AppPaths {
    pub data_dir: PathBuf,
    pub log_dir: PathBuf,
    pub config_file: PathBuf,
    pub livekit_yaml: PathBuf,
    pub accord_log: PathBuf,
    pub livekit_log: PathBuf,
}

impl AppPaths {
    pub fn resolve() -> Result<Self> {
        let dirs = ProjectDirs::from("gg", "daccord", "Accord")
            .context("could not determine platform data directory")?;
        let data_dir = dirs.data_dir().to_path_buf();
        let log_dir = data_dir.join("logs");

        Ok(Self {
            config_file: data_dir.join("config.toml"),
            livekit_yaml: data_dir.join("livekit.yaml"),
            accord_log: log_dir.join("accord.log"),
            livekit_log: log_dir.join("livekit.log"),
            log_dir,
            data_dir,
        })
    }

    pub fn ensure_dirs(&self) -> Result<()> {
        std::fs::create_dir_all(&self.data_dir)
            .with_context(|| format!("creating data dir {:?}", self.data_dir))?;
        std::fs::create_dir_all(&self.log_dir)
            .with_context(|| format!("creating log dir {:?}", self.log_dir))?;
        Ok(())
    }
}
