# Curated games and the Space Arcade

The community server controls space policy and authoritative live sessions.
The master server owns review, signatures, immutable versions and revocation.
The initial approved authorities are chess and Pong; new rules engines require
an audited server implementation. Presentation/input modules use the bounded
WASM host API 1 documented in the
[creator SDK](https://github.com/DaccordProject/daccord/blob/feat/wasm-experiences/docs/experiences/architecture.md).

Operators set `EXPERIENCES_ENABLED=true`, `EXPERIENCE_DIRECTORY_URL` (default
`https://master.daccord.gg`), and `EXPERIENCE_TRUSTED_KEYS` (key id → hex 32-byte
Ed25519 public key). Directory URLs require HTTPS, with HTTP allowed only for
loopback development. Redirects, custom installation URLs, uploaded binaries,
Lua/native archives, unknown capabilities and unrestricted WASI are denied.
Default policy disables executable experiences until the operator provisions
trust. Review/publication credentials never belong on community servers.

## Owner lifecycle

`GET /spaces/{space}/experiences/directory` is owner/admin discovery.
`PUT /spaces/{space}/experiences/{id}` with `{version}` pins an exact reviewed
release. Explicit owner operations enable new versions, updates or rollbacks;
every update ends existing sessions and increments the installation generation.
`PATCH` with `{enabled, turn_timeout_seconds}` configures or disables a game.
`DELETE` removes it and stops active sessions. All controls require `manage_space`
and a member account; read-only guests and bots cannot operate the Arcade.

`GET/PATCH /spaces/{space}/arcade` reads/sets the single Arcade switch. The client
places this entry outside the channel list and hides it until an enabled game
exists. Turning it off stops sessions. Historical results remain readable to
previously authorized members. Package delivery is authenticated through
`GET /spaces/{space}/experiences/{id}/package`; guests receive no network grant.

Every installation/package/start/turn operation fetches current directory
approval and verifies the exact signed payload, digest, manifest and pinned
version against operator-provisioned keys. Outages, withdrawal and revocation
fail closed and end affected active sessions. No cached package can silently
execute during an outage. Re-enablement requires an explicit owner operation
and fresh approval. Revocation is permanent at the directory; key rotation
requires provisioning new public keys and reviewing new immutable versions.

## Shared session API

All paths are rooted at `/spaces/{space}/arcade/sessions`.

| Operation | Request |
|---|---|
| Discover | GET collection; invite-only sessions filtered |
| Create lobby | POST `{game_id, invite_only, invited:[member_ids]}` |
| Snapshot/resume | GET `/{id}` |
| Join/leave/ready/start | POST `/{id}/members` `{operation,revision,spectator,ready}` |
| Move/resign | POST `/{id}/actions` `{revision,kind,a,b,promotion?}` |
| Real-time transport | WS `/{id}/live`, then `{token:"Bearer …"}` from the trusted SDK |

Every read and mutation requires current space membership and invitation access.
Joining a started game preserves the player slots; late members can spectate.
Only players submit actions. Both players must be ready, and only the lobby host
starts. Lobby host departure transfers ownership to the next player, or a
spectator when no players remain; an empty lobby ends. Leaving a started game
records a forfeit. Spectator departure does not affect play. Invite-only creators
retain invitation access to their result after leaving.

Compare-and-swap revisions prevent concurrent moves or stale ready/host changes
from overwriting state. Installation-generation and Arcade policy checks are
part of the SQL write condition, so disable/update/remove also reject requests
already in flight. A revision conflict requires fetching a new snapshot;
mutations are never blindly replayed after a rate-limit response.

`experience.session` gateway events carry participant-scoped snapshots under the
`experiences` intent. The gateway rechecks membership; spectators cannot submit
moves. The client shows in-app turn notifications and Arcade waiting-on-you
badges. Push delivery remains subject to the existing push service/platform
availability (#81). Reconnect retrieves persisted state and pinned package
identity rather than trusting local state or the former host.

## Authorities and limits

Chess uses cozy-chess's MIT-licensed legal move engine, including check, castling, en passant,
promotion, checkmate and stalemate. The authenticated slot/turn is checked before
rule validation. The server records win/draw/cancellation outcomes, repetition,
50-move and insufficient-material draws, resignations and optional turn-timeout
forfeits. Turn-based state survives all players going offline; presence never
advances a turn. Timeout checks precede reads/moves. Abandoned lobbies expire
after a day; inactive games/results have a 30-day retention window.

Pong is server-authoritative: the host owns fixed 16ms simulation steps, paddles,
ball, collisions and the first-to-five result. WebSocket snapshots run at 20Hz,
with monotonic input sequences and at most 30 messages/second per
user/space/session across connections. Messages/frames are at most 2048 bytes.
Each connection retains at most one pending paddle target. A compare-and-swap
conflict retries that target on the next snapshot tick; a newer input replaces
it, so a final drag position converges without another input event.
The trusted host coalesces inputs to 20Hz; guests receive the same `read_state`,
`draw`, `action` API as chess and no socket. Target end-to-end input latency is
250ms: up to 50ms host coalescing, 16ms simulation, 50ms snapshot and ordinary
network round-trip. Functional convergence tests are separate from measured
mobile/browser latency, which needs device evidence.

Loss of a player connection pauses after 5s; reconnect restores the same slot.
After 60s the remaining live player receives a disconnect forfeit. With no live
connections the persisted state pauses until reconnect/retention cleanup. Missed
ticks are capped to 100ms and never fast-forward an offline game. Credential,
membership, pinned release and fresh approval are revalidated every 5s while a
live socket is open; policy-generation SQL guards reject disable/update at once.

Sessions are limited to 32 active games per space, 2 players and 16 spectators.
Chess history is bounded to 2048 plies. Package/module/instruction/drawing limits
are enforced by the shared contract and portable client host. The fixed capability
set means updates cannot silently gain ambient filesystem/network/storage access.

## Cutover and verification

Legacy SDK models, source/bundle upload/download routes, channel activity
sessions, arbitrary action forwarding and client-submitted leaderboards are
removed together. Their database tables remain inert for operator export; users
must explicitly enable a reviewed replacement. The master has no legacy listings
to migrate. Existing deployments must upgrade both client and community server;
old servers have no Arcade capability and display an unavailable state.

Run `cargo test --test experiences` for signed directory enablement, lobbies,
chess authorization, persisted resume, invite filtering, policy races, revocation,
retired routes and two WebSocket Pong players. Run the shared contract tests with
`cargo test --manifest-path crates/experience_contract/Cargo.toml`. CI additionally
runs the complete SQLite and PostgreSQL suites. Client physical-device validation
is recorded separately; server tests do not establish a mobile platform claim.

Live simulation revisions advance independently of user intent. Running Pong
spectator joins, departures and resignations may carry an older nonnegative
snapshot revision; future revisions are rejected. The server reapplies only
these lifecycle operations to its freshly approved current state in a short
database write transaction. Approval requests finish before acquiring the write
lock; the current snapshot, participants and revision are checked again under
the lock, so repeated ticks cannot starve a departure or resignation. The same
SQL revision and installation-policy guards apply. Chess moves and all lobby
ready/start operations still require the exact observed revision.
