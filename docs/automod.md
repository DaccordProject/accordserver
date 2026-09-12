# Local attachment moderation

AutoMod scans uploads before publishing their attachments. It runs a specialized
NudeNet 320n image detector on the server CPU, using ONNX Runtime from Rust. No
Python process, GPU, or cloud account is needed. Existing installations keep
model scanning disabled until an operator enables a policy. Explicit hash blocks
and upload rate limits work independently of model scanning.

The server includes the local inference code; the model and native runtime are
optional installed assets. Neither is automatically downloaded when the server
starts. Inference works offline after installation. Missing or broken assets
keep uploads pending when an enabled rule requires scanning.

## Install and enable

On Linux x86_64, run the checksum-verifying helper:

```sh
./scripts/setup-automod.sh ./data/automod-model
```

For video sampling, install your operating system’s `ffmpeg` package as well
(e.g. `apt install ffmpeg` on Debian/Ubuntu or `brew install ffmpeg` on macOS).
Both `ffmpeg` and `ffprobe` must be on PATH, or set the executable paths below.
The model helper installs only the image model and ONNX Runtime.

Set the three environment variables printed by the helper and restart
`accordserver`. It installs approximately 12.2 MB of model weights and downloads
an approximately 7.8 MB compressed CPU runtime archive. These sizes do not describe
RAM consumption. The Rust `ort` binding is pinned to its ONNX Runtime 1.22 API.

For macOS, Windows, or Linux ARM64, install the appropriate CPU archive from
[ONNX Runtime 1.22.0](https://github.com/microsoft/onnxruntime/releases/tag/v1.22.0)
and the [NudeNet 320n model](https://github.com/notAI-tech/NudeNet/tree/v3).
Set `ACCORD_AUTOMOD_RUNTIME_PATH` to the extracted `.dylib`, `.dll`, or `.so` and
`ACCORD_AUTOMOD_MODEL_PATH` to the ONNX file. Those platforms require their own
packaging and performance validation. Model/runtime licenses remain in their
upstream distributions; the repository does not bundle the weights.

The Linux amd64 Docker image includes ONNX Runtime 1.22, FFmpeg and FFprobe.
Model weights remain optional: mount the helper's asset directory read-only at
`/app/data/automod-model` and set `ACCORD_AUTOMOD_SCANNER=local`. The image already
sets the model and runtime paths; do not override its runtime path with the
helper's host path. For example, add this Compose override to your deployment:

```yaml
services:
  accordserver:
    volumes:
      - ./data/automod-model:/app/data/automod-model:ro
    environment:
      ACCORD_AUTOMOD_SCANNER: local
```

Enable a policy after starting the container. Desktop launches likewise
need the environment variables and locally installed assets; the client
configuration UI is tracked in DaccordProject/daccord#322.

Check readiness with an instance administrator's bearer token:

```sh
curl -H "Authorization: Bearer $ACCORD_ADMIN_TOKEN" \
  http://localhost:39099/api/v1/automod/health
```

`scanner: "ready"` means the model loaded. Health also returns the model identity,
queue counts, held bytes and configured capacity. Model identity includes a hash
of the actual weights plus the preprocessing version.

Enable the default quarantine policy for the whole server (`*`) or one space
(replace `*` with its ID):

```sh
curl -X PUT -H "Authorization: Bearer $ACCORD_ADMIN_TOKEN" \
  -H 'Content-Type: application/json' \
  http://localhost:39099/api/v1/automod/\*/policy \
  --data '{"enabled":true}'
```

This installs the default rules: check blocked hashes in all channels, and
quarantine explicit exposed-body-part detections with scores at least `0.8` in
channels whose `nsfw` flag is false. `manage_messages` permission exempts a
member by default. The threshold is an initial policy setting, not a calibrated
probability or a measured guarantee; tune it against representative community
images and moderator review decisions. The detector covers exposed body parts,
not all forms of sexual content or every drawing style.

## Configuration

| Environment variable | Default | Meaning |
|---|---|---|
| `ACCORD_AUTOMOD_SCANNER` | `none` | `local`, `http`, or `none` |
| `ACCORD_AUTOMOD_MODEL_PATH` | empty | Local NudeNet 320n ONNX file |
| `ACCORD_AUTOMOD_RUNTIME_PATH` | empty | Local ONNX Runtime 1.22 shared library |
| `ACCORD_AUTOMOD_FFMPEG_PATH` | `ffmpeg` | Video frame decoder executable |
| `ACCORD_AUTOMOD_FFPROBE_PATH` | `ffprobe` | Video metadata probe executable |
| `ACCORD_AUTOMOD_THREADS` | `2` | Local inference threads, clamped to 1–4 |
| `ACCORD_AUTOMOD_MAX_HELD` | `1000` | Maximum pending/quarantined/rejected files |
| `ACCORD_AUTOMOD_MAX_HELD_BYTES` | `1073741824` | Combined held upload byte limit |
| `ACCORD_AUTOMOD_SCANNER_URL` | empty | HTTP scanner URL, when selected |
| `ACCORD_AUTOMOD_SCANNER_SECRET` | empty | HTTP bearer secret |
| `ACCORD_AUTOMOD_MODEL_VERSION` | empty | Required HTTP scanner version identity |

One local inference runs at a time, outside Tokio's async executor. A 30-second
scan timeout does not spawn unlimited replacement inference tasks: the blocking
task retains its permit until it finishes. Failed scans retry after 60 seconds.
Inference runs without the worker lock held, so moderator review calls and
policy edits are never queued behind a running scan; the upload row and policy
are re-read and the decision re-derived after the scan, so a review or policy
change made while the scanner was busy always wins.
Queue admission returns HTTP 429 before creating a message if capacity is full.
Existing attachment size/count limits still apply. Policy retention is 1–90 days
(default 7); expired held files are removed, while decision metadata remains in
the audit ledger. Terminal file deletion retries after failures.

Decode limits are 16 million pixels, a maximum padded square of 16 million
pixels, maximum dimensions of 8192, and a 128 MB decoder allocation budget.
JPEG, still PNG and still WebP are supported. MP4/MOV, WebM/Matroska and AVI
videos use five-frame sampling. GIF, animated PNG/WebP, other formats, and
malformed images remain held when a media scan is required.
MIME types supplied by uploaders do not bypass inspection. Channels or roles
explicitly exempted by policy can still post these formats.

## Video sampling

Videos are recognized by their bytes, regardless of the uploaded filename or
MIME type. FFprobe checks duration and dimensions, then FFmpeg extracts one frame
at **10%, 30%, 50%, 70%, and 90%** of the video duration. Each frame is scaled and
padded to 320×320, and the same configured image model classifies all five.
The maximum score for each category is used for rule evaluation, so one flagged
sample is enough; scores are not averaged. Sample timestamps are recorded in
`sampled_timestamps_ms` in the scan result and audit details. Very short clips
can yield repeated frames; inability to extract all five samples holds the file.

Sampling deliberately trades coverage for cost: content between these five
positions can be missed. This is not whole-video classification. Audio is not
classified. The sampler supports one video stream, dimensions up to 4096 per
side / 16 million pixels, and duration up to ten minutes. Clips exceeding those
limits, missing decoder tools, decode failures and timeouts stay pending for
retry or review. Video and image results share the same policy/quarantine flow.

FFmpeg runs one decoding thread and one filter thread at a time. Each subprocess
has a ten-second deadline, bounded output, and is killed when its task is
cancelled. The complete sampling plus five scans is also subject to the
30-second scan deadline. Demuxers are selected from recognized container types;
playlists/network protocols are not accepted, and only file/pipe protocols are
enabled. No GPU is used. See the upstream [FFmpeg](https://ffmpeg.org/ffmpeg.html)
and [FFprobe](https://ffmpeg.org/ffprobe.html) documentation for the underlying tools.

## Policies and rules

Policies are persisted alongside server configuration in `automod_policies`.
`GET /api/v1/automod/{scope}/policy` returns the effective policy and whether it
is inherited. `PUT` replaces a policy; omitted fields take documented defaults.
`DELETE` removes an override and restores inheritance (or the disabled factory
default when deleting `*`). Space overrides are complete policies, not merges.
Instance policy requires server administrator access; space configuration
requires `manage_space`.

Example full policy:

```json
{
  "enabled": true,
  "retention_days": 7,
  "exempt_roles": [],
  "exempt_permissions": ["manage_messages"],
  "rules": [
    {
      "id": "blocked-file",
      "scope": {"type": "all"},
      "trigger": {"type": "hash_denylist"},
      "action": {"type": "quarantine"}
    },
    {
      "id": "explicit-image",
      "scope": {"type": "non_nsfw"},
      "trigger": {
        "type": "media",
        "categories": ["FEMALE_BREAST_EXPOSED", "FEMALE_GENITALIA_EXPOSED", "MALE_GENITALIA_EXPOSED", "ANUS_EXPOSED"],
        "threshold": 0.8
      },
      "action": {"type": "quarantine"}
    }
  ]
}
```

Other scopes: `{"type":"channels","ids":["channel-id"]}`.
Other triggers:

- `{"type":"non_nsfw_attachment"}` matches any attachment outside an NSFW channel.
- `{"type":"low_trust","min_account_age_hours":24,"min_space_age_hours":24,"require_role":true}`
  matches if any configured trust requirement is unmet. Space tenure and roles
  apply only to space channels.

Actions are `quarantine`, `reject`, `flag`, and `timeout` (with `seconds`, maximum
86400). A timeout also quarantines and is permitted only for a separate hash or
trust rule. It cannot be attached to a media score. Instance admins and space
owners cannot be automatically timed out, and existing longer timeouts remain.
`flag` publishes the attachment and atomically creates an `nsfw` report under the
System account in the existing reports queue.

Deterministic reject rules are checked before accepting an upload so the poster
gets an immediate HTTP 400 with the rule ID. Otherwise, the worker uses the first
matching rule in policy order. All applicable media rules must be evaluated
before deciding that no rule matched. Unknown scanner categories cause a hold.
Hash decisions run without a model when they match before a media rule.

Results are cached for one day by SHA-256 and scanner/preprocessing identity
(up to 10,000 entries, pruned hourly). Policy is evaluated again against cached scores, so a
changed threshold takes effect without re-running inference. Updating the
weights changes the local cache identity. Exact hashes do not catch modified or
re-encoded copies. Every new local attachment stores its SHA-256 as indexed
`content_hash`, including uploads made while scanning is disabled. Legacy rows
may have a null hash; blocking one computes and stores it from the local file
without downloading remote URLs. Identical uploads still own separate files;
content-addressed storage and perceptual matching are follow-up work.

Hash blocks apply to every route that stores a file, not only message
attachments: avatars, member avatars, space icons and banners, custom emoji and
soundboard clips are checked against the same denylist before they are written.
Account-level images (user avatar and banner) are checked against instance-wide
blocks; images belonging to a space are checked against that space's blocks as
well. These routes are not scanned by the model, which remains limited to
message attachments.

A moderator with `manage_messages` can block an existing attachment with:

```http
POST /api/v1/automod/{space_id}/attachments/{attachment_id}/block
Content-Type: application/json

{"reason":"Repeated prohibited image"}
```

Use `*` instead of the space ID for an instance-wide block (instance admin only).
The response includes `content_hash` and `scope_id`. Then use the normal
message-delete action to remove the original. The block, moderator ID, timestamp
and audit event survive message deletion. Scope mismatches are rejected.

Explicit blocks reject identical uploads with HTTP 400 even if scanning is
disabled, the uploader is exempt, or a policy omits the hash rule. An enabled,
applicable hash rule can instead apply its configured action, such as quarantine.
Space blocks apply only to that space; instance blocks apply to every space and
DM. Removing a block uses the existing hash-delete endpoint.

## Message lifecycle and review API

`POST /api/v1/channels/{channel}/messages/upload` returns HTTP 202 when scanning
is queued, with the ordinary message in `data` and IDs in `pending_attachments`.
Message text can appear immediately; withheld attachment URLs and bytes cannot.
Clients must treat 202 as accepted and can show an attachment processing state.
Disabled/exempt uploads keep HTTP 200 and immediate attachment delivery unless
the file has an explicit hash block.

The queue stores originals in `<storage>/automod/<id>`, outside every static
file route. Withheld originals never have public attachment records. Safe
publication writes the CDN file and commits the attachment row with the decision,
report (if any), and audit record. `message.update` adds the released attachment
or removes a withdrawn one. CDN downloads revalidate against live database state,
including conditional and range requests; `Cache-Control: private, no-cache`
allows cached bytes only after revalidation. Approved attachments otherwise retain
the existing URL-based CDN access behavior.

| Endpoint | Access / purpose |
|---|---|
| `GET /api/v1/automod/health` | Instance admin; scanner readiness and backlog |
| `GET /api/v1/automod/{scope}/uploads?status=quarantined&before=ID` | Moderator queue, newest first, 100 per page; `*` lists all for instance admins |
| `GET /api/v1/automod/uploads/{id}` | Uploader or moderator; status/reason without content |
| `GET /api/v1/automod/uploads/{id}/content` | Moderator only; download private original, `no-store`, sandboxed octet-stream |
| `PATCH /api/v1/automod/uploads/{id}` | Moderator; review decision with mandatory reason |
| `GET /api/v1/automod/{scope}/events?before=ID` | Moderator; persistent decision/configuration ledger |
| `POST /api/v1/automod/{scope}/attachments/{id}/block` | Channel `manage_messages`, or instance admin for `*`; block a stored attachment |
| `GET /api/v1/automod/{scope}/hashes?before=HASH` | Configurator; hash denylist, descending order |
| `PUT /api/v1/automod/{scope}/hashes/{sha256}` | Configurator; block exact file, JSON `{"reason":"..."}` |
| `DELETE /api/v1/automod/{scope}/hashes/{sha256}` | Configurator; unblock file |

Review body: `{"action":"release","reason":"False positive reviewed"}`.
Actions: `release`, `quarantine`, `reject`, `remove`, `retry`. Space review
requires `moderate_members`; DM/deleted-space review is available to instance
admins. Originals survive deletion of their message/channel/member for the
retention window; release cannot resurrect a deleted message. `reject` following
a model scan is an asynchronous final status, not a retroactive HTTP failure.
Disabling policy while uploads are pending holds them for explicit review.

Gateway events `automod.upload_update` (moderation intent) and
`automod.upload_status` (messages intent, uploader only) contain IDs/status.
Moderator permissions are checked when notifications are delivered, not just
when a session subscribes; the check runs after the cheaper space and intent
filters, so sessions that would not receive the event cost nothing. Detailed scores and rules are available through the
review/audit APIs. Automatic space decisions also appear in the existing space
audit log under the System account, and are broadcast as `audit_log.create`
like every other audit entry, so an open moderation view updates live.

The queue assumes one server writer process per database/storage directory,
matching the existing deployment. Pending work resumes after process restarts.
Full-frame video scanning, animated-image scanning, retroactive scanning of
existing uploads, model scanning of avatars/emoji, federated remote media and
the client review/configuration UI are outside this attachment implementation.

## Optional HTTP scanner

The server sends a POST with raw uploaded bytes as `application/octet-stream`
and `Authorization: Bearer <secret>`. Redirects are disabled. Responses are
limited to 64 KiB and must match:

```json
{"model_version":"operator-configured-version","scores":{"explicit":0.95,"safe":0.05}}
```

Scores must be finite numbers in [0,1], and every configured category must be
present. `model_version` must equal `ACCORD_AUTOMOD_MODEL_VERSION`; change it when
the endpoint changes model/preprocessing. Video samples are submitted to the
HTTP image classifier individually; the server aggregates the five results.
The HTTP backend uses the same image/decoder limits. `none` keeps deterministic rules usable, but an enabled
media rule will hold rather than skip a scan if no scanner is available.

## Validation

Native preprocessing has reference fixtures generated with OpenCV 4.10 and
[NudeNet v3’s preprocessing](https://github.com/notAI-tech/NudeNet/blob/v3/nudenet/nudenet.py), covering BGR channel order, square padding, up/downscaling and
fixed 320-pixel inputs. The server does not depend on OpenCV.

`cargo test --test automod` exercises quarantine, release/withdrawal, authorization,
queue capacity, cache policy changes, scanner failures, retention, and restart
recovery with a deterministic scanner. Run the real CPU backend explicitly:

```sh
ACCORD_AUTOMOD_MODEL_PATH=/absolute/path/320n.onnx \
ACCORD_AUTOMOD_RUNTIME_PATH=/absolute/path/libonnxruntime.so \
cargo test --test automod real_local_model_cpu_smoke -- --ignored --nocapture
```

This smoke test confirms model compatibility and inference on a synthetic image;
it does not measure classification accuracy. Evaluate false positives/negatives
on representative images before using rejection or punitive policies.


The FFmpeg sampler can also be tested with a generated ten-second video:

```sh
ACCORD_AUTOMOD_FFMPEG_PATH=/absolute/path/ffmpeg \
ACCORD_AUTOMOD_FFPROBE_PATH=/absolute/path/ffprobe \
cargo test --test automod real_video_sampler_extracts_five_spaced_frames -- --ignored --nocapture
```


## Slowmode and upload limits

Channel `rate_limit` now enforces a cooldown of 0–21600 seconds (0 disables it).
Text, thread replies and multipart messages share one cooldown per user and
channel. Reservations and message insertion are atomic, so concurrent sends
cannot both pass. Cooldowns survive message deletion and server restart.
Effective `manage_messages` or `manage_channels` permissions, space ownership
and instance admin privileges exempt users from slowmode.

Uploads also use independent per-user token buckets across all channels,
including DMs. Multipart message uploads and the base64 ingest routes (avatars,
member avatars, space icons and banners, emoji and soundboard clips) share one
bucket, so the budget bounds a user's total upload bandwidth rather than one
endpoint's. These apply to moderators and admins as well:

| Server setting | Default | Accepted values |
|---|---|---|
| `upload_requests_per_minute` | 6 | 1–600 |
| `upload_bytes_per_minute` | 52428800 (50 MiB) | 1–1099511627776 |

Configure them through `PATCH /api/v1/admin/settings`. Both are exposed in
client-facing settings. Capacity starts full and refills continuously over one
minute. Multipart file bytes are charged while reading each chunk, independent
of `Content-Length`, before disk writes or publication; base64 routes are
charged once the payload is decoded, also before any disk write. Failed attempts retain
charges for requests and bytes already accepted. Each request must fit the byte
budget; keep it at least as large as the largest upload you intend to allow.
Upload buckets are bounded in memory and reset on restart; slowmode uses the
database. Text messages continue to use the existing general request limiter.

Both controls return HTTP 429 with a `Retry-After` header and numeric
`error.retry_after` in seconds. The existing maximum file size and attachment
count limits still apply.
