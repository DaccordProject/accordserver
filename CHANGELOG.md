# Changelog

Release highlights and upgrade notes for Accord Server. Earlier releases are
documented in [GitHub Releases](https://github.com/DaccordProject/accordserver/releases).

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

[0.2.0]: https://github.com/DaccordProject/accordserver/compare/v0.1.35...v0.2.0
