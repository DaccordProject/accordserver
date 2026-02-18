# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What This Is

Accord is a chat/voice server backend for a Godot game client. It's a Rust web server built with Axum that provides a Discord-like API with WebSocket gateway, REST endpoints, and SFU-based voice routing. Think of it as a self-hosted Discord backend.

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
- `DATABASE_URL` — SQLite connection string (default: `sqlite:accord.db?mode=rwc`; unused in SFU mode)
- `RUST_LOG` — tracing filter (default: `accordserver=debug,tower_http=debug`)
- `ACCORD_MODE` — `"main"` (default) or `"sfu"` — selects which mode to run

### Voice Backend
- `ACCORD_VOICE_BACKEND` — `"custom"` (default) or `"livekit"` — selects the voice backend

### LiveKit (required when `ACCORD_VOICE_BACKEND=livekit`)
- `LIVEKIT_URL` — LiveKit server URL (e.g. `wss://livekit.example.com`)
- `LIVEKIT_API_KEY` — LiveKit API key
- `LIVEKIT_API_SECRET` — LiveKit API secret

### SFU Mode Only
These are required when `ACCORD_MODE=sfu`:
- `ACCORD_MAIN_URL` — base URL of the main server (e.g. `http://main-server:3000`)
- `ACCORD_SFU_NODE_ID` — unique ID for this SFU node
- `ACCORD_SFU_REGION` — region label (e.g. `"us-east"`)
- `ACCORD_SFU_CAPACITY` — max concurrent sessions (integer)
- `ACCORD_SFU_ENDPOINT` — publicly reachable address for clients (e.g. `ws://sfu-1:4000`)
- `ACCORD_SFU_HEARTBEAT_INTERVAL` — seconds between heartbeats (default: `25`)

## Architecture

Single-binary Axum application with SQLite (via sqlx) for persistence. The binary runs in two modes:

- **Main mode** (default): Full server — REST API, gateway, database, SFU node management.
- **SFU mode**: Lightweight forwarding node that auto-registers with the main server, heartbeats, and deregisters on shutdown. Exposes a `/health` endpoint.

The crate is structured as a library (`src/lib.rs`) with a thin binary entry point (`src/main.rs`).

### Core Modules

- **`src/main.rs`** — Initializes tracing and config, then branches on `AccordMode`: `run_main_server()` (DB, gateway, SFU reaper, full router) or `run_sfu_node()` (lightweight SFU runtime).
- **`src/config.rs`** — `Config` struct loaded from environment variables. Contains `AccordMode` enum (`Main`/`Sfu`), `VoiceBackend` enum (`Custom`/`LiveKit`), optional `SfuConfig` (parsed when mode is SFU), and optional `LiveKitConfig` (parsed when voice backend is LiveKit).
- **`src/state.rs`** — `AppState` shared across all handlers. Holds: `db` (SqlitePool), `sfu_nodes` (DashMap), `voice_states` (DashMap), `dispatcher` (gateway event broadcaster), `gateway_tx` (broadcast sender), `voice_backend` (Custom or LiveKit), `livekit_client` (optional LiveKitClient), `rate_limits` (DashMap of per-user token buckets).
- **`src/error.rs`** — `AppError` enum implementing `IntoResponse`.
- **`src/snowflake.rs`** — Snowflake-style unique ID generator used for all entity IDs.

### Gateway (WebSocket) — `src/gateway/`

The gateway is the real-time event system. Clients connect via `GET /ws`.

- **`mod.rs`** — WebSocket upgrade handler and the main session loop. Flow: send HELLO → wait for IDENTIFY (with token + intents) → send READY → enter event loop handling heartbeats, broadcasts, voice state updates, and voice signals.
- **`events.rs`** — Message envelope (`GatewayMessage`), opcodes (0-11: EVENT, HEARTBEAT, IDENTIFY, RESUME, HEARTBEAT_ACK, HELLO, RECONNECT, INVALID_SESSION, PRESENCE_UPDATE, VOICE_STATE_UPDATE, REQUEST_MEMBERS, VOICE_SIGNAL), and close codes (4000-4014).
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
- **Voice** — Join/leave channels, voice regions, voice status, voice info (`GET /voice/info` returns `{ "backend": "custom"|"livekit" }`)
- **SFU** — Node registration, heartbeat, deregistration (internal/admin)
- **Applications** — Bot app CRUD, token reset
- **Interactions** — Slash command stubs
- **Gateway info** — `GET /api/v1/gateway` (public), `GET /api/v1/gateway/bot` (authenticated)

### Database Layer — `src/db/`

One module per resource (auth, users, spaces, channels, messages, members, roles, bans, invites, emojis, sfu). Each contains query functions using sqlx.

- **`mod.rs`** — Creates SQLite pool with WAL mode and runs migrations via `sqlx::migrate!()`.

### Models — `src/models/`

Serializable structs for each entity (user, space, channel, message, member, role, permission, invite, emoji, embed, attachment, voice, presence, application, interaction). These are the data types shared between db, routes, and gateway.

### Middleware — `src/middleware/`

- **`auth.rs`** — Token hashing (`create_token_hash`) using SHA-256 and authentication resolution. Resolves `Bearer` (user) and `Bot` tokens against `user_tokens`/`bot_tokens` tables. User passwords are hashed with Argon2id (via the `argon2` crate) and stored in the `password_hash` column on the `users` table.
- **`permissions.rs`** — Central authorization module. Key functions: `resolve_member_permissions()` (computes effective permissions from @everyone + assigned roles; owner gets implicit `administrator`), `require_permission()`, `require_membership()`, `require_channel_permission()`, `require_channel_membership()`. Defines `DEFAULT_EVERYONE_PERMISSIONS` constant used when creating new spaces.
- **`rate_limit.rs`** — Token-bucket rate limiter applied to all `/api/v1/*` routes. 60 requests/minute + 10 burst per user (keyed by SHA-256 of Authorization header). Returns 429 with `Retry-After` header and `X-RateLimit-Limit`, `X-RateLimit-Remaining`, `X-RateLimit-Reset` headers on every response.

### Voice — `src/voice/`

- **`state.rs`** — In-memory voice state management (join/leave tracking) via the `voice_states` DashMap.
- **`sfu.rs`** — SFU node allocation logic (picks a node for a voice session). Used when `voice_backend == Custom`.
- **`signaling.rs`** — WebRTC signaling relay between clients via the gateway. Only active for the custom SFU backend.
- **`livekit.rs`** — `LiveKitClient` wrapping the `livekit-api` crate. Handles room creation, JWT token generation, participant removal, and room cleanup. Used when `voice_backend == LiveKit`.

Voice flow (Custom): client sends VOICE_STATE_UPDATE (opcode 9) → server updates voice state → broadcasts `voice.state_update` to space members → sends `voice.server_update` back to client with allocated SFU endpoint → client connects to SFU for WebRTC.

Voice flow (LiveKit): same initial steps, but `voice.server_update` contains a LiveKit URL and JWT token instead of an SFU endpoint. The client connects directly to LiveKit for WebRTC. Voice signaling (opcode 11) is skipped since LiveKit handles it internally.

### SFU Client & Runtime

- **`src/sfu_client.rs`** — HTTP client (reqwest) that calls the main server's SFU management REST endpoints: `register()`, `heartbeat(current_load)`, `deregister()`. Used by the SFU runtime and integration tests.
- **`src/sfu_runtime.rs`** — Complete SFU node lifecycle when running in SFU mode:
  1. Registers with main server (exponential backoff retry, so the SFU node can start before the main server is ready).
  2. Starts a minimal Axum server with a `/health` endpoint returning node_id, region, current_load.
  3. Spawns a heartbeat task on a configurable interval (default 25s, well under the 60s reaper timeout).
  4. On SIGINT/SIGTERM: aborts heartbeat, deregisters with main server, exits.
  Uses its own `SfuState` (not `AppState`) — no database, gateway, or dispatcher.

### Background Tasks

- **Stale SFU node reaper** — Spawned in `main.rs` only when `voice_backend == Custom`. Runs every 30s, marks nodes offline if no heartbeat for 60s, removes them from the in-memory DashMap.

## Database

SQLite with WAL journal mode. Migrations in `migrations/` run automatically at startup. ~20 tables including: users, spaces, channels, messages, members, member_roles, roles, permission_overwrites, bans, invites, emojis, emoji_roles, reactions, attachments, pinned_messages, sfu_nodes, applications, bot_tokens, user_tokens, dm_participants. IDs are TEXT (snowflake-generated). Timestamps stored as TEXT via SQLite's `datetime()`. The `users` table has a nullable `password_hash` column (Argon2id) for registered users; bot users and legacy seeded users have `NULL`.

New migrations should be added as numbered SQL files (e.g., `migrations/005_add_something.sql`).

## Tests

Integration tests live in `tests/` with a shared helper in `tests/common/mod.rs`:
- `tests/http.rs` — REST API tests
- `tests/ws.rs` — WebSocket/gateway tests
- `tests/e2e.rs` — End-to-end flows (includes authorization and rate limiting tests)
- `tests/security.rs` — Security tests (authorization enforcement, input validation, authentication edge cases)
- `tests/sfu.rs` — SFU client lifecycle tests (register → heartbeat → deregister round-trip)

## Agent-Specific Notes

This repository includes a compiled documentation database/knowledgebase at `AGENTS.db`.
For context for any task, you MUST use MCP `agents_search` to look up context including architectural, API, and historical changes.
Treat `AGENTS.db` layers as immutable; avoid in-place mutation utilities unless required by the design.
