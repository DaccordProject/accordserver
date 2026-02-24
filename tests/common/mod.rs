#![allow(dead_code)]

use accordserver::db;
use accordserver::gateway::dispatcher::Dispatcher;
use accordserver::middleware::auth::{create_token_hash, generate_token};
use accordserver::models::user::{CreateUser, User};
use accordserver::routes;
use accordserver::state::AppState;
use accordserver::storage;
use accordserver::voice::livekit::LiveKitClient;
use axum::body::Body;
use dashmap::DashMap;
use http::{Method, Request};
use sqlx::SqlitePool;
use std::sync::Arc;
use tokio::sync::RwLock;

/// A user created for testing, bundling the User record with its raw token.
pub struct TestUser {
    pub user: User,
    pub token: String,
    pub is_bot: bool,
}

impl TestUser {
    /// Returns the Authorization header value (e.g. `"Bearer xxx"` or `"Bot xxx"`).
    pub fn auth_header(&self) -> String {
        if self.is_bot {
            format!("Bot {}", self.token)
        } else {
            format!("Bearer {}", self.token)
        }
    }

    /// Returns the token string formatted for gateway IDENTIFY (includes prefix).
    pub fn gateway_token(&self) -> String {
        self.auth_header()
    }
}

/// Test server that owns an in-memory SQLite pool and full AppState.
/// Each instance is isolated — safe for parallel tests.
pub struct TestServer {
    pub state: AppState,
}

impl TestServer {
    /// Create a new TestServer with an in-memory SQLite database.
    pub async fn new() -> Self {
        let pool = db::create_pool("sqlite::memory:")
            .await
            .expect("failed to create test pool");

        let (dispatcher, gateway_tx) = Dispatcher::new();

        let storage_path = storage::temp_storage_path();
        // Create storage subdirectories
        for subdir in &["emojis", "sounds"] {
            std::fs::create_dir_all(storage_path.join(subdir)).ok();
        }

        let livekit_client = Some(LiveKitClient::new(
            "http://localhost:7880",
            "ws://localhost:7880",
            "devkey",
            "secret",
        ));

        let state = AppState {
            db: pool,
            voice_states: Arc::new(DashMap::new()),
            dispatcher: Arc::new(RwLock::new(Some(dispatcher))),
            gateway_tx: Arc::new(RwLock::new(Some(gateway_tx))),
            test_mode: true,
            livekit_client,
            rate_limits: Arc::new(DashMap::new()),
            storage_path,
        };

        Self { state }
    }

    /// Returns an Axum Router wired to this server's state for `oneshot()` calls.
    pub fn router(&self) -> axum::Router {
        routes::router(self.state.clone())
    }

    /// Returns a reference to the underlying SQLite pool.
    pub fn pool(&self) -> &SqlitePool {
        &self.state.db
    }

    /// Binds a TCP listener on port 0, spawns the server, and returns the base URL.
    pub async fn spawn(&self) -> String {
        let app = self.router();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        format!("http://127.0.0.1:{}", addr.port())
    }

    /// Create a user and insert a bearer token into `user_tokens` with far-future expiry.
    /// Returns a `TestUser` with `is_bot = false`.
    pub async fn create_user_with_token(&self, username: &str) -> TestUser {
        let user = db::users::create_user(
            self.pool(),
            &CreateUser {
                username: username.to_string(),
                display_name: None,
            },
        )
        .await
        .expect("failed to create test user");

        let token = generate_token();
        let token_hash = create_token_hash(&token);

        sqlx::query(
            "INSERT INTO user_tokens (token_hash, user_id, expires_at) VALUES (?, ?, '2099-12-31T23:59:59')",
        )
        .bind(&token_hash)
        .bind(&user.id)
        .execute(self.pool())
        .await
        .expect("failed to insert test token");

        TestUser {
            user,
            token,
            is_bot: false,
        }
    }

    /// Create an application with a bot user and token.
    /// Returns `(owner TestUser, bot TestUser)`.
    pub async fn create_bot_with_token(
        &self,
        owner_username: &str,
        app_name: &str,
    ) -> (TestUser, TestUser) {
        let owner = self.create_user_with_token(owner_username).await;

        let (_app, bot_token) =
            db::auth::create_application(self.pool(), &owner.user.id, app_name, "test bot")
                .await
                .expect("failed to create test application");

        // Fetch the bot user (created by create_application)
        let bot_user_id: String =
            sqlx::query_scalar("SELECT bot_user_id FROM applications WHERE name = ?")
                .bind(app_name)
                .fetch_one(self.pool())
                .await
                .expect("failed to find bot user");

        let bot_user = db::users::get_user(self.pool(), &bot_user_id)
            .await
            .expect("failed to get bot user");

        let bot = TestUser {
            user: bot_user,
            token: bot_token,
            is_bot: true,
        };

        (owner, bot)
    }

    /// Create a space owned by the given user. Returns the space ID.
    pub async fn create_space(&self, owner_id: &str, name: &str) -> String {
        let space = db::spaces::create_space(
            self.pool(),
            owner_id,
            &accordserver::models::space::CreateSpace {
                name: name.to_string(),
                slug: None,
                description: None,
                public: None,
            },
        )
        .await
        .expect("failed to create test space");
        space.id
    }

    /// Create a public space owned by the given user. Returns the space ID.
    pub async fn create_public_space(&self, owner_id: &str, name: &str) -> String {
        let space = db::spaces::create_space(
            self.pool(),
            owner_id,
            &accordserver::models::space::CreateSpace {
                name: name.to_string(),
                slug: None,
                description: None,
                public: Some(true),
            },
        )
        .await
        .expect("failed to create test public space");
        space.id
    }

    /// Ban a user from a space.
    pub async fn ban_user(&self, space_id: &str, user_id: &str, banned_by: &str) {
        db::bans::create_ban(self.pool(), space_id, user_id, Some("test ban"), banned_by)
            .await
            .expect("failed to ban test user");
    }

    /// Create a channel in the given space. Returns the channel ID.
    pub async fn create_channel(&self, space_id: &str, name: &str) -> String {
        let channel = db::channels::create_channel(
            self.pool(),
            space_id,
            &accordserver::models::channel::CreateChannel {
                name: name.to_string(),
                channel_type: "text".to_string(),
                topic: None,
                parent_id: None,
                nsfw: None,
                bitrate: None,
                user_limit: None,
                rate_limit: None,
                position: None,
            },
        )
        .await
        .expect("failed to create test channel");
        channel.id
    }

    /// Create a voice channel in the given space. Returns the channel ID.
    pub async fn create_voice_channel(&self, space_id: &str, name: &str) -> String {
        let channel = db::channels::create_channel(
            self.pool(),
            space_id,
            &accordserver::models::channel::CreateChannel {
                name: name.to_string(),
                channel_type: "voice".to_string(),
                topic: None,
                parent_id: None,
                nsfw: None,
                bitrate: None,
                user_limit: None,
                rate_limit: None,
                position: None,
            },
        )
        .await
        .expect("failed to create test voice channel");
        channel.id
    }

    /// Add a user as a member of a space.
    pub async fn add_member(&self, space_id: &str, user_id: &str) {
        db::members::add_member(self.pool(), space_id, user_id)
            .await
            .expect("failed to add test member");
    }

    /// Create a role in a space via the DB. Returns the role ID.
    pub async fn create_role(
        &self,
        space_id: &str,
        name: &str,
        permissions: &[&str],
    ) -> String {
        let input = accordserver::models::role::CreateRole {
            name: name.to_string(),
            color: None,
            hoist: None,
            permissions: Some(permissions.iter().map(|s| s.to_string()).collect()),
            mentionable: None,
        };
        let row = db::roles::create_role(self.pool(), space_id, &input)
            .await
            .expect("failed to create test role");
        row.id
    }

    /// Assign a role to a member via the DB.
    pub async fn assign_role(&self, space_id: &str, user_id: &str, role_id: &str) {
        db::members::add_role_to_member(self.pool(), space_id, user_id, role_id)
            .await
            .expect("failed to assign test role");
    }

    /// Create an admin user with a token. Sets `is_admin = true` on the user.
    pub async fn create_admin_with_token(&self, username: &str) -> TestUser {
        let test_user = self.create_user_with_token(username).await;
        sqlx::query("UPDATE users SET is_admin = 1 WHERE id = ?")
            .bind(&test_user.user.id)
            .execute(self.pool())
            .await
            .expect("failed to set admin flag");
        test_user
    }
}

// ---------------------------------------------------------------------------
// Request builder helpers
// ---------------------------------------------------------------------------

/// Build an authenticated request with no body.
pub fn authenticated_request(method: Method, uri: &str, auth_header: &str) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header("Authorization", auth_header)
        .body(Body::empty())
        .unwrap()
}

/// Build an authenticated request with a JSON body.
pub fn authenticated_json_request(
    method: Method,
    uri: &str,
    auth_header: &str,
    body: &serde_json::Value,
) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header("Authorization", auth_header)
        .header("Content-Type", "application/json")
        .body(Body::from(serde_json::to_vec(body).unwrap()))
        .unwrap()
}

/// Build an unauthenticated request with a JSON body.
pub fn json_request(method: Method, uri: &str, body: &serde_json::Value) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header("Content-Type", "application/json")
        .body(Body::from(serde_json::to_vec(body).unwrap()))
        .unwrap()
}

/// Parse a response body into a `serde_json::Value`.
pub async fn parse_body(response: axum::response::Response) -> serde_json::Value {
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

// ---------------------------------------------------------------------------
// Backward-compatible shim for existing tests
// ---------------------------------------------------------------------------

/// Creates a test app the same way the old `test_app()` did.
/// Existing tests in `http.rs` and `ws.rs` continue to work unchanged.
pub async fn test_app() -> axum::Router {
    let server = TestServer::new().await;
    routes::router(server.state)
}
