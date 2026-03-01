use crate::config::MasterServerConfig;
use reqwest::Client;
use serde_json::json;

const VERSION: &str = env!("CARGO_PKG_VERSION");

async fn register(client: &Client, config: &MasterServerConfig) -> Result<(), reqwest::Error> {
    let url = format!("{}/api/v1/servers", config.url);
    client
        .post(&url)
        .bearer_auth(&config.bearer_token)
        .json(&json!({
            "id": config.server_id,
            "name": config.server_name,
            "url": config.public_url,
            "version": VERSION,
        }))
        .send()
        .await?
        .error_for_status()?;
    Ok(())
}

async fn heartbeat(client: &Client, config: &MasterServerConfig) -> Result<u16, reqwest::Error> {
    let url = format!("{}/api/v1/servers/{}/heartbeat", config.url, config.server_id);
    let resp = client
        .post(&url)
        .bearer_auth(&config.bearer_token)
        .send()
        .await?;
    let status = resp.status().as_u16();
    if status != 404 {
        resp.error_for_status()?;
    }
    Ok(status)
}

async fn deregister(client: &Client, config: &MasterServerConfig) -> Result<(), reqwest::Error> {
    let url = format!("{}/api/v1/servers/{}", config.url, config.server_id);
    client
        .delete(&url)
        .bearer_auth(&config.bearer_token)
        .send()
        .await?
        .error_for_status()?;
    Ok(())
}

pub async fn run(config: MasterServerConfig) {
    let client = Client::new();
    let interval = tokio::time::Duration::from_secs(config.heartbeat_interval);

    loop {
        match register(&client, &config).await {
            Ok(()) => {
                tracing::info!(
                    "registered with master server at {} as \"{}\" (id: {})",
                    config.url,
                    config.server_name,
                    config.server_id
                );
            }
            Err(e) => {
                tracing::warn!("failed to register with master server: {e}");
                tokio::time::sleep(interval).await;
                continue;
            }
        }

        loop {
            tokio::time::sleep(interval).await;

            match heartbeat(&client, &config).await {
                Ok(404) => {
                    tracing::warn!("master server returned 404; re-registering");
                    break;
                }
                Ok(_) => {
                    tracing::debug!("master server heartbeat ok");
                }
                Err(e) => {
                    tracing::warn!("master server heartbeat failed: {e}");
                }
            }
        }
    }
}

pub async fn deregister_from(config: &MasterServerConfig) {
    let client = Client::new();
    if let Err(e) = deregister(&client, config).await {
        tracing::warn!("failed to deregister from master server: {e}");
    } else {
        tracing::info!("deregistered from master server");
    }
}
