# Security operations

## Initial administrator

Public registration never grants instance administration, including on empty databases. Existing administrators retain access. The server prints a warning when there is no administrator.

Create the operator account locally before exposing a new server. Choose a new username; provisioning refuses to overwrite or promote an existing account. Passwords must be 16–128 characters.

```bash
read -r -s -p 'Admin password: ' ACCORD_BOOTSTRAP_PASSWORD
export ACCORD_BOOTSTRAP_PASSWORD
./accordserver --bootstrap-admin operator
unset ACCORD_BOOTSTRAP_PASSWORD
```

Use the same `DATABASE_URL` or `--data-dir` as the server. This command creates the account and exits without starting a listener. For Compose, use `docker compose run --rm -e ACCORD_BOOTSTRAP_PASSWORD accordserver ./accordserver --bootstrap-admin operator` with the variable exported locally, then unset it. Desktop operators can run the bundled server binary with their desktop data directory.

## Deployment credentials and proxy addresses

Both Compose configurations require `LIVEKIT_API_KEY` and `LIVEKIT_API_SECRET`. Generate independent values with `openssl rand -hex 32` and store them in an untracked `.env` with mode 600. PostgreSQL additionally requires `POSTGRES_PASSWORD` and a matching `DATABASE_URL`; URL-encode its password. LiveKit's signaling endpoint is exposed through Caddy, with only media TCP/UDP ports published directly.

For an existing deployment using `devkey` / `secret`, generate new credentials, update both Accord and LiveKit, and restart both services. This disconnects current calls and invalidates tokens signed with the old key. Updating a PostgreSQL environment variable does not change an existing role's password: rotate it in PostgreSQL, then update the application's connection URL and restart. Preserve the database volume.

Forwarded IPs require both `TRUST_PROXY_HEADERS=true` and `TRUSTED_PROXY_IPS`, a comma-separated list of the actual reverse proxies' literal IP addresses. All other peers use their socket IP. Configure the proxy to replace incoming forwarding headers. Without an allowlist, clients behind a proxy share one IP budget.

## Native plugin signing

Native uploads require two additional multipart fields: `signer` and `signature`. `signer` contains letters, digits, `_` or `-`. `signature` is a base64 Ed25519 signature. Configure `ACCORD_PLUGIN_TRUSTED_KEYS` as a JSON object mapping signer IDs to base64 raw 32-byte Ed25519 public keys. An empty trust store rejects all native uploads.

Sign these bytes with the trusted private key:

```
ASCII("accord-native-plugin-v1") || 0x00 || SHA256(exact_uploaded_zip_bytes)
```

The archive digest binds every file, including `plugin.json`. Keep the detached signature outside the ZIP to avoid a self-referential hash. Merely including `plugin.sig` inside an archive is insufficient. The server computes `bundle_hash`, verifies the signature, and derives `signed` and the returned signature metadata (`ed25519-v1:<signer>:<base64-signature>`). Scripted uploads cannot assert trust through manifest fields.

The migration clears older unverified trust flags. Reinstall native bundles with a valid trusted signature before downloading them again. Downloads reverify against the current trust store, so removing a key also blocks future downloads of bundles signed with that key.

## Revocation, limits and deleted files

Gateway credentials are revalidated before inbound operations and broadcast delivery, and every two seconds when idle or parked. Revoked, expired, deleted or disabled identities lose their sessions and replay eligibility. Gateway admission allows at most 512 connections globally, 32 per IP and eight per authenticated user or guest token, including parked connections. Heartbeats have a separate budget of ten per ten seconds; normal negotiated heartbeats remain supported.

Preview tasks have a global limit of 32 and a per-user limit of two. At capacity, message creation succeeds without starting another preview. Authentication and request trackers each hold at most 10,000 entries, with expired entries reclaimed on admission.

Membership, channel permissions and account state are reconciled against active voice states after API mutations and every two seconds. Revocations enqueue a durable LiveKit eviction before clearing voice state; failed removals retry. New voice join tokens expire after 60 seconds. **Self-hosted LiveKit does not invalidate an already issued JWT when removing a participant**, so a cached, unexpired token can reconnect; short expiry reduces this window but does not eliminate it. LiveKit Cloud supports token revocation on removal. See [LiveKit token behavior](https://docs.livekit.io/frontends/reference/tokens-grants/). No end-to-end media test against a production LiveKit deployment was performed.

Attachment downloads require live metadata and use `Cache-Control: no-store`. Database deletion triggers enqueue local file removal, including cascades. Failed removals retry across restarts. An hourly scan removes pre-upgrade orphan files older than one hour; the grace period protects new uploads before their metadata commits. The general CDN serves only named asset directories, so encoded paths cannot fall back to serving orphan attachments. Existing downstream cached copies cannot be recalled.
