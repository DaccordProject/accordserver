# Accord Server

A self-hosted Discord-like chat and voice server backend, built in Rust with [Axum](https://github.com/tokio-rs/axum). Designed as the backend for a [Godot](https://godotengine.org/) game client.

## Features

- **User Registration & Login** — Register with username/password, login to get bearer tokens, logout to revoke tokens. Passwords hashed with Argon2id.
- **REST API** — Full CRUD for users, spaces (guilds), channels, messages, members, roles, bans, invites, reactions, emojis, and bot applications
- **Public Spaces** — Spaces can be marked public, allowing anyone to join without an invite
- **WebSocket Gateway** — Real-time event streaming with intent-based filtering, heartbeats, and session management
- **Voice** — Join/leave voice channels with two backend options:
  - **Custom SFU** — Built-in WebRTC signaling relay with distributed SFU node management
  - **LiveKit** — Integration with [LiveKit](https://livekit.io/) for managed WebRTC
- **SQLite** — Lightweight persistence with automatic migrations (WAL mode)
- **Snowflake IDs** — Discord-style unique ID generation for all entities
- **Authorization** — Role-based permission system with per-handler enforcement. Space owners get implicit administrator. New spaces grant sensible default permissions (view, send, react, connect, etc.) to all members via the `@everyone` role.
- **Rate Limiting** — Token-bucket rate limiter (60 req/min + 10 burst per user) with `X-RateLimit-*` and `Retry-After` headers
- **Secure Token Storage** — Tokens hashed with SHA-256 before database storage
- **Bot Support** — Application/bot token authentication alongside user bearer tokens

## Quick Start

```bash
# Build
cargo build

# Run (starts on port 39099)
cargo run

# Run tests
cargo test
```

The server creates an `accord.db` SQLite database in the working directory and runs migrations automatically on startup.

## Configuration

All configuration is done via environment variables.

| Variable | Default | Description |
|---|---|---|
| `PORT` | `3000` | Server listen port |
| `DATABASE_URL` | `sqlite:accord.db?mode=rwc` | SQLite connection string |
| `RUST_LOG` | `accordserver=debug,tower_http=debug` | Tracing log filter |
| `ACCORD_MODE` | `main` | `main` (full server) or `sfu` (forwarding node) |
| `ACCORD_VOICE_BACKEND` | `custom` | `custom` (built-in SFU) or `livekit` |

### LiveKit Backend

Required when `ACCORD_VOICE_BACKEND=livekit`:

| Variable | Description |
|---|---|
| `LIVEKIT_URL` | LiveKit server URL (e.g. `wss://livekit.example.com`) |
| `LIVEKIT_API_KEY` | LiveKit API key |
| `LIVEKIT_API_SECRET` | LiveKit API secret |

### SFU Mode

Required when `ACCORD_MODE=sfu`:

| Variable | Description |
|---|---|
| `ACCORD_MAIN_URL` | Base URL of the main server |
| `ACCORD_SFU_NODE_ID` | Unique ID for this SFU node |
| `ACCORD_SFU_REGION` | Region label (e.g. `us-east`) |
| `ACCORD_SFU_CAPACITY` | Max concurrent sessions |
| `ACCORD_SFU_ENDPOINT` | Publicly reachable address for clients |
| `ACCORD_SFU_HEARTBEAT_INTERVAL` | Seconds between heartbeats (default: `25`) |

## Docker

The server image is published to GHCR:

```
ghcr.io/daccordproject/accordserver
```

### Docker Compose

```yaml
services:
  accordserver:
    image: ghcr.io/daccordproject/accordserver:latest
    ports:
      - "39099:39099"
    volumes:
      - accord-data:/app/data
    environment:
      PORT: 39099
      DATABASE_URL: sqlite:/app/data/accord.db?mode=rwc
      RUST_LOG: accordserver=debug,tower_http=debug

volumes:
  accord-data:
```

## Architecture

Single-binary Axum application that runs in two modes:

- **Main mode** (default) — Full server with REST API, WebSocket gateway, database, and SFU node management
- **SFU mode** — Lightweight forwarding node that registers with the main server, heartbeats, and exposes a `/health` endpoint

### Project Structure

```
src/
  main.rs           Entry point — branches on AccordMode
  lib.rs            Library root
  config.rs         Config loaded from environment variables
  state.rs          Shared AppState (db, voice, dispatcher, etc.)
  error.rs          AppError enum → JSON error responses
  snowflake.rs      Snowflake ID generator
  sfu_client.rs     HTTP client for SFU ↔ main server communication
  sfu_runtime.rs    SFU node lifecycle (register, heartbeat, shutdown)
  db/               Database queries (one module per resource)
  models/           Serializable data types
  routes/           REST API handlers under /api/v1 (incl. auth)

  gateway/          WebSocket gateway (events, sessions, dispatcher)
  voice/            Voice state, SFU allocation, signaling, LiveKit
  middleware/       Auth, permissions, and rate limiting
migrations/         SQLite migration files
tests/              Integration and E2E tests
```

## API Overview

All REST endpoints live under `/api/v1`. The gateway WebSocket is at `/ws`.

### Response Format

```json
{ "data": { "id": "123", "name": "..." } }

{ "data": [...], "cursor": { "after": "last_id", "has_more": true } }

{ "error": { "code": "not_found", "message": "..." } }
```

### Key Endpoints

| Group | Endpoints |
|---|---|
| Auth | `POST /auth/register`, `POST /auth/login`, `POST /auth/logout` |
| Users | `GET/PATCH /users/@me`, `GET /users/{id}`, `GET /users/@me/spaces` |
| Spaces | CRUD `/spaces`, channels, public join (`POST /spaces/{id}/join`) |
| Channels | CRUD `/channels/{id}` |
| Messages | CRUD, bulk delete, pins, typing indicators |
| Members | List, search, get, update, kick, role assignment |
| Roles | CRUD, reordering |
| Bans | List, get, create, remove |
| Invites | CRUD, accept; space-level and channel-level |
| Reactions | Add/remove per-user, list, bulk remove |
| Emojis | CRUD with role restrictions |
| Voice | Join/leave, regions, status, backend info |
| Applications | Bot app CRUD, token reset |
| Gateway | `GET /gateway`, `GET /gateway/bot` |

### Authentication

Register and login to obtain a bearer token:

```bash
# Register a new account
curl -X POST /api/v1/auth/register \
  -H "Content-Type: application/json" \
  -d '{"username": "alice", "password": "securepassword123"}'
# → { "data": { "user": {...}, "token": "..." } }

# Login
curl -X POST /api/v1/auth/login \
  -H "Content-Type: application/json" \
  -d '{"username": "alice", "password": "securepassword123"}'
# → { "data": { "user": {...}, "token": "..." } }
```

Use the token in subsequent requests:

```
Authorization: Bearer <user_token>
Authorization: Bot <bot_token>
```

Passwords are hashed with Argon2id. Tokens are hashed with SHA-256 before storage. All API endpoints require authentication except `POST /auth/register`, `POST /auth/login`, `GET /gateway`, and `GET /health`.

### Authorization

Every route handler enforces permission checks. Permissions are resolved from the `@everyone` role plus any roles assigned to the member. Space owners have implicit `administrator` access.

| Permission | Required For |
|---|---|
| `view_channel` | Reading spaces, channels, messages, members |
| `send_messages` | Sending messages, typing indicators |
| `manage_channels` | Creating, updating, deleting channels |
| `manage_messages` | Deleting others' messages, pinning, bulk delete |
| `manage_roles` | Role CRUD, assigning/removing roles |
| `manage_nicknames` | Updating other members' nicknames |
| `kick_members` | Kicking members from a space |
| `ban_members` | Banning/unbanning members |
| `create_invites` | Creating invites |
| `manage_emojis` | Emoji CRUD |
| `add_reactions` | Adding reactions to messages |
| `connect` | Joining voice channels |
| `change_nickname` | Updating own nickname |

## Gateway Protocol

Clients connect via WebSocket at `/ws`. The server sends a `HELLO` with `heartbeat_interval`, the client responds with `IDENTIFY` (token + intents), and the server sends `READY` to begin the event stream.

| Opcode | Name | Direction |
|---|---|---|
| 0 | EVENT | server → client |
| 1 | HEARTBEAT | bidirectional |
| 2 | IDENTIFY | client → server |
| 3 | RESUME | client → server |
| 4 | HEARTBEAT_ACK | server → client |
| 5 | HELLO | server → client |
| 6 | RECONNECT | server → client |
| 7 | INVALID_SESSION | server → client |
| 8 | PRESENCE_UPDATE | client → server |
| 9 | VOICE_STATE_UPDATE | client → server |
| 10 | REQUEST_MEMBERS | client → server |
| 11 | VOICE_SIGNAL | client → server |

Events are filtered by space membership and client intents: `spaces`, `members`, `messages`, `message_content`, `presences`, `voice_states`, and more.

## Voice

Two voice backends are supported, selected via `ACCORD_VOICE_BACKEND`:

**Custom SFU** — The client sends `VOICE_STATE_UPDATE` (opcode 9) through the gateway. The server allocates an SFU node and returns a `voice.server_update` event with the SFU endpoint. The client connects directly to the SFU for WebRTC, using opcode 11 (`VOICE_SIGNAL`) for signaling.

**LiveKit** — Same initial flow, but `voice.server_update` contains a LiveKit URL and JWT token. The client connects to LiveKit directly; signaling is handled by LiveKit internally.

## Development

```bash
cargo check          # Fast compile check
cargo test           # Run all tests
cargo test test_name # Run a single test
cargo clippy         # Lint
cargo fmt            # Format
```

Tests use in-memory SQLite databases with per-test isolation — no external services required. The test suite includes authorization enforcement tests (`tests/security.rs`) and rate limiting tests. See [`tests/README.md`](tests/README.md) for details on the test infrastructure.

## License

See [LICENSE](LICENSE) for details.
