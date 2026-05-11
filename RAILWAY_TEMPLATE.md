# Publishing the Accord Server as a Railway Template

The deploy button in `README.md` uses Railway's **ad-hoc repo deploy** (`?template=<github-url>`), which only provisions the `accordserver` service. A real one-click experience — Postgres, persistent volume, env vars all wired in a single click — requires a **published Railway Template**, which can only be created through Railway's dashboard after a first manual deploy.

This guide walks you through that one-time setup so the deploy button truly becomes one click for everyone after you.

## Prerequisites

- A Railway account on a plan that supports publishing templates (Hobby+ at time of writing).
- A LiveKit Cloud project if you want voice in the template — get `LIVEKIT_API_KEY` / `LIVEKIT_API_SECRET` and your `wss://` URL from the LiveKit dashboard. Skip if voice should be off by default.
- A locally-generated `TOTP_ENCRYPTION_KEY`: `openssl rand -hex 32`.

## 1. Do a first deploy from the current button

1. Click the **Deploy on Railway** button in `README.md`.
2. Authorize Railway to read this repo if prompted.
3. In the env var form, paste the `TOTP_ENCRYPTION_KEY` and set `RUST_LOG=accordserver=info,tower_http=info`. Leave the `LIVEKIT_*` fields blank for now.
4. Click **Deploy**. Wait for the build (~3–5 min) to finish and the `/health` healthcheck to go green.

You now have a project with one service: `accordserver`. The next steps add the rest.

## 2. Add Postgres

1. In the project canvas, click **+ New → Database → Add PostgreSQL**.
2. Wait for Postgres to provision.
3. Open the `accordserver` service → **Variables** tab → **+ New Variable**.
4. Add `DATABASE_URL` with the value `${{Postgres.DATABASE_URL}}` (literal text — Railway resolves the reference at runtime).
5. The service will redeploy. Migrations run automatically on first connect.

> If you'd rather ship a SQLite-default template, skip Postgres and go to step 3.

## 3. (SQLite path) Add a Volume

Only needed if you skipped step 2.

1. `accordserver` service → **Settings → Volumes → + New Volume**.
2. Mount path: `/app/data`. Size: 1 GB is plenty for chat.
3. Redeploy. Without this, the SQLite file is lost on every redeploy.

## 4. (Optional) Wire LiveKit Cloud

1. `accordserver` → **Variables** → add:
   - `LIVEKIT_INTERNAL_URL` = your `wss://...livekit.cloud` URL
   - `LIVEKIT_EXTERNAL_URL` = same value
   - `LIVEKIT_API_KEY` = from LiveKit dashboard
   - `LIVEKIT_API_SECRET` = from LiveKit dashboard
2. Redeploy. Watch logs for `✓ livekit reachable`.

Do **not** add a LiveKit service to the Railway project. Railway's edge is TCP-only and LiveKit media needs UDP; signaling would connect but no audio would flow.

## 5. Generate a public domain

1. `accordserver` → **Settings → Networking → Generate Domain**.
2. Test:
   ```bash
   curl https://<your-domain>.up.railway.app/health
   curl https://<your-domain>.up.railway.app/api/v1/gateway
   ```
3. Test the WebSocket upgrade (Railway's proxy supports it natively):
   ```bash
   curl -i -N \
     -H "Connection: Upgrade" \
     -H "Upgrade: websocket" \
     -H "Sec-WebSocket-Version: 13" \
     -H "Sec-WebSocket-Key: $(openssl rand -base64 16)" \
     https://<your-domain>.up.railway.app/ws
   ```
   Expect `HTTP/1.1 101 Switching Protocols`.

## 6. Publish as a Template

1. In the project page, click the **⋯** menu → **Publish as Template** (or **Templates → Create Template** in the sidebar — Railway has moved this around).
2. Fill in:
   - **Name:** `Accord Server`
   - **Description:** "Self-hosted Discord-like chat & voice backend (Rust/Axum)."
   - **README:** point at this repo's `README.md`.
   - **Tags:** `chat`, `rust`, `discord`, `websocket`.
3. For each variable, mark whether it's **required** and add a description + default. Suggested settings:

   | Variable | Required | Default | Notes |
   |---|---|---|---|
   | `DATABASE_URL` | yes | `${{Postgres.DATABASE_URL}}` | Auto-resolved reference |
   | `RUST_LOG` | no | `accordserver=info,tower_http=info` | |
   | `TOTP_ENCRYPTION_KEY` | yes | *(generator: `openssl rand -hex 32`)* | Mark "user must provide" |
   | `LIVEKIT_INTERNAL_URL` | no | — | Voice off if unset |
   | `LIVEKIT_EXTERNAL_URL` | no | — | |
   | `LIVEKIT_API_KEY` | no | — | |
   | `LIVEKIT_API_SECRET` | no | — | |

4. Include the **Postgres** service and the **Volume** in the template (Railway picks them up automatically from your project).
5. Click **Publish**. Railway returns a **template URL** like `https://railway.com/template/abc123`.

## 7. Replace the README deploy button

Edit `README.md` line 5 — swap the ad-hoc URL for the published template URL:

```diff
-[![Deploy on Railway](https://railway.com/button.svg)](https://railway.com/new/template?template=https://github.com/daccordproject/accordserver&envs=...)
+[![Deploy on Railway](https://railway.com/button.svg)](https://railway.com/template/abc123)
```

Commit and push. The button now provisions accordserver + Postgres + volume + env prompts in one click.

## Maintaining the template

- **Code changes** redeploy automatically if you connected the template to this repo's main branch (Railway prompts during publish).
- **Schema changes** — when you publish a new template version, existing deployments are not migrated; only new deploys pick up the changes. Use Railway's "Update Template" flow when env vars or services change shape.
- **Revoking** — *Templates → your template → Settings → Unpublish* removes it from the marketplace but keeps existing deploys running.

## Troubleshooting

| Symptom | Cause | Fix |
|---|---|---|
| Healthcheck times out on first deploy | Build slow on cold cache | Bump `healthcheckTimeout` in `railway.json` to 300 |
| `password authentication failed` for Postgres | Stale env after Postgres re-provisioned | Redeploy `accordserver` so it re-reads `${{Postgres.DATABASE_URL}}` |
| `voice.server_update` returns but client can't hear audio | LiveKit pointed at a self-hosted instance on Railway (UDP blocked) | Switch `LIVEKIT_*` to LiveKit Cloud |
| SQLite DB empty after each deploy | No volume mounted | Add a volume at `/app/data` (step 3) |
