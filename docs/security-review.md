# Server security review — 2026-09-07

This review covered the Rust server's authentication, authorization, REST and WebSocket delivery, uploads, outbound HTTP, configuration, and locked dependencies. It used source inspection, regression tests, the existing integration suite, and RustSec advisory scanning. It did not test a deployed instance or the desktop client.

## Fixed findings

- **Internal network requests through link previews:** user-supplied URLs previously reached private addresses, followed unchecked redirects, and buffered unlimited HTML. Previews now validate schemes, credentials, IP literals and redirects; filter DNS results at connection time; disable proxy bypasses; and stop after 1 MiB. Federation uses the same address classification, handles bracketed IPv6 literals, disables proxy bypasses, and bounds responses to 2 MiB. The preview DNS checks stay enabled even when federation's local testing override is set. Reqwest's [client configuration](https://docs.rs/reqwest/latest/reqwest/struct.ClientBuilder.html) documents the resolver, proxy and redirect hooks.
- **Private channel disclosure:** public-space membership/listing previously published all channel messages through anonymous REST reads, search, crawler snapshots and oEmbed. WebSocket delivery checked space membership without channel visibility. Anonymous access now requires explicit `allow_anonymous_read`; authenticated reads require channel access and history permission. Channel lists, READY, voice state and channel broadcasts are filtered. Buffered message replay is rechecked after permission changes.
- **Role hierarchy bypass:** a role manager could lower a higher role, raise their own role, or move `@everyone`. Reordering validates the full batch against current hierarchy and persists it transactionally. Foreign-space roles and invalid positions are rejected. Bulk member role assignment validates space ownership.
- **Channel permission escalation:** channel overwrites could grant arbitrary permissions, including `administrator`. Grants and denials must be within the actor's permissions; replacing/deleting old overwrites also checks permissions. `administrator` cannot be supplied or acquired through channel overwrites. Channel operations require visibility.
- **Guest scope and revocation:** space/channel listing enforces token scope, and disabling guest access invalidates guest authentication for REST and new WebSocket connections. Existing channel streams also recheck guest access.
- **Rate-limit bypass:** arbitrary authorization and forwarded IP headers previously supplied new buckets. Verified users now share a bucket across tokens; anonymous, invalid-token and guest requests use the connection IP. Stale buckets are bounded. Registration and guest issuance use the same IP policy.
- **Uploaded active content:** CDN responses now carry `nosniff` and a restrictive sandbox policy; attachments are served as downloads.
- **Resource exhaustion and parser failures:** compressed plugin manifests/icons have expansion limits; incoming WebSocket frames/messages are limited to 256 KiB; pagination has positive bounds; and HTML tag parsing uses byte-preserving ASCII case conversion to avoid Unicode offset panics.
- **Credential disclosure:** the startup banner redacts database passwords and query parameters.

## Dependency audit

Updated `h2`, `rustls-webpki`, `anyhow`, `event-listener`, both locked `rand` versions, `serial_test`, and `spin`. This removes five vulnerability findings, all reported unsoundness warnings, and the yanked-package warning.

`cargo audit` still reports [RUSTSEC-2023-0071](https://rustsec.org/advisories/RUSTSEC-2023-0071.html) for `rsa`, retained in Cargo.lock by SQLx's optional MySQL dependency. No upstream patch is available. `cargo tree --locked --target all -i rsa` reports no active dependency path: the server enables SQLite and PostgreSQL. No advisory was suppressed. Reassess this finding if MySQL or another RSA consumer is enabled.

## Deployment behavior changes

Anonymous channel history must now be explicitly published with `allow_anonymous_read` in a public space. Guests still need their scoped space to allow guest access. Existing role/channel overwrite data is retained; administrator grants in channel overwrites no longer take effect.

`TRUST_PROXY_HEADERS` defaults to false. Enable it only behind a trusted proxy that replaces incoming IP headers and prevents direct access to the server; otherwise forwarded headers remain untrusted. Without this option, a proxy's connections share its IP bucket.

Link previews and federation connect directly rather than through environment-configured HTTP proxies. Preview HTML is limited to 1 MiB, federation response bodies to 2 MiB, plugin manifests to 256 KiB, and plugin icons to 2 MiB.

## Validation

Regression coverage is in `tests/security_sweep.rs`, `tests/ws.rs`, and unit tests in `src/unfurl.rs` and `src/routes/plugins.rs`. It exercises both rejected attacks and authorized behavior, including private-message delivery, guest revocation, role reordering, overwrite removal, forged headers, HTML downloads, redirect SSRF, streamed oversized HTML, Unicode parsing and compressed ZIP expansion.

- `cargo test --locked --all-targets`: 405 passed, no failures.
- `cargo test --locked --features test-seed --test security test_seed_endpoint_works_with_feature`: passed; the default suite also verifies the endpoint is absent without the feature.
- `cargo fmt --check` and `git diff --check`: passed.
- Release preparation boxes federation verification error responses without changing their HTTP status or body, addressing the two `clippy::result_large_err` warnings identified during the initial sweep. `cargo clippy --locked --all-targets -- -D warnings` passes without lint exceptions.
- `cargo audit --json`: one remaining inactive RSA advisory described above; no other warnings.

PostgreSQL integration tests require a PostgreSQL instance and were not run in this environment.

## Follow-up sweep — 2026-09-08

The follow-up addresses #59, #62–#67, #69 and #71–#75 with explicit local admin provisioning, atomic invite acceptance, HTML escaping and invite-page CSP, trusted proxy allowlists, bounded authentication/preview/socket work, live gateway credential revalidation, actual native-plugin signature verification, plugin channel/participant boundaries, and durable attachment deletion. Seed output now shares database URL redaction. SQLite channel and space deletion explicitly removes dependent messages within the same transaction.

For #68, active voice access is reconciled after API mutations and periodically. A durable eviction queue calls LiveKit with a timeout and retries failures, including after a restart. New join tokens expire after 60 seconds. The self-hosted LiveKit cached-token reconnection limitation remains: removal does not revoke an already issued JWT. Keep #68 open for deployment-level validation and stronger reconnection control; the active-participant and retry fixes are covered by a mocked Twirp test.

Operational changes, signing format, credential rotation, administrator provisioning, proxy configuration and old attachment cleanup are documented in [security operations](security-operations.md). Existing native bundles must be reinstalled with a trusted detached signature. No new release tag is part of this follow-up.

Regression coverage adds `tests/security_followup.rs`, gateway revocation and admission/flood tests, trusted proxy tests, and Ed25519 positive/tampering/unknown-signer tests. The attachment tests cover metadata gating before unlink, channel/space/account/message deletion, encoded-path bypasses, and old orphan cleanup. The previous heartbeat flood test now expects an explicit rate-limit close; normal heartbeat tests remain.

Follow-up local validation: `cargo test --locked --all-targets` passed all 420 tests; `cargo clippy --locked --all-targets -- -D warnings`, `cargo fmt --check` and `git diff --check` passed. PostgreSQL and Docker Compose are unavailable locally; PostgreSQL is covered by the repository's push CI.
