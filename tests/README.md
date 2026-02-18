# Accord Server — Test Suite

## Running Tests

```bash
# Run all tests
cargo test

# Run only the E2E (authenticated) tests
cargo test --test e2e

# Run only the HTTP (unauthenticated) tests
cargo test --test http

# Run only the WebSocket tests
cargo test --test ws

# Run a single test by name
cargo test test_space_crud_lifecycle

# Run with output visible
cargo test -- --nocapture
```

## Test Architecture

- **In-memory SQLite** — each test creates its own `TestServer` with a fresh `sqlite::memory:` database. Migrations run automatically on pool creation.
- **Per-test isolation** — no shared state between tests. Each `TestServer` gets its own `AppState`, dispatcher, and broadcast channel.
- **Parallel-safe** — tests run concurrently by default with no conflicts.
- **No external dependencies** — no network services or file I/O required.

## Test Files

| File | What it covers |
|---|---|
| `tests/http.rs` | Health endpoint, 404 handling, CORS headers, WebSocket upgrade rejection |
| `tests/ws.rs` | Gateway HELLO, heartbeat_interval, invalid IDENTIFY, timeout, close |
| `tests/e2e.rs` | Authenticated API: users, spaces, channels, messages, public spaces, space-level invites, gateway auth flows |
| `tests/common/mod.rs` | Shared test infrastructure (`TestServer`, `TestUser`, request helpers) |

## Infrastructure

### `TestServer`

The core test abstraction. Creates an isolated server instance with an in-memory database.

```rust
let server = TestServer::new().await;

// Get a router for oneshot() calls
let app = server.router();

// Or spawn a real TCP server for WebSocket tests
let base_url = server.spawn().await; // e.g. "http://127.0.0.1:12345"
```

**Data seeders:**

```rust
// Create a user with a valid bearer token
let alice = server.create_user_with_token("alice").await;

// Create a bot application with owner + bot user
let (owner, bot) = server.create_bot_with_token("owner", "MyBot").await;

// Create a space (owner is auto-added as member)
let space_id = server.create_space(&alice.user.id, "My Space").await;

// Create a channel in a space
let channel_id = server.create_channel(&space_id, "general").await;

// Add a user as a member of a space
server.add_member(&space_id, &bob.user.id).await;

// Create a public space (joinable without invite)
let space_id = server.create_public_space(&alice.user.id, "Open").await;

// Ban a user from a space
server.ban_user(&space_id, &bob.user.id, &alice.user.id).await;
```

### `TestUser`

Bundles a `User` record with its raw token and bot flag.

```rust
let alice = server.create_user_with_token("alice").await;
alice.user.id          // the user's snowflake ID
alice.auth_header()    // "Bearer <token>"
alice.gateway_token()  // same as auth_header() — gateway IDENTIFY expects prefix

let (_, bot) = server.create_bot_with_token("owner", "Bot").await;
bot.auth_header()      // "Bot <token>"
```

### Request builders

```rust
use common::{authenticated_request, authenticated_json_request, parse_body};

// GET/DELETE with no body
let req = authenticated_request(Method::GET, "/api/v1/users/@me", &alice.auth_header());

// POST/PATCH with JSON body
let req = authenticated_json_request(
    Method::POST,
    "/api/v1/spaces",
    &alice.auth_header(),
    &serde_json::json!({ "name": "My Space" }),
);

// Parse response body to JSON
let response = app.oneshot(req).await.unwrap();
let body = parse_body(response).await;
assert_eq!(body["data"]["name"], "My Space");
```

## Writing a New E2E Test

Follow this pattern:

```rust
#[tokio::test]
async fn test_my_feature() {
    // 1. Create an isolated server
    let server = TestServer::new().await;

    // 2. Seed test data
    let alice = server.create_user_with_token("alice").await;
    let space_id = server.create_space(&alice.user.id, "Test").await;

    // 3. Make API calls (get a fresh router for each oneshot)
    let app = server.router();
    let req = authenticated_request(
        Method::GET,
        &format!("/api/v1/spaces/{space_id}"),
        &alice.auth_header(),
    );
    let response = app.oneshot(req).await.unwrap();

    // 4. Assert
    assert_eq!(response.status(), StatusCode::OK);
    let body = parse_body(response).await;
    assert_eq!(body["data"]["name"], "Test");
}
```

> **Note:** `oneshot()` consumes the router, so call `server.router()` before each request.

## WebSocket Test Pattern

```rust
#[tokio::test]
async fn test_gateway_something() {
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;

    let base_url = server.spawn().await;
    let ws_url = base_url.replace("http://", "ws://");
    let (mut ws, _) = connect_async(format!("{ws_url}/ws")).await.unwrap();

    // 1. Receive HELLO (op=5)
    let hello: serde_json::Value = serde_json::from_str(
        &ws.next().await.unwrap().unwrap().into_text().unwrap()
    ).unwrap();
    assert_eq!(hello["op"], 5);

    // 2. Send IDENTIFY (op=2)
    ws.send(Message::Text(serde_json::json!({
        "op": 2,
        "data": {
            "token": alice.gateway_token(),
            "intents": ["messages"]
        }
    }).to_string().into())).await.unwrap();

    // 3. Receive READY (op=0, type="ready")
    let ready: serde_json::Value = serde_json::from_str(
        &ws.next().await.unwrap().unwrap().into_text().unwrap()
    ).unwrap();
    assert_eq!(ready["type"], "ready");

    // 4. Now test your gateway interaction...

    ws.close(None).await.unwrap();
}
```

## API Response Format

All API endpoints return JSON with this structure:

```json
// Success (single resource)
{ "data": { "id": "...", "name": "...", ... } }

// Success (list)
{ "data": [ ... ] }

// Success (list with pagination)
{ "data": [ ... ], "cursor": { "after": "last_id", "has_more": true } }

// Success (delete / side-effect only)
{ "data": null }

// Error
{ "error": { "code": "not_found", "message": "unknown_space" } }
```

HTTP status codes: 200 OK, 401 Unauthorized, 403 Forbidden, 404 Not Found, 429 Rate Limited.
