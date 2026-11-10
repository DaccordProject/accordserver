# Private chat encryption

Version 2 (per-device keys, device approval and revocation by account-key rotation) is described in [Version 2](#version-2-per-device-keys) below. Version 1 envelopes remain readable and admissible; updated clients send only v2.

## Version 1 (legacy-readable)

Private `dm` and `group_dm` channels accept only authenticated encrypted envelopes for new messages and edits. Space channels retain their plaintext protocol and reject private encryption envelopes. Existing plaintext history is retained; system messages continue to use their existing type. The database message writer enforces admission for REST, MCP, and federated sends, so another entry point cannot downgrade a private send. Encrypted private attachments bypass server-side content scanning and URL unfurling because the server has no plaintext. Rate limits, attachment counts/size limits, participant authorization, and gateway routing remain enforced.

Deploy with the companion Daccord client PR. Old clients cannot send private messages after this upgrade. Every participant must first connect an updated client to register a public identity. A second device imports the account's existing passphrase-protected identity backup. The server has no private-key recovery/reset endpoint. SQLite migration 038 and PostgreSQL migration 023 create the identity, canonical chat-context, and replay-token tables. User/channel deletion cascades their associated key/context rows. Accepted message tokens survive message deletion and edits until their account is deleted.

### v1 API

- `PUT /api/v1/users/@me/encryption`: bearer-authenticated registration of `{exchange_key, signing_key}` (canonical padded base64, 32 bytes each). Registration is idempotent for an identical key and returns 409 for replacement. The identity's wire user ID is fixed at registration.
- `GET /api/v1/channels/{channel_id}/encryption`: participant-only discovery of `{channel_id, self_user_id, cdn_url, participants}`. Each participant row has `{user_id, wire_user_id, current, identity}`. `identity: null` means the user has not provisioned a key. Former message authors are included for authenticating history; only `current: true` rows receive new message keys. Local aliases are separate from canonical wire IDs.
- Existing JSON create/edit and multipart upload endpoints carry the encrypted content string. Private uploads use `attachment-N.bin` and `application/octet-stream`; original names/types and file keys are inside the encrypted message. Edits must be made by the original author.
- `/federation/v1/encryption/identity` and `/federation/v1/encryption/chat` use the existing signed peer request, trust, replay, and rate-limit checks. Identity requests can read only users homed on the target server. Chat discovery proves that the requesting actor belongs to the signing peer and is a chat participant. Cached remote identities are immutable. Private sends/edits/files are forwarded to the chat home as ciphertext and mirrored through the private message event types.

The content string is `daccord-e2ee:1:` followed by a JSON object with exactly `payload` and `signature`. `signature` is padded base64 Ed25519 over UTF-8 compact JSON encoding of the payload array:

```
[1, channel_context, author_context, message_token, reply_context,
 edit_message_context, ephemeral_public_key, encrypted_body, recipients]
```

The token is 16 random bytes. Reply and edit contexts are null or IDs in the chat home's namespace. Chat and user contexts are qualified by their home domain when federation is enabled. Recipient entries are `[wire_user_id, exchange_public_key, wrapped_content_key]`, sorted lexicographically by wire user ID, with exactly one entry per current participant, including the sender. The server checks the signature, contexts, recipient set and public keys. Each accepted send or revision reserves a unique `(author_id, token)` transactionally; replay returns 409 and never creates another message/revision. The envelope is limited to 256 KiB, its encrypted body to 64 KiB, and its recipients to 100.

AES-256-GCM boxes are base64 of `12-byte nonce || ciphertext || 16-byte tag`. Each message has a fresh random 32-byte content key and ephemeral X25519 pair. The body encrypts JSON `{content, files, embeds}`. File descriptors contain `{filename, content_type, size, key}`; actual file boxes are uploaded as binary opaque ciphertext with independently generated file keys.

```
context = UTF8(JSON(["daccord-e2ee-v1", channel, author, token,
                     reply, edit, ephemeral_public_key]))
shared = X25519(ephemeral_private, recipient_exchange_public)
wrapping_key = HKDF-SHA256(shared, salt = empty,
  info = UTF8(JSON(["daccord-e2ee-wrap-v1", base64(context),
                    recipient_id, recipient_exchange_public])), length = 32)
wrapped_key_aad = UTF8(JSON([base64(context), recipient_id, recipient_exchange_public]))
body_aad = context
file_aad = UTF8(JSON(["daccord-e2ee-file-v1", channel_context]))
```

Receivers verify the pinned sender signing key and envelope context before unwrapping/decrypting. File boxes authenticate before preview/save. Compact JSON arrays avoid object-order ambiguity across Dart and Rust. The shared test fixture includes publicly disclosed test seeds and proves that the Dart-generated envelope verifies and passes admission in Rust. These seeds must never be used for actual accounts.

### v1 security properties and limits

The server sees participant identities, group names, timestamps, reply relationships, reactions, message/file counts, and ciphertext sizes. It stores/forwards ciphertext and public keys only. Existing plaintext history is not retroactively protected, and calls retain their existing transport. Client key pinning uses trust on first use and provides fingerprint comparison for detecting first-use directory substitution. There is no independent audit, forward secrecy, post-compromise recovery, automatic secret synchronization, or per-device revocation. This is a static account-identity envelope protocol, not Signal's Double Ratchet or MLS. A compromised account identity can decrypt retained envelopes addressed to it; encrypted backups protect the same long-lived identity rather than adding ratcheting.

Server-side scanners and report queues cannot inspect private plaintext. A deliberately reporting client could disclose content separately, but this protocol does not automatically send decrypted private messages or file keys to moderators. The two server backends share the same admission code and separate equivalent migrations. `tests/private_chat_encryption.rs` tests admission, immutable keys, tampering, membership, replay, edit ownership, plaintext rejection, and channel exclusion; the two-server federation regression exercises canonical identities, encrypted forwarding and replica edits.

## Version 2: per-device keys

v2 gives each device its own keys so a lost device can be removed without affecting the others, lets a new device be approved from an existing one by comparing a code, and keeps contacts trusting an account across device changes without re-verifying fingerprints. The v1 identity becomes **account version 1**; pins, fingerprints and history keep working. SQLite migration 040 and PostgreSQL migration 025 add `e2ee_account_versions` (backfilled with version 1 from `e2ee_identities`), `e2ee_devices`, and `account_version`/`refreshed_at` columns on `e2ee_identities`. All rows cascade on user deletion.

### Keys

- **Account key, version n**: X25519 `exchange_key` + Ed25519 `signing_key`, validated as in v1. Contacts pin the account signing key trust-on-first-use as in v1. Every approved device holds the private account keyring (all versions).
- **Device key**: per-device X25519 + Ed25519. `device_id` is 16 random bytes in base64url without padding (exactly 22 characters of `[A-Za-z0-9_-]`); `name` is 1–64 characters of user-visible plaintext.
- **Device certificate**: padded base64 Ed25519 signature by account signing key version `account_version` over the compact JSON `["daccord-e2ee-device-v2", user_context, account_version, device_id, exchange_key, signing_key]` (`account_version` is a JSON integer; `user_context` is `e2ee_identities.user_context`, qualified when federation is enabled).
- **Succession**: version n+1 is signed by version n's signing key over `["daccord-e2ee-rotate-v2", user_context, n+1, new_exchange_key, new_signing_key]`. Verifiers walk the chain from their pinned version; a broken or missing link is treated like a v1 identity change.

The link (comparison) code and key-package encryption are client-only; the server only checks the key package's shape: a JSON string `{"ephemeral": base64(32 bytes), "box": base64(nonce || ciphertext || tag)}` of at most 16 KiB.

### Device lifecycle

`status` is `pending`, `active` or `revoked`. A device that holds the keyring registers itself active with a certificate by the current account key; otherwise it registers pending and an active device approves it by uploading a certificate and a key package (the keyring sealed to the pending device). Removing active devices always rotates the account key; removed devices become `revoked` tombstones (kept so their history still verifies) and the sessions bound to them are deleted. Pending devices can be declined without rotation and expire 24 hours after creation. An account has at most 20 non-revoked devices. A revoked `device_id` can never be registered again.

Each device row is bound to the `user_tokens` session that last registered it (a session is bound to at most one device). Every client start re-registers idempotently, refreshing the binding and `last_seen_at`. Bot and guest tokens cannot manage devices (403).

### v2 API (bearer auth, under `/api/v1`, responses in the usual `{"data": …}` envelope)

`DeviceRow` = `{device_id, name, exchange_key, signing_key, certificate|null, account_version|null, status, created_at, last_seen_at, current}`; `current` is true for the row bound to the calling session. Timestamps are RFC 3339 UTC.

- `PUT /users/@me/encryption` is unchanged (v1 registration of account version 1). After a rotation it is idempotent for the *current* key and returns 409 otherwise.
- `GET /users/@me/encryption` → `{user_context, account: {version, exchange_key, signing_key}|null, successions: [{version, exchange_key, signing_key, signature}], devices: [DeviceRow]}`. Successions cover versions ≥ 2 ascending; devices are pending and active rows.
- `PUT /users/@me/encryption/devices/{device_id}` with `{name, exchange_key, signing_key, certificate?, account_version?}`. Device keys are immutable per ID (409); the name may change. With a certificate (both fields together), the account must exist (400 otherwise), `account_version` must be current (409) and the certificate must verify (400): the device becomes active. Without one it is pending unless already active; a newly pending device emits `encryption.device_pending` with its `DeviceRow`. Revoked IDs return 403 "This device was removed from encrypted chats."; a 21st device returns 400.
- `POST /users/@me/encryption/devices/{device_id}/approve` with `{certificate, account_version, key_package}`. The caller's session must be bound to an active device (403); the target must be pending (404 unknown, 409 otherwise); the certificate must verify against the current version. Emits `encryption.devices_update`.
- `GET /users/@me/encryption/devices/{device_id}/package` → `{key_package}` for the session bound to that device only (403 for other sessions, 404 when there is none). `DELETE` on the same path acknowledges and deletes it.
- `DELETE /users/@me/encryption/devices/{device_id}` declines a pending device (row deleted, not tombstoned) from an active device's session or the pending device's own session. Active devices return 409 "Use rotation to remove an active device." Emits `encryption.devices_update`.
- `POST /users/@me/encryption/rotate` with `{revoke: [device_id], account: {exchange_key, signing_key}, version, signature, devices: [{device_id, certificate}], packages: [{device_id, key_package}]}`. The caller must be bound to an active device that is not being revoked (400); `version` must be current + 1 (409); `signature` is the succession by the current key and the new keys must not reuse an earlier version's (400); `revoke` must name active devices, `devices` exactly the remaining active devices (including the caller) and `packages` exactly the remaining active devices except the caller (409 on any set mismatch); each certificate must verify against the new key (400). Atomically: the version is recorded, `e2ee_identities` moves to the new key, revoked rows get `revoked_at` and their bound sessions are deleted, certificates and packages are replaced, and pending devices are deleted. Emits `encryption.devices_update` and returns the `GET /users/@me/encryption` shape. `revoke` may be empty.

Gateway events use `op: 0`, intent `direct_messages`, and target only the account's own sessions: `encryption.device_pending` (data `DeviceRow` with `current: false`) and `encryption.devices_update` (data `{}`). Clients must request the `direct_messages` intent to receive them.

Channel discovery (`GET /channels/{id}/encryption`) rows gain `account_version` (null without an identity), `successions`, and `devices`: `[{device_id, exchange_key, signing_key, certificate, account_version, revoked}]` with active and revoked, never pending, devices. `identity` stays the current account key, so v1 clients keep working until a rotation happens.

### v2 envelope

Content is `daccord-e2ee:2:` followed by compact JSON `{"payload": p, "signature": s}` (exactly these keys):

```
p = [2, channel_context, author_context, device_id, token, reply_context,
     edit_context, ephemeral_public_key, encrypted_body, recipients]
s = base64 Ed25519 signature by the SENDER DEVICE signing key over compact JSON(p)
recipient = [user_context, key_id, exchange_public_key, wrapped_content_key]

context      = UTF8(JSON(["daccord-e2ee-v2", channel, author, device_id, token,
                          reply, edit, ephemeral_public_key]))
wrapping_key = HKDF-SHA256(X25519(ephemeral_private, recipient_exchange_public),
                 salt = empty, info = UTF8(JSON(["daccord-e2ee-wrap-v2",
                 base64(context), user_context, key_id, recipient_exchange_public])),
                 length = 32)
wrapped_aad  = UTF8(JSON([base64(context), user_context, key_id, recipient_exchange_public]))
body_aad     = context;  file_aad = UTF8(JSON(["daccord-e2ee-file-v1", channel_context]))
```

`key_id` is `"account"` (the participant's current account exchange key) or a device ID. The required recipient set has, for every current participant, one `"account"` entry with their current account key plus one entry per active device, sorted by `user_context` then `key_id` in code-unit order, without duplicates, at most 400 entries. The token is 16 random bytes; contexts, body JSON, file boxes and size limits are as in v1 (envelope ≤ 256 KiB, body box ≤ 64 KiB, wrapped keys exactly 60 bytes).

Admission (REST, MCP and the federation writer, as in v1) checks structure, contexts, the sender device (active and certified by the author's current account version; pending or unknown devices and revoked devices return 403), the device signature, and the exact recipient set. Wrong sets return 409 "Encryption keys changed. Refresh before sending."; clients refresh discovery and retry once. The send transaction reserves the token (payload item 4; item 3 in v1) in the shared `e2ee_message_tokens` table and rechecks the sender device and the recipient set under the chat lock. PostgreSQL additionally share-locks the participants' identity rows, which every rotation updates, so a send cannot interleave with a rotation.

Receivers verify the author's account chain, the device certificate, then the device signature, and unwrap with their own device entry or an `"account"` entry matching any keyring version. History from revoked devices still verifies.

### Federation

`/federation/v1/encryption/identity` responses add `account_version`, `successions` and `devices` (active and revoked, with certificates). Caches accept a new account version only through a valid succession chain from the cached version; older responses are ignored and an identity change at the same version is a 409, as in v1. Device rows are replaced from the home's response after certificate verification; cached revoked rows are never reactivated or dropped, and keys are immutable per device. A server first seeing an account at version n > 1 trusts it on first use and stores the part of the chain it can verify; device certificates from versions it never saw are skipped.

The chat home refreshes each current remote participant during discovery when its cache is older than 30 seconds (an unreachable server keeps the cached keys), and refreshes a forwarded message's author once when the sender device is not yet known. Replicas update their cache of *remote* users from the home's discovery response before admitting mirrored messages; their own users' keys are authoritative locally. Because a replica's view of its own users may be newer than the home's cache, replicas check v2 recipient membership but leave the exact recipient set to the chat home.

### Residual risks

Every approved device holds the account keyring, so a stolen device that is not removed can read everything addressed to the account; removal rotates the key so it cannot read later messages, but it keeps whatever it already had. A malicious server colluding with a thief could race a rotation before the owner does. Device-to-session binding proves only possession of the account's bearer token, not of the device key: someone holding a stolen session token can bind a known device ID's public keys to their session and read or delete its (still sealed) key package, but cannot approve devices or rotate without the account private key. Remote participants' device lists can be up to 30 seconds stale at the chat home, so a just-revoked remote device may still be addressed briefly. Link codes are 16 decimal digits (about 53 bits). There is still no forward secrecy, post-compromise security, or independent audit.

`tests/private_chat_encryption_v2.rs` covers registration, certificate failures, approval, packages, decline, expiry, the device cap, rotation, tombstones, v2 admission, replay, recipient sets, sender-device rules, v1 compatibility and discovery shape, and verifies the Dart interop fixture `tests/fixtures/e2ee_v2.json` when present. The two-server federation regression covers v2 device discovery and admission across servers.
