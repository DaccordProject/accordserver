use reqwest::Client;
use serde_json::json;
use std::fmt;

#[derive(Debug)]
pub enum SfuClientError {
    Http(reqwest::Error),
    ServerError { status: u16, body: String },
}

impl fmt::Display for SfuClientError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SfuClientError::Http(e) => write!(f, "HTTP error: {e}"),
            SfuClientError::ServerError { status, body } => {
                write!(f, "server returned {status}: {body}")
            }
        }
    }
}

impl From<reqwest::Error> for SfuClientError {
    fn from(e: reqwest::Error) -> Self {
        SfuClientError::Http(e)
    }
}

pub struct SfuClient {
    client: Client,
    base_url: String,
    node_id: String,
    endpoint: String,
    region: String,
    capacity: i64,
    auth_token: Option<String>,
}

impl SfuClient {
    pub fn new(
        base_url: String,
        node_id: String,
        endpoint: String,
        region: String,
        capacity: i64,
    ) -> Self {
        Self {
            client: Client::new(),
            base_url,
            node_id,
            endpoint,
            region,
            capacity,
            auth_token: None,
        }
    }

    pub fn with_auth_token(mut self, token: String) -> Self {
        self.auth_token = Some(token);
        self
    }

    fn apply_auth(&self, builder: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        if let Some(ref token) = self.auth_token {
            builder.header("Authorization", format!("Bearer {token}"))
        } else {
            builder
        }
    }

    pub async fn register(&self) -> Result<(), SfuClientError> {
        let url = format!("{}/api/v1/sfu/nodes", self.base_url);
        let builder = self.client.post(&url).json(&json!({
            "id": self.node_id,
            "endpoint": self.endpoint,
            "region": self.region,
            "capacity": self.capacity,
        }));
        let resp = self.apply_auth(builder).send().await?;

        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            return Err(SfuClientError::ServerError { status, body });
        }

        Ok(())
    }

    pub async fn heartbeat(&self, current_load: i64) -> Result<(), SfuClientError> {
        let url = format!(
            "{}/api/v1/sfu/nodes/{}/heartbeat",
            self.base_url, self.node_id
        );
        let builder = self
            .client
            .post(&url)
            .json(&json!({ "current_load": current_load }));
        let resp = self.apply_auth(builder).send().await?;

        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            return Err(SfuClientError::ServerError { status, body });
        }

        Ok(())
    }

    pub async fn deregister(&self) -> Result<(), SfuClientError> {
        let url = format!("{}/api/v1/sfu/nodes/{}", self.base_url, self.node_id);
        let builder = self.client.delete(&url);
        let resp = self.apply_auth(builder).send().await?;

        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            return Err(SfuClientError::ServerError { status, body });
        }

        Ok(())
    }
}
