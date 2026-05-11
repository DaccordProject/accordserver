# Accord Server

A self-hosted Discord-like chat and voice server backend, built in Rust with [Axum](https://github.com/tokio-rs/axum). Designed as the backend for a [Godot](https://godotengine.org/) game client.

[![Deploy on Railway](https://railway.com/button.svg)](https://railway.com/new/template?template=https%3A%2F%2Fgithub.com%2Fdaccordproject%2Faccordserver)

> The button above does an **ad-hoc repo deploy**: Railway clones the default branch, builds from `Dockerfile`, and applies `railway.json`. It does **not** pre-prompt for env vars or provision Postgres — Railway only supports that for *published templates*, which must be created through the dashboard (see [RAILWAY_TEMPLATE.md](RAILWAY_TEMPLATE.md) for the walkthrough). After the initial deploy, follow [Deploying to Railway](#deploying-to-railway) to add Postgres / a volume, env vars, and a public domain.

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

## Deploying to Railway

Click the [Deploy on Railway](#accord-server) button at the top of this README to provision a service from this repo. Railway builds with the included `Dockerfile` and reads configuration from `railway.json` (healthcheck `/health`, restart-on-failure).

> **Note:** Railway deploys the repo's **default branch**. `railway.json` and any env-var defaults only take effect once they're merged into `main` — if you click the button against a branch where this file hasn't landed yet, Railway falls back to auto-detection and may build with different defaults.

Railway injects a `PORT` env var at runtime; the server binds to it automatically — no manual port config required.

### After the deploy completes

1. **Add a Volume for SQLite persistence.** In your Railway service, click *Settings → Volumes → New Volume* and mount it at `/app/data`. Without a volume the database is wiped on every redeploy.
   - Or skip the volume and use PostgreSQL instead (see next step).
2. **(Optional) Add PostgreSQL.** Click *+ New → Database → PostgreSQL* in your Railway project, then in the accordserver service set `DATABASE_URL=${{Postgres.DATABASE_URL}}` so the two services stay linked via reference variables.
3. **Generate a `TOTP_ENCRYPTION_KEY`.** The deploy form prompts for one; if you skipped it, run `openssl rand -hex 32` locally and add it in *Variables*. Without this, TOTP secrets are stored in plaintext.
4. **Expose a public domain.** In *Settings → Networking* click *Generate Domain* — Railway proxies it to the container port, terminates TLS, and natively supports the gateway WebSocket at `/ws`.

### Voice / LiveKit on Railway

Railway only exposes **TCP** ports. LiveKit's signaling is WebSocket-over-TCP (fine), but the **media plane requires UDP** for WebRTC — which Railway does not route. This means:

- **You cannot self-host LiveKit on Railway.** Audio/video will fail to negotiate even though signaling connects.
- **Use [LiveKit Cloud](https://livekit.io/cloud)** (or any UDP-capable host) and point Accord at it:

  ```
  LIVEKIT_INTERNAL_URL=wss://your-project.livekit.cloud
  LIVEKIT_EXTERNAL_URL=wss://your-project.livekit.cloud
  LIVEKIT_API_KEY=<from LiveKit dashboard>
  LIVEKIT_API_SECRET=<from LiveKit dashboard>
  ```

- Leave the four `LIVEKIT_*` vars unset to disable voice entirely; chat, presence, and the gateway WebSocket work without them.

### Railway-specific notes

- The gateway WebSocket (`/ws`) works out of the box — Railway's edge proxy supports the HTTP Upgrade handshake.
- The `Dockerfile`'s `ENV PORT=39099` is overridden by Railway's injected `PORT`; no change needed.
- The `/health` endpoint is used by Railway for liveness — startup must complete within `healthcheckTimeout` (100s, set in `railway.json`).

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

## Plugins

Accord supports installable plugins that run inside spaces. Plugins are uploaded as `.daccord-plugin` bundles (ZIP files) and can power activities, bots, themes, or custom commands.

### Plugin Types

| Type | Description |
|---|---|
| `activity` | Interactive activities (games, whiteboards, etc.) with session and participant management |
| `bot` | Automated bots that respond to events |
| `theme` | Visual themes for the client |
| `command` | Custom slash commands |

### Bundle Format

A `.daccord-plugin` bundle is a ZIP file containing:

```
plugin.json          # Required — plugin manifest
bin/plugin.elf       # Required for scripted plugins — the ELF binary
plugin.sig           # Required for native plugins — signature file
assets/icon.png      # Optional — plugin icon
```

The `plugin.json` manifest defines the plugin metadata:

```json
{
  "name": "My Plugin",
  "type": "activity",
  "runtime": "scripted",
  "description": "A cool plugin",
  "version": "1.0.0",
  "entry_point": "main",
  "max_participants": 4,
  "max_spectators": 10,
  "lobby": true,
  "canvas_size": [800, 600],
  "permissions": [],
  "data_topics": []
}
```

| Field | Required | Description |
|---|---|---|
| `name` | Yes | Plugin name (max 100 characters) |
| `type` | Yes | One of: `activity`, `bot`, `theme`, `command` |
| `runtime` | Yes | `scripted` (ELF binary) or `native` (full bundle, requires signature) |
| `description` | No | Short description |
| `version` | No | Semver version string |
| `entry_point` | No | Entry point function name |
| `max_participants` | No | Max player slots (0 = unlimited) |
| `max_spectators` | No | Max spectator slots |
| `lobby` | No | Whether sessions start in a lobby state before running |
| `canvas_size` | No | `[width, height]` for activity rendering (max 1280x720) |
| `permissions` | No | Permissions the plugin requests |
| `data_topics` | No | Data topics the plugin subscribes to |

### Installing a Plugin

Plugins are installed per-space. The installing user must have the `manage_space` permission.

```bash
# Upload a .daccord-plugin bundle
curl -X POST /api/v1/spaces/{space_id}/plugins \
  -H "Authorization: Bearer <token>" \
  -F "bundle=@my-plugin.daccord-plugin"
```

The server validates the bundle, extracts the manifest, and stores the plugin. A `plugin.installed` gateway event is broadcast to space members.

### Managing Plugins

```bash
# List plugins in a space (optionally filter by type)
curl /api/v1/spaces/{space_id}/plugins?type=activity \
  -H "Authorization: Bearer <token>"

# Uninstall a plugin (requires manage_space)
curl -X DELETE /api/v1/spaces/{space_id}/plugins/{plugin_id} \
  -H "Authorization: Bearer <token>"

# Download plugin ELF binary (scripted plugins only)
curl /api/v1/plugins/{plugin_id}/elf \
  -H "Authorization: Bearer <token>" -o plugin.elf

# Download full plugin bundle
curl /api/v1/plugins/{plugin_id}/bundle \
  -H "Authorization: Bearer <token>" -o plugin.zip

# Get plugin icon
curl /api/v1/plugins/{plugin_id}/icon \
  -H "Authorization: Bearer <token>" -o icon.png
```

### Activity Sessions

Activity plugins support multiplayer sessions with lobby, running, and ended states.

```bash
# Create a session (starts in "lobby" if plugin has lobby enabled)
curl -X POST /api/v1/plugins/{plugin_id}/sessions \
  -H "Authorization: Bearer <token>" \
  -H "Content-Type: application/json" \
  -d '{"channel_id": "123"}'

# Join as a player or spectator
curl -X POST /api/v1/plugins/{plugin_id}/sessions/{session_id}/roles \
  -H "Authorization: Bearer <token>" \
  -H "Content-Type: application/json" \
  -d '{"user_id": "456", "role": "player"}'

# Start the session (host only, transitions lobby → running)
curl -X PATCH /api/v1/plugins/{plugin_id}/sessions/{session_id} \
  -H "Authorization: Bearer <token>" \
  -H "Content-Type: application/json" \
  -d '{"state": "running"}'

# Send an action to other participants (running sessions only)
curl -X POST /api/v1/plugins/{plugin_id}/sessions/{session_id}/actions \
  -H "Authorization: Bearer <token>" \
  -H "Content-Type: application/json" \
  -d '{"type": "move", "x": 10, "y": 20}'

# End a session (host or manage_space permission)
curl -X DELETE /api/v1/plugins/{plugin_id}/sessions/{session_id} \
  -H "Authorization: Bearer <token>"
```

Session state transitions: `lobby` → `running` → `ended` (or `lobby` → `ended` to cancel).

### Gateway Events

Plugin events are broadcast over the WebSocket gateway under the `plugins` intent:

| Event | Description |
|---|---|
| `plugin.installed` | A plugin was installed in a space |
| `plugin.uninstalled` | A plugin was removed from a space |
| `plugin.session_state` | A session was created, changed state, or ended |
| `plugin.role_changed` | A participant's role changed in a session |
| `plugin.event` | An action was relayed to session participants |

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
