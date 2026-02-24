# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What This Is

Accord is a chat/voice server backend for a Godot game client. It's a Rust web server built with Axum that provides a Discord-like API with WebSocket gateway, REST endpoints, and WebRTC voice routing. Think of it as a self-hosted Discord backend.

## Build & Run Commands

- **Build:** `cargo build`
- **Run:** `cargo run` (listens on port 3000 by default)
- **Check (fast compile check):** `cargo check`
- **Run tests:** `cargo test`
- **Run a single test:** `cargo test test_name`
- **Lint:** `cargo clippy`
- **Format:** `cargo fmt`

## Environment Variables

### General
- `PORT` — server port (default: `3000`)
- `DATABASE_URL` — SQLite connection string (default: `sqlite:accord.db?mode=rwc`)
- `RUST_LOG` — tracing filter (default: `accordserver=debug,tower_http=debug`)

### LiveKit Configuration
- `LIVEKIT_INTERNAL_URL` — LiveKit server URL for internal API requests (e.g. `http://livekit:7880`). Defaults to `LIVEKIT_URL`.
- `LIVEKIT_EXTERNAL_URL` — LiveKit server URL for client WebRTC connections (e.g. `wss://livekit.example.com`). Defaults to internal URL.
- `LIVEKIT_API_KEY` — LiveKit API key
- `LIVEKIT_API_SECRET` — LiveKit API secret

## Architecture

Single-binary Axum application with SQLite (via sqlx) for persistence, and LiveKit for WebRTC voice integration.

The crate is structured as a library (`src/lib.rs`) with a thin binary entry point (`src/main.rs`).

### Core Modules

- **`src/main.rs`** — Initializes tracing, config, database connection, and starts the server.
- **`src/config.rs`** — `Config` struct loaded from environment variables. Contains `LiveKitConfig`.
- **`src/state.rs`** — `AppState` shared across all handlers. Holds: `db` (SqlitePool), `voice_states` (DashMap), `dispatcher` (gateway event broadcaster), `gateway_tx` (broadcast sender), `livekit_client` (LiveKitClient), `rate_limits` (DashMap of per-user token buckets).
- **`src/error.rs`** — `AppError` enum implementing `IntoResponse`.
- **`src/snowflake.rs`** — Snowflake-style unique ID generator used for all entity IDs.

### Gateway (WebSocket) — `src/gateway/`

The gateway is the real-time event system. Clients connect via `GET /ws`.

- **`mod.rs`** — WebSocket upgrade handler and the main session loop. Flow: send HELLO → wait for IDENTIFY (with token + intents) → send READY → enter event loop handling heartbeats, broadcasts, voice state updates, and voice signals.
- **`events.rs`** — Message envelope (`GatewayMessage`), opcodes (0-10: EVENT, HEARTBEAT, IDENTIFY, RESUME, HEARTBEAT_ACK, HELLO, RECONNECT, INVALID_SESSION, PRESENCE_UPDATE, VOICE_STATE_UPDATE, REQUEST_MEMBERS), and close codes (4000-4014).
- **`dispatcher.rs`** — Manages broadcast channel. Sessions register/deregister. Events are sent to all sessions then filtered by space membership and intents.
- **`session.rs`** — Per-connection state: user_id, intents, space_ids, sequence counter, send channel.
- **`heartbeat.rs`** — Heartbeat interval/timeout constants.
- **`intents.rs`** — Maps event types to intent categories for filtering.

Authentication on the gateway uses `"Bot <token>"` or `"Bearer <token>"` in the IDENTIFY payload, resolved against `bot_tokens`/`user_tokens` tables via token hashing.

### REST API — `src/routes/`

All REST endpoints are under `/api/v1` with a rate-limit middleware layer. The router is built in `routes/mod.rs`. Key resource groups:

- **Auth** — `POST /auth/register` (unauthenticated), `POST /auth/login` (unauthenticated), `POST /auth/logout` (authenticated). Passwords hashed with Argon2id. Returns user + bearer token on register/login.
- **Users** — `GET/PATCH /users/@me`, `GET /users/{user_id}`, `GET /users/@me/spaces`
- **Spaces** (guilds) — CRUD + channel listing/creation/reordering. Spaces have a `public` flag; public spaces allow joining without an invite via `POST /spaces/{space_id}/join`.
- **Channels** — CRUD (nested under spaces for creation, top-level for get/update/delete)
- **Messages** — CRUD, bulk delete, pins, typing indicators
- **Members** — List, search, get, update, kick, role assignment
- **Roles** — CRUD, reordering
- **Bans** — List, get, create, delete
- **Invites** — CRUD, accept; supports both channel-level and space-level invites (channel_id is optional)
- **Public Spaces** — `POST /spaces/{space_id}/join` lets users join public spaces without an invite
- **Reactions** — Add/remove per-user, list by emoji, bulk remove
- **Emojis** — CRUD with role restrictions
- **Voice** — Join/leave channels, voice regions, voice status, voice info (`GET /voice/info` returns `{ "backend": "livekit" }`)
- **Applications** — Bot app CRUD, token reset
- **Interactions** — Slash command stubs
- **Gateway info** — `GET /api/v1/gateway` (public), `GET /api/v1/gateway/bot` (authenticated)

### Database Layer — `src/db/`

One module per resource (auth, users, spaces, channels, messages, members, roles, bans, invites, emojis). Each contains query functions using sqlx.

- **`mod.rs`** — Creates SQLite pool with WAL mode and runs migrations via `sqlx::migrate!()`.

### Models — `src/models/`

Serializable structs for each entity (user, space, channel, message, member, role, permission, invite, emoji, embed, attachment, voice, presence, application, interaction). These are the data types shared between db, routes, and gateway.

### Middleware — `src/middleware/`

- **`auth.rs`** — Token hashing (`create_token_hash`) using SHA-256 and authentication resolution. Resolves `Bearer` (user) and `Bot` tokens against `user_tokens`/`bot_tokens` tables. User passwords are hashed with Argon2id (via the `argon2` crate) and stored in the `password_hash` column on the `users` table.
- **`permissions.rs`** — Central authorization module. Key functions: `resolve_member_permissions()` (computes effective permissions from @everyone + assigned roles; owner gets implicit `administrator`), `require_permission()`, `require_membership()`, `require_channel_permission()`, `require_channel_membership()`. Defines `DEFAULT_EVERYONE_PERMISSIONS` constant used when creating new spaces.
- **`rate_limit.rs`** — Token-bucket rate limiter applied to all `/api/v1/*` routes. 60 requests/minute + 10 burst per user (keyed by SHA-256 of Authorization header). Returns 429 with `Retry-After` header and `X-RateLimit-Limit`, `X-RateLimit-Remaining`, `X-RateLimit-Reset` headers on every response.

### Voice — `src/voice/`

- **`state.rs`** — In-memory voice state management (join/leave tracking) via the `voice_states` DashMap.
- **`livekit.rs`** — `LiveKitClient` wrapping the `livekit-api` crate. Handles room creation, JWT token generation, participant removal, and room cleanup.

Voice flow: client sends VOICE_STATE_UPDATE (opcode 9) → server updates voice state → broadcasts `voice.state_update` to space members → sends `voice.server_update` back to client containing a LiveKit URL and JWT token. The client connects directly to LiveKit for WebRTC.

## Database

SQLite with WAL journal mode. Migrations in `migrations/` run automatically at startup. ~20 tables including: users, spaces, channels, messages, members, member_roles, roles, permission_overwrites, bans, invites, emojis, emoji_roles, reactions, attachments, pinned_messages, applications, bot_tokens, user_tokens, dm_participants. IDs are TEXT (snowflake-generated). Timestamps stored as TEXT via SQLite's `datetime()`. The `users` table has a nullable `password_hash` column (Argon2id) for registered users; bot users and legacy seeded users have `NULL`.

New migrations should be added as numbered SQL files (e.g., `migrations/005_add_something.sql`).

## Tests

Integration tests live in `tests/` with a shared helper in `tests/common/mod.rs`:
- `tests/http.rs` — REST API tests
- `tests/ws.rs` — WebSocket/gateway tests
- `tests/e2e.rs` — End-to-end flows (includes authorization and rate limiting tests)
- `tests/security.rs` — Security tests (authorization enforcement, input validation, authentication edge cases)

## Agent-Specific Notes

This repository includes a compiled documentation database/knowledgebase at `AGENTS.db`.
For context for any task, you MUST use MCP `agents_search` to look up context including architectural, API, and historical changes.
Treat `AGENTS.db` layers as immutable; avoid in-place mutation utilities unless required by the design.
