#[derive(Debug, Clone, PartialEq)]
pub enum AccordMode {
    Main,
    Sfu,
}

#[derive(Debug, Clone, PartialEq)]
pub enum VoiceBackend {
    Custom,
    LiveKit,
}

#[derive(Debug, Clone)]
pub struct SfuConfig {
    pub main_url: String,
    pub node_id: String,
    pub region: String,
    pub capacity: i64,
    pub endpoint: String,
    pub heartbeat_interval_secs: u64,
}

#[derive(Debug, Clone)]
pub struct LiveKitConfig {
    pub url: String,
    pub api_key: String,
    pub api_secret: String,
}

pub struct Config {
    pub port: u16,
    pub database_url: String,
    pub test_mode: bool,
    pub mode: AccordMode,
    pub sfu: Option<SfuConfig>,
    pub voice_backend: VoiceBackend,
    pub livekit: Option<LiveKitConfig>,
    pub storage_path: std::path::PathBuf,
}

impl Config {
    pub fn from_env() -> Self {
        let mode = match std::env::var("ACCORD_MODE")
            .unwrap_or_else(|_| "main".to_string())
            .to_lowercase()
            .as_str()
        {
            "sfu" => AccordMode::Sfu,
            _ => AccordMode::Main,
        };

        let sfu = if mode == AccordMode::Sfu {
            let main_url =
                std::env::var("ACCORD_MAIN_URL").expect("ACCORD_MAIN_URL is required in SFU mode");
            let node_id = std::env::var("ACCORD_SFU_NODE_ID")
                .expect("ACCORD_SFU_NODE_ID is required in SFU mode");
            let region = std::env::var("ACCORD_SFU_REGION")
                .expect("ACCORD_SFU_REGION is required in SFU mode");
            let capacity: i64 = std::env::var("ACCORD_SFU_CAPACITY")
                .expect("ACCORD_SFU_CAPACITY is required in SFU mode")
                .parse()
                .expect("ACCORD_SFU_CAPACITY must be a valid integer");
            let endpoint = std::env::var("ACCORD_SFU_ENDPOINT")
                .expect("ACCORD_SFU_ENDPOINT is required in SFU mode");
            let heartbeat_interval_secs: u64 = std::env::var("ACCORD_SFU_HEARTBEAT_INTERVAL")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(25);

            Some(SfuConfig {
                main_url,
                node_id,
                region,
                capacity,
                endpoint,
                heartbeat_interval_secs,
            })
        } else {
            None
        };

        let voice_backend = match std::env::var("ACCORD_VOICE_BACKEND")
            .unwrap_or_else(|_| "custom".to_string())
            .to_lowercase()
            .as_str()
        {
            "livekit" => VoiceBackend::LiveKit,
            _ => VoiceBackend::Custom,
        };

        let livekit = if voice_backend == VoiceBackend::LiveKit {
            let url = std::env::var("LIVEKIT_URL")
                .expect("LIVEKIT_URL is required when ACCORD_VOICE_BACKEND=livekit");
            let api_key = std::env::var("LIVEKIT_API_KEY")
                .expect("LIVEKIT_API_KEY is required when ACCORD_VOICE_BACKEND=livekit");
            let api_secret = std::env::var("LIVEKIT_API_SECRET")
                .expect("LIVEKIT_API_SECRET is required when ACCORD_VOICE_BACKEND=livekit");
            Some(LiveKitConfig {
                url,
                api_key,
                api_secret,
            })
        } else {
            None
        };

        let storage_path = std::env::var("ACCORD_STORAGE_PATH")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| std::path::PathBuf::from("./cdn"));

        Self {
            port: std::env::var("PORT")
                .ok()
                .and_then(|p| p.parse().ok())
                .unwrap_or(39099),
            database_url: std::env::var("DATABASE_URL")
                .unwrap_or_else(|_| "sqlite:accord.db?mode=rwc".to_string()),
            test_mode: std::env::var("ACCORD_TEST_MODE")
                .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
                .unwrap_or(false),
            mode,
            sfu,
            voice_backend,
            livekit,
            storage_path,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    fn clear_env() {
        std::env::remove_var("PORT");
        std::env::remove_var("DATABASE_URL");
        std::env::remove_var("ACCORD_MODE");
        std::env::remove_var("ACCORD_MAIN_URL");
        std::env::remove_var("ACCORD_SFU_NODE_ID");
        std::env::remove_var("ACCORD_SFU_REGION");
        std::env::remove_var("ACCORD_SFU_CAPACITY");
        std::env::remove_var("ACCORD_SFU_ENDPOINT");
        std::env::remove_var("ACCORD_SFU_HEARTBEAT_INTERVAL");
        std::env::remove_var("ACCORD_TEST_MODE");
        std::env::remove_var("ACCORD_VOICE_BACKEND");
        std::env::remove_var("LIVEKIT_URL");
        std::env::remove_var("LIVEKIT_API_KEY");
        std::env::remove_var("LIVEKIT_API_SECRET");
    }

    #[test]
    #[serial]
    fn test_default_config() {
        clear_env();
        let config = Config::from_env();
        assert_eq!(config.port, 39099);
        assert_eq!(config.database_url, "sqlite:accord.db?mode=rwc");
        assert_eq!(config.mode, AccordMode::Main);
        assert!(config.sfu.is_none());
    }

    #[test]
    #[serial]
    fn test_port_from_env() {
        clear_env();
        std::env::set_var("PORT", "8080");
        let config = Config::from_env();
        assert_eq!(config.port, 8080);
    }

    #[test]
    #[serial]
    fn test_database_url_from_env() {
        clear_env();
        std::env::set_var("DATABASE_URL", "sqlite:test.db");
        let config = Config::from_env();
        assert_eq!(config.database_url, "sqlite:test.db");
    }

    #[test]
    #[serial]
    fn test_invalid_port_falls_back_to_default() {
        clear_env();
        std::env::set_var("PORT", "not_a_number");
        let config = Config::from_env();
        assert_eq!(config.port, 39099);
    }

    #[test]
    #[serial]
    fn test_sfu_mode_config() {
        clear_env();
        std::env::set_var("ACCORD_MODE", "sfu");
        std::env::set_var("ACCORD_MAIN_URL", "http://localhost:3000");
        std::env::set_var("ACCORD_SFU_NODE_ID", "sfu-1");
        std::env::set_var("ACCORD_SFU_REGION", "us-east");
        std::env::set_var("ACCORD_SFU_CAPACITY", "100");
        std::env::set_var("ACCORD_SFU_ENDPOINT", "ws://sfu-1:4000");
        let config = Config::from_env();
        assert_eq!(config.mode, AccordMode::Sfu);
        let sfu = config.sfu.unwrap();
        assert_eq!(sfu.main_url, "http://localhost:3000");
        assert_eq!(sfu.node_id, "sfu-1");
        assert_eq!(sfu.region, "us-east");
        assert_eq!(sfu.capacity, 100);
        assert_eq!(sfu.endpoint, "ws://sfu-1:4000");
        assert_eq!(sfu.heartbeat_interval_secs, 25);
    }

    #[test]
    #[serial]
    fn test_sfu_custom_heartbeat_interval() {
        clear_env();
        std::env::set_var("ACCORD_MODE", "sfu");
        std::env::set_var("ACCORD_MAIN_URL", "http://localhost:3000");
        std::env::set_var("ACCORD_SFU_NODE_ID", "sfu-1");
        std::env::set_var("ACCORD_SFU_REGION", "us-east");
        std::env::set_var("ACCORD_SFU_CAPACITY", "50");
        std::env::set_var("ACCORD_SFU_ENDPOINT", "ws://sfu-1:4000");
        std::env::set_var("ACCORD_SFU_HEARTBEAT_INTERVAL", "10");
        let config = Config::from_env();
        let sfu = config.sfu.unwrap();
        assert_eq!(sfu.heartbeat_interval_secs, 10);
    }

    #[test]
    #[serial]
    #[should_panic(expected = "ACCORD_MAIN_URL is required")]
    fn test_sfu_mode_missing_main_url_panics() {
        clear_env();
        std::env::set_var("ACCORD_MODE", "sfu");
        Config::from_env();
    }

    #[test]
    #[serial]
    fn test_main_mode_explicit() {
        clear_env();
        std::env::set_var("ACCORD_MODE", "main");
        let config = Config::from_env();
        assert_eq!(config.mode, AccordMode::Main);
        assert!(config.sfu.is_none());
    }

    #[test]
    #[serial]
    fn test_default_voice_backend_is_custom() {
        clear_env();
        let config = Config::from_env();
        assert_eq!(config.voice_backend, VoiceBackend::Custom);
        assert!(config.livekit.is_none());
    }

    #[test]
    #[serial]
    fn test_livekit_voice_backend() {
        clear_env();
        std::env::set_var("ACCORD_VOICE_BACKEND", "livekit");
        std::env::set_var("LIVEKIT_URL", "wss://livekit.example.com");
        std::env::set_var("LIVEKIT_API_KEY", "my-api-key");
        std::env::set_var("LIVEKIT_API_SECRET", "my-api-secret");
        let config = Config::from_env();
        assert_eq!(config.voice_backend, VoiceBackend::LiveKit);
        let lk = config.livekit.unwrap();
        assert_eq!(lk.url, "wss://livekit.example.com");
        assert_eq!(lk.api_key, "my-api-key");
        assert_eq!(lk.api_secret, "my-api-secret");
    }

    #[test]
    #[serial]
    #[should_panic(expected = "LIVEKIT_URL is required")]
    fn test_livekit_backend_missing_url_panics() {
        clear_env();
        std::env::set_var("ACCORD_VOICE_BACKEND", "livekit");
        Config::from_env();
    }
}
