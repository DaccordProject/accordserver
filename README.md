# Accord Server

A self-hosted Discord-like chat and voice server backend, built in Rust with [Axum](https://github.com/tokio-rs/axum). Designed as the backend for a [Godot](https://godotengine.org/) game client.

## Features

- **User Registration & Login** — Register with username/password, login to get bearer tokens, logout to revoke tokens. Passwords hashed with Argon2id.
- **REST API** — Full CRUD for users, spaces (guilds), channels, messages, members, roles, bans, invites, reactions, emojis, and bot applications
- **Public Spaces** — Spaces can be marked public, allowing anyone to join without an invite
- **WebSocket Gateway** — Real-time event streaming with intent-based filtering, heartbeats, and session management
- **Voice** — Join/leave voice channels powered by [LiveKit](https://livekit.io/) for managed WebRTC
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

The server creates a SQLite database by default and runs migrations automatically on startup. Set `DATABASE_URL` to a `postgres://` connection string to use PostgreSQL instead (see [Database](#database)).

## Configuration

All configuration is done via environment variables.

| Variable | Default | Description |
|---|---|---|
| `PORT` | `39099` | Server listen port |
| `DATABASE_URL` | `sqlite:data/accord.db?mode=rwc` | Database connection string (SQLite or PostgreSQL) |
| `RUST_LOG` | `accordserver=debug,tower_http=debug` | Tracing log filter |
| `LIVEKIT_INTERNAL_URL` | | LiveKit server URL for server communication (e.g. `http://livekit:7880`) |
| `LIVEKIT_EXTERNAL_URL` | | LiveKit server URL for client connections (e.g. `wss://livekit.example.com`) |
| `LIVEKIT_API_KEY` | | LiveKit API key |
| `LIVEKIT_API_SECRET` | | LiveKit API secret |

## Database

Accord supports both **SQLite** and **PostgreSQL** as database backends. The backend is chosen automatically based on the `DATABASE_URL` format.

### SQLite (default)

No setup required. The server creates the database file automatically on startup.

```bash
# Default — creates data/accord.db in the working directory
DATABASE_URL=sqlite:data/accord.db?mode=rwc

# Docker — persisted via volume mount
DATABASE_URL=sqlite:/app/data/accord.db?mode=rwc
```

### PostgreSQL

Set `DATABASE_URL` to a PostgreSQL connection string:

```bash
DATABASE_URL=postgres://accord:yourpassword@localhost/accord
```

On first startup the server will automatically:
1. Connect to the `postgres` maintenance database
2. Create the application database if it doesn't exist
3. Grant schema privileges if needed (handles PG 15+ restrictions)
4. Run all migrations

**Requirements:**
- The PostgreSQL **role** (user) must already exist — the server cannot create roles
- The role must have the `CREATEDB` privilege, **or** the database must already exist
- If the database was created externally, the role should be the database owner for migrations to work

**Password special characters:** If your password contains special characters, URL-encode them in `DATABASE_URL`:

| Character | Encoded |
|---|---|
| `!` | `%21` |
| `@` | `%40` |
| `#` | `%23` |
| `$` | `%24` |
| `%` | `%25` |
| `&` | `%26` |
| `/` | `%2F` |

Example: password `hunter2!` becomes `postgres://accord:hunter2%21@localhost/accord`

## Docker

The server image is published to GHCR:

```
ghcr.io/daccordproject/accordserver
```

### Docker Compose (SQLite)

```yaml
services:
  accordserver:
    image: ghcr.io/daccordproject/accordserver:latest
    ports:
      - "39099:39099"
    volumes:
      - accord-data:/app/data
    environment:
      DATABASE_URL: sqlite:/app/data/accord.db?mode=rwc
      RUST_LOG: accordserver=debug,tower_http=debug
      LIVEKIT_INTERNAL_URL: http://livekit:7880
      LIVEKIT_EXTERNAL_URL: ws://localhost:7880
      LIVEKIT_API_KEY: devkey
      LIVEKIT_API_SECRET: secret
    depends_on:
      - livekit

  livekit:
    image: livekit/livekit-server:latest
    command: --dev --keys '{"devkey": "secret"}'
    ports:
      - "7880:7880"
      - "7881:7881"
      - "7882:7882/udp"

volumes:
  accord-data:
```

### Docker Compose (PostgreSQL)

A ready-to-use compose file is provided at `docker-compose.postgres.yml`:

```bash
docker compose -f docker-compose.postgres.yml up -d
```

Or configure it manually:

```yaml
services:
  accordserver:
    image: ghcr.io/daccordproject/accordserver:latest
    ports:
      - "39099:39099"
    volumes:
      - accord-data:/app/data
    environment:
      DATABASE_URL: "postgres://accord:yourpassword@postgres/accord"
      RUST_LOG: accordserver=debug,tower_http=debug
      LIVEKIT_INTERNAL_URL: http://livekit:7880
      LIVEKIT_EXTERNAL_URL: ws://localhost:7880
      LIVEKIT_API_KEY: devkey
      LIVEKIT_API_SECRET: secret
    depends_on:
      postgres:
        condition: service_healthy
      livekit:
        condition: service_started

  postgres:
    image: postgres:17
    volumes:
      - postgres-data:/var/lib/postgresql/data
    environment:
      # These only take effect on FIRST initialization (empty data directory).
      # If you change them later, you must wipe the volume or alter the role manually.
      POSTGRES_USER: accord
      POSTGRES_PASSWORD: "yourpassword"
      POSTGRES_DB: accord
    healthcheck:
      test: ["CMD-SHELL", "pg_isready -U accord -d accord"]
      interval: 5s
      timeout: 5s
      retries: 5

  livekit:
    image: livekit/livekit-server:latest
    command: --dev --keys '{"devkey": "secret"}'
    ports:
      - "7880:7880"
      - "7881:7881"
      - "7882:7882/udp"

volumes:
  accord-data:
  postgres-data:
```

**Important notes:**
- `POSTGRES_USER`, `POSTGRES_PASSWORD`, and `POSTGRES_DB` only take effect when PostgreSQL initializes a **fresh data directory**. If the volume already has data, changing these values does nothing. To reset: stop the stack, delete the postgres volume, and start again.
- Always **quote** `POSTGRES_PASSWORD` in YAML if it contains special characters (especially `!`, which is a YAML tag indicator).
- The password in `POSTGRES_PASSWORD` and `DATABASE_URL` must match. Remember to URL-encode special characters in `DATABASE_URL`.
- The `healthcheck` and `depends_on: condition: service_healthy` ensure the server waits for PostgreSQL to be ready before connecting.

### Troubleshooting PostgreSQL

| Error | Cause | Fix |
|---|---|---|
| `role "X" does not exist` | The PostgreSQL role was never created | The volume has stale data from a previous init. Delete the postgres volume and restart, or create the role manually: `docker compose exec postgres psql -U postgres -c "CREATE ROLE accord WITH LOGIN PASSWORD 'pass';"` |
| `database "X" does not exist` | The database wasn't created | The server creates it automatically on startup. If it fails, check that the role has `CREATEDB` privilege, or create it manually: `docker compose exec postgres psql -U accord -c "CREATE DATABASE accord;"` |
| `permission denied for schema public` | PG 15+ restricts schema access | The server handles this automatically. If it still fails, grant manually: `docker compose exec postgres psql -U postgres -d accord -c "GRANT ALL ON SCHEMA public TO accord;"` |
| `password authentication failed` | Password mismatch between `DATABASE_URL` and Postgres | Ensure passwords match. Check for unquoted `!` in YAML and missing URL-encoding in `DATABASE_URL`. |
| Changes to `POSTGRES_USER`/`POSTGRES_DB` have no effect | Volume has existing data | PostgreSQL only reads these on first init. Delete the volume: `docker compose down -v` then `docker compose up -d` |

## Architecture

Single-binary Axum application with a REST API, WebSocket gateway, database, and LiveKit voice integration.

### Project Structure

```
src/
  main.rs           Entry point
  lib.rs            Library root
  config.rs         Config loaded from environment variables
  state.rs          Shared AppState (db, voice, dispatcher, etc.)
  error.rs          AppError enum → JSON error responses
  snowflake.rs      Snowflake ID generator
  db/               Database queries (one module per resource)
  models/           Serializable data types
  routes/           REST API handlers under /api/v1 (incl. auth)

  gateway/          WebSocket gateway (events, sessions, dispatcher)
  voice/            Voice state, signaling, LiveKit
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

Events are filtered by space membership and client intents: `spaces`, `members`, `messages`, `message_content`, `presences`, `voice_states`, and more.

## Voice

The client sends `VOICE_STATE_UPDATE` (opcode 9) through the gateway. The server returns a `voice.server_update` event containing a LiveKit URL and JWT token. The client connects to LiveKit directly; WebRTC and signaling are handled by LiveKit internally.

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
