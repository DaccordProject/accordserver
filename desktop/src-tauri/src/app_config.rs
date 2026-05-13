use anyhow::{Context, Result};
use base64::Engine;
use rand::RngCore;
use serde::{Deserialize, Serialize};

use crate::paths::AppPaths;

/// Persisted configuration for the desktop app. Generated on first launch.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    pub port: u16,
    pub livekit_port: u16,
    pub livekit_api_key: String,
    pub livekit_api_secret: String,
    pub totp_encryption_key: String,
}

impl AppConfig {
    /// Loads the config from disk, or generates and persists a fresh one on
    /// first launch.
    pub fn load_or_init(paths: &AppPaths) -> Result<Self> {
        if paths.config_file.exists() {
            let text = std::fs::read_to_string(&paths.config_file)
                .with_context(|| format!("reading {:?}", paths.config_file))?;
            let cfg: AppConfig = toml::from_str(&text)
                .with_context(|| format!("parsing {:?}", paths.config_file))?;
            return Ok(cfg);
        }

        let cfg = AppConfig::generate();
        cfg.save(paths)?;
        cfg.write_livekit_yaml(paths)?;
        Ok(cfg)
    }

    fn generate() -> Self {
        let mut rng = rand::thread_rng();

        // LiveKit recommends API keys with prefix "API" + 16 hex chars.
        let mut key_bytes = [0u8; 8];
        rng.fill_bytes(&mut key_bytes);
        let livekit_api_key = format!("API{}", hex::encode(key_bytes));

        let mut secret_bytes = [0u8; 32];
        rng.fill_bytes(&mut secret_bytes);
        let livekit_api_secret = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(secret_bytes);

        let mut totp_bytes = [0u8; 32];
        rng.fill_bytes(&mut totp_bytes);
        let totp_encryption_key = hex::encode(totp_bytes);

        Self {
            port: 39099,
            livekit_port: 7880,
            livekit_api_key,
            livekit_api_secret,
            totp_encryption_key,
        }
    }

    fn save(&self, paths: &AppPaths) -> Result<()> {
        let text = toml::to_string_pretty(self).context("serialising config")?;
        std::fs::write(&paths.config_file, text)
            .with_context(|| format!("writing {:?}", paths.config_file))?;
        Ok(())
    }

    /// Writes a minimal `livekit.yaml` consumed by `livekit-server --config`.
    /// Documented at https://docs.livekit.io/home/self-hosting/deployment/
    pub fn write_livekit_yaml(&self, paths: &AppPaths) -> Result<()> {
        let yaml = serde_yaml::to_string(&serde_yaml::Value::Mapping({
            let mut m = serde_yaml::Mapping::new();
            m.insert("port".into(), self.livekit_port.into());
            m.insert(
                "bind_addresses".into(),
                serde_yaml::Value::Sequence(vec!["0.0.0.0".into()]),
            );
            m.insert(
                "rtc".into(),
                serde_yaml::Value::Mapping({
                    let mut r = serde_yaml::Mapping::new();
                    r.insert("tcp_port".into(), 7881.into());
                    r.insert("port_range_start".into(), 50000.into());
                    r.insert("port_range_end".into(), 60000.into());
                    r.insert("use_external_ip".into(), false.into());
                    r
                }),
            );
            m.insert(
                "keys".into(),
                serde_yaml::Value::Mapping({
                    let mut k = serde_yaml::Mapping::new();
                    k.insert(self.livekit_api_key.clone().into(), self.livekit_api_secret.clone().into());
                    k
                }),
            );
            m.insert(
                "logging".into(),
                serde_yaml::Value::Mapping({
                    let mut l = serde_yaml::Mapping::new();
                    l.insert("level".into(), "info".into());
                    l
                }),
            );
            m
        }))
        .context("serialising livekit.yaml")?;
        std::fs::write(&paths.livekit_yaml, yaml)
            .with_context(|| format!("writing {:?}", paths.livekit_yaml))?;
        Ok(())
    }

    pub fn livekit_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.livekit_port)
    }
}
