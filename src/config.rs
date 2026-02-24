#[derive(Debug, Clone)]
pub struct LiveKitConfig {
    pub internal_url: String,
    pub external_url: String,
    pub api_key: String,
    pub api_secret: String,
}

pub struct Config {
    pub port: u16,
    pub database_url: String,
    pub test_mode: bool,
    pub livekit: LiveKitConfig,
    pub storage_path: std::path::PathBuf,
}

impl Config {
    pub fn from_env() -> Self {
        let internal_url = std::env::var("LIVEKIT_INTERNAL_URL")
            .or_else(|_| std::env::var("LIVEKIT_URL"))
            .expect("LIVEKIT_INTERNAL_URL or LIVEKIT_URL is required");
        let external_url = std::env::var("LIVEKIT_EXTERNAL_URL")
            .unwrap_or_else(|_| internal_url.clone());
        let api_key = std::env::var("LIVEKIT_API_KEY")
            .expect("LIVEKIT_API_KEY is required");
        let api_secret = std::env::var("LIVEKIT_API_SECRET")
            .expect("LIVEKIT_API_SECRET is required");
            
        let livekit = LiveKitConfig {
            internal_url,
            external_url,
            api_key,
            api_secret,
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
        std::env::remove_var("ACCORD_TEST_MODE");
        std::env::remove_var("LIVEKIT_URL");
        std::env::remove_var("LIVEKIT_INTERNAL_URL");
        std::env::remove_var("LIVEKIT_EXTERNAL_URL");
        std::env::remove_var("LIVEKIT_API_KEY");
        std::env::remove_var("LIVEKIT_API_SECRET");
    }

    #[test]
    #[serial]
    #[should_panic(expected = "LIVEKIT_INTERNAL_URL or LIVEKIT_URL is required")]
    fn test_missing_livekit() {
        clear_env();
        Config::from_env();
    }

    #[test]
    #[serial]
    fn test_livekit_config() {
        clear_env();
        std::env::set_var("LIVEKIT_INTERNAL_URL", "http://livekit:7880");
        std::env::set_var("LIVEKIT_EXTERNAL_URL", "wss://livekit.example.com");
        std::env::set_var("LIVEKIT_API_KEY", "my-api-key");
        std::env::set_var("LIVEKIT_API_SECRET", "my-api-secret");
        
        let config = Config::from_env();
        assert_eq!(config.port, 39099);
        assert_eq!(config.database_url, "sqlite:accord.db?mode=rwc");
        
        assert_eq!(config.livekit.internal_url, "http://livekit:7880");
        assert_eq!(config.livekit.external_url, "wss://livekit.example.com");
        assert_eq!(config.livekit.api_key, "my-api-key");
        assert_eq!(config.livekit.api_secret, "my-api-secret");
    }
}
