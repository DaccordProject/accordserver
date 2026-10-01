# Private chat encryption v1

Private `dm` and `group_dm` channels accept only authenticated encrypted envelopes for new messages and edits. Space channels retain their plaintext protocol and reject private encryption envelopes. Existing plaintext history is retained; system messages continue to use their existing type. The database message writer enforces admission for REST, MCP, and federated sends, so another entry point cannot downgrade a private send. Encrypted private attachments bypass server-side content scanning and URL unfurling because the server has no plaintext. Rate limits, attachment counts/size limits, participant authorization, and gateway routing remain enforced.

Deploy with the companion Daccord client PR. Old clients cannot send private messages after this upgrade. Every participant must first connect an updated client to register a public identity. A second device imports the account's existing passphrase-protected identity backup. The server has no private-key recovery/reset endpoint. SQLite migration 037 and PostgreSQL migration 022 create the identity, canonical chat-context, and replay-token tables. User/channel deletion cascades their associated key/context rows. Accepted message tokens survive message deletion and edits until their account is deleted.

## API

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

## Security properties and limits

The server sees participant identities, group names, timestamps, reply relationships, reactions, message/file counts, and ciphertext sizes. It stores/forwards ciphertext and public keys only. Existing plaintext history is not retroactively protected, and calls retain their existing transport. Client key pinning uses trust on first use and provides fingerprint comparison for detecting first-use directory substitution. There is no independent audit, forward secrecy, post-compromise recovery, automatic secret synchronization, or per-device revocation. This is a static account-identity envelope protocol, not Signal's Double Ratchet or MLS. A compromised account identity can decrypt retained envelopes addressed to it; encrypted backups protect the same long-lived identity rather than adding ratcheting.

Server-side scanners and report queues cannot inspect private plaintext. A deliberately reporting client could disclose content separately, but this protocol does not automatically send decrypted private messages or file keys to moderators. The two server backends share the same admission code and separate equivalent migrations. `tests/private_chat_encryption.rs` tests admission, immutable keys, tampering, membership, replay, edit ownership, plaintext rejection, and channel exclusion; the two-server federation regression exercises canonical identities, encrypted forwarding and replica edits.
