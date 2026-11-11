# Changelog

## 0.2.5

Private chats gain per-device encryption keys (encryption v2). Each device has
its own keys, certified by a versioned account key; the existing identity becomes
account version 1, so pins, fingerprints and history are unchanged. A new device
registers as pending and is approved from an existing one after both compare a
16-digit code; the approval delivers the account keyring sealed to the new
device. Removing a device rotates the account key with forward and back
signatures, re-certifies the remaining devices, deletes the removed device's
sessions and closes its gateway connections. New endpoints live under
`/users/@me/encryption`; the `encryption.device_pending` and
`encryption.devices_update` gateway events use the `direct_messages` intent.
v2 envelopes are signed by the sender device and carry its account version;
v1 envelopes remain admissible, so older clients keep working.

Member presence snapshots are now complete and privacy-filtered, and presence
federates across servers.

Adds SQLite migration 040 and PostgreSQL migration 025. Requires Daccord client
0.2.28 for device approval and removal; older clients continue on v1. Federated
peers should upgrade before users remove devices: older servers keep cached
identities immutable and refuse an account after its key rotates. See
`docs/private-chat-encryption.md`.

## 0.2.4

Arcade now has one ordinary channel per space, with persisted ordering, category
placement, renaming and channel view permissions. Enabling the first game
creates the entry; a database constraint prevents duplicate Arcade channels.
Existing enabled spaces receive their entry during the upgrade.

Arcade summaries include the number of visible active games and lobbies, with
invite-only sessions counted only for authorized members. Games expire after
seven days without player activity; session snapshots expose the removal
deadline for the client countdown. Reads, spectators and automatic Pong ticks
do not reset it. Cleanup removes expired sessions with revision checks while
preserving installed games and other completed-game history.

Requires the updated Arcade client for channel rendering, badges and countdowns.
Signing, trust and directory settings are unchanged. Master server 0.1.2 remains
compatible and needs no update.

Release highlights and upgrade notes for Accord Server. Earlier releases are
documented in [GitHub Releases](https://github.com/DaccordProject/accordserver/releases).

## [0.2.3] - 2026-10-02

### Added

- Space Arcade with reviewed, signed Chess and Pong releases, open or invite-only
  lobbies, spectators, persisted turns, and authoritative multiplayer sessions.
- Signed end-to-end encrypted private messages and attachments, including
  federated private conversations.

### Fixed

- Assigned distinct Arcade and private-chat encryption migration versions for
  SQLite and PostgreSQL so new and upgraded servers start successfully.
- Documented reachable LiveKit media addresses for Docker deployments.

### Upgrade notes

- Upgrade the master directory and Daccord clients alongside community servers.
  Legacy executable plugin routes are retired; existing plugin tables remain
  available for export.
- Arcade is disabled until operators configure `EXPERIENCES_ENABLED=true` and
  `EXPERIENCE_TRUSTED_KEYS`. Space owners then enable reviewed games and Arcade.
- Updated clients must send signed encrypted payloads for private messages;
  plaintext private-message writes are rejected.

## [0.2.2] - 2026-09-28

### Fixed

- Direct messages now use canonical user identities across local and federated
  accounts, avoiding duplicate conversations when the same users reconnect or
  message each other through different servers.

### Documentation

- Documented how administrators can rotate a bootstrap account password and
  recover access when the password is lost.

## [0.2.1] - 2026-09-23

### Added

- Space summaries and space responses now include `member_count` and
  `presence_count`, so clients can show accurate roster totals (for example the
  Offline section) without fetching every member. The totals are also carried in
  create/update responses and `space.update` events, and exclude system and
  invisible users where appropriate.

## [0.2.0] - 2026-09-12

Changes since 0.1.35.

### Added

- Optional local AutoMod for image and sampled-video attachments, with per-space
  policies, quarantine, moderator review, audit events, and retention controls.
- SHA-256 attachment hash blocking across message uploads, avatars, banners,
  space icons, emoji, and soundboard clips.
- Configurable per-user upload request and byte budgets shared across upload routes.
- Atomic channel slowmode shared by text messages, thread replies, and multipart
  messages, with cooldowns that survive restarts and message deletion.

### Fixed

- Restored attachment content types, byte-range requests, and cache revalidation
  for inline audio/video playback and seeking. CDN requests revalidate attachment
  availability so moderation can withdraw access.

### Changed

- Updated client references to the Flutter/Dart Daccord client.

### Upgrade notes

- Model scanning remains disabled until configured. Local scanning requires model
  weights and ONNX Runtime 1.22; video sampling also requires FFmpeg and FFprobe.
  The Linux amd64 Docker image includes the runtime and video tools, but model
  weights must be installed separately. See the [AutoMod guide](docs/automod.md).
- Explicit hash blocks and upload budgets apply independently of model scanning.
  Default upload budgets are 6 requests and 50 MiB per minute per user; adjust
  `upload_requests_per_minute` and `upload_bytes_per_minute` in admin settings.
- Clients must handle HTTP 202 for uploads awaiting moderation and HTTP 429 with
  `Retry-After` for slowmode or upload limits.

[0.2.2]: https://github.com/DaccordProject/accordserver/compare/v0.2.1...v0.2.2
[0.2.1]: https://github.com/DaccordProject/accordserver/compare/v0.2.0...v0.2.1
[0.2.0]: https://github.com/DaccordProject/accordserver/compare/v0.1.35...v0.2.0

[0.2.3]: https://github.com/DaccordProject/accordserver/compare/v0.2.2...v0.2.3
