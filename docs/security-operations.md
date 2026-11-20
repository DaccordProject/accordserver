# Security operations

## Initial administrator

### Why you aren't an administrator after signing up

Creating an account, in Daccord or with `POST /api/v1/auth/register`, always makes an ordinary user. This is true even for the first account on a brand-new server, so a stranger who reaches your server first cannot take it over. Until an administrator exists, the server log says `no administrator configured`.

Every new server starts with a space called **General**. It is owned by a hidden **System** account. New accounts join General automatically as ordinary members: they can chat there, but they cannot rename it, add channels or change its settings.

What to do depends on what you want:

- **A space for you and your friends.** You don't need to be an administrator. Create your own space. You own it, so you have full control over its channels, roles, invites and settings. In Daccord 0.2.29 or later, click **+** (Add a server) in the space list and choose the **Create** tab, or click **Create a space** on the "No spaces yet" screen. Older Daccord versions have no create button; any API client can use `POST /api/v1/spaces`.
- **Customise General, or manage the whole server** (users, password resets, reports, server settings, AutoMod). You need an instance administrator. Create the first one as described below, then [make your everyday account an administrator](#make-your-everyday-account-an-administrator). Instance administrators bypass space permission checks, so they can edit General.

### Create the first administrator

Run the `--bootstrap-admin` command once, on the computer that runs the server. It:

- creates a **new** account with the username you give it (1–32 characters). It never promotes or overwrites an existing account, so choose a name nobody uses yet. The examples use `operator`.
- reads the password from the `ACCORD_BOOTSTRAP_PASSWORD` environment variable. It must be 16–128 characters.
- must use the **same database** as the running server, meaning the same `--data-dir` or `DATABASE_URL`. If both are set, `DATABASE_URL` wins.
- creates the account and exits. It doesn't start another server, so you can leave the running server alone.

On success it prints `administrator created; remove ACCORD_BOOTSTRAP_PASSWORD before starting the server`. If it stops with `could not create admin (existing usernames are never overwritten)`, the username is taken or the password is too short or too long. If it stops with `set ACCORD_BOOTSTRAP_PASSWORD`, the password variable wasn't set in that same terminal window. Fix the problem and run the command again.

The snippets below don't show the password while you type it and keep it out of your shell history. The last line of each one removes the password from the terminal session.

#### Desktop app

The desktop app (the Accord tray icon) installs its own copy of the server program, `accordserver`, and keeps its data in a per-user folder. Run that copy and pass the app's data folder with `--data-dir`. You don't need to quit Accord first. Run the commands as the same user who runs the tray app.

| Installed from | Server program | Data folder (`--data-dir`) |
|---|---|---|
| Linux `.deb` | `/usr/bin/accordserver` | `~/.local/share/accord` (`$XDG_DATA_HOME/accord` if you set `XDG_DATA_HOME`) |
| Linux `.AppImage` | `usr/bin/accordserver`, packed inside the AppImage | same as `.deb` |
| macOS `.dmg` | `/Applications/Accord.app/Contents/MacOS/accordserver` | `~/Library/Application Support/gg.daccord.Accord` |
| Windows `-setup.exe` | `%LOCALAPPDATA%\Accord\accordserver.exe` | `%APPDATA%\daccord\Accord\data` |
| Windows `.msi` | `C:\Program Files\Accord\accordserver.exe` | `%APPDATA%\daccord\Accord\data` |

These are the default install locations; use your own if you chose a different folder. The tray menu's **Open data folder** opens the data folder, which contains `accord.db`.

**Linux (`.deb`).** Open a terminal and run:

```bash
printf 'Admin password (16+ characters): '; stty -echo; read -r ACCORD_BOOTSTRAP_PASSWORD; stty echo; echo
export ACCORD_BOOTSTRAP_PASSWORD
accordserver --data-dir "${XDG_DATA_HOME:-$HOME/.local/share}/accord" --bootstrap-admin operator
unset ACCORD_BOOTSTRAP_PASSWORD
```

**Linux (AppImage).** The server program is packed inside the AppImage. Extract only that file into a temporary folder, run it, then delete the folder. Replace `~/Applications/Accord.AppImage` with the path to your AppImage:

```bash
work=$(mktemp -d) && cd "$work"
~/Applications/Accord.AppImage --appimage-extract usr/bin/accordserver
printf 'Admin password (16+ characters): '; stty -echo; read -r ACCORD_BOOTSTRAP_PASSWORD; stty echo; echo
export ACCORD_BOOTSTRAP_PASSWORD
./squashfs-root/usr/bin/accordserver --data-dir "${XDG_DATA_HOME:-$HOME/.local/share}/accord" --bootstrap-admin operator
unset ACCORD_BOOTSTRAP_PASSWORD
cd ~ && rm -rf "$work"
```

If the server reports that `libssl.so.3` is missing, run `--appimage-extract` without a file name to extract the whole AppImage. Then add `LD_LIBRARY_PATH="$work/squashfs-root/usr/lib"` at the start of the `./squashfs-root/usr/bin/accordserver` line.

**macOS.** Open Terminal and run the following (it works in the default zsh shell and in bash):

```sh
printf 'Admin password (16+ characters): '; stty -echo; read -r ACCORD_BOOTSTRAP_PASSWORD; stty echo; echo
export ACCORD_BOOTSTRAP_PASSWORD
/Applications/Accord.app/Contents/MacOS/accordserver --data-dir "$HOME/Library/Application Support/gg.daccord.Accord" --bootstrap-admin operator
unset ACCORD_BOOTSTRAP_PASSWORD
```

If macOS blocks the program, allow it under **System Settings → Privacy & Security** and run the command again.

**Windows.** Open PowerShell from the Start menu. Don't use **Run as administrator**: the data folder belongs to your user. Paste:

```powershell
$server = "$env:LOCALAPPDATA\Accord\accordserver.exe"
if (-not (Test-Path $server)) { $server = "$env:ProgramFiles\Accord\accordserver.exe" }
$secure = Read-Host 'Admin password (16+ characters)' -AsSecureString
$env:ACCORD_BOOTSTRAP_PASSWORD = [System.Net.NetworkCredential]::new('', $secure).Password
& $server --data-dir "$env:APPDATA\daccord\Accord\data" --bootstrap-admin operator
Remove-Item Env:ACCORD_BOOTSTRAP_PASSWORD
Remove-Variable secure
```

The first two lines find the server whether you installed with the `-setup.exe` or the `.msi`. The password variable exists only in that PowerShell window, and the last two lines remove it. Closing the window also discards it.

#### Docker Compose

Open a terminal in the folder that contains your `docker-compose.yml` and `.env`. Compose reads the LiveKit credentials from `.env` and refuses to run without them.

```bash
read -r -s -p 'Admin password (16+ characters): ' ACCORD_BOOTSTRAP_PASSWORD; echo
export ACCORD_BOOTSTRAP_PASSWORD
docker compose run --rm -e ACCORD_BOOTSTRAP_PASSWORD accordserver ./accordserver --bootstrap-admin operator
unset ACCORD_BOOTSTRAP_PASSWORD
```

For PostgreSQL, add `-f docker-compose.postgres.yml` after `docker compose`. The one-off container uses the same volume and `DATABASE_URL` as the running server. `-e ACCORD_BOOTSTRAP_PASSWORD` without a value copies the variable from your terminal, so the password doesn't appear in the command line.

#### From source or a standalone binary

Use the same `DATABASE_URL` or `--data-dir` as the server. Without either, both use `data/accord.db` relative to the current folder, so run the command from the folder you start the server in.

```bash
read -r -s -p 'Admin password (16+ characters): ' ACCORD_BOOTSTRAP_PASSWORD; echo
export ACCORD_BOOTSTRAP_PASSWORD
./accordserver --bootstrap-admin operator   # from a source checkout: cargo run --release -- --bootstrap-admin operator
unset ACCORD_BOOTSTRAP_PASSWORD
```

### Make your everyday account an administrator

1. Make sure the server is running. Daccord keeps one account per server, so if you're signed in with your everyday account, choose **Settings → Log out** first. Then sign in to the server with the bootstrap username and password, using the normal login screen. The account has no spaces; that's expected, because instance administration doesn't require space membership.
2. Open **Settings → Server administration → Users**. Find your everyday account, open the **⋮** menu next to it, and choose **Make admin**.
3. Log out, then sign back in with your everyday account. (If another administrator promotes you while you're signed in, quit and reopen Daccord 0.2.29 or later to pick up the change; older versions need a sign-out.)

Your everyday account now has **Settings → Server administration** and can edit General. Change the bootstrap account's password (see below) and keep it as a spare administrator. Over the API, an administrator can promote a user with `PATCH /api/v1/admin/users/{user_id}` and `{"is_admin": true}`. To make your account the owner of General, use `PATCH /api/v1/admin/spaces/{space_id}` with `{"owner_id": "<user id>"}`.

### If you edited the database by hand

Use `--bootstrap-admin` instead where you can. If you have already set `is_admin` yourself, for example in DB Browser for SQLite, check each of these:

1. **Stop the server before editing.** Quit Accord from the tray, run `docker compose stop accordserver`, or stop your service. Start it again after saving.
2. **Use the number `1`** to make an account an administrator (`0` removes it). Don't type `true`. Before 0.2.6, text in this column made sign-in fail for that account. 0.2.6 also reads `true`, but some checks compare against the number only.
3. **Save the change.** DB Browser keeps edits pending until you click **Write Changes**.
4. **Edit the database the server actually uses.** For the desktop app, that's `accord.db` in the data folder above. For Docker, it's `/app/data/accord.db` inside the `accordserver_data` volume. A copy on your computer only takes effect if you copy it back while the server is stopped. When copying, include any `accord.db-wal` and `accord.db-shm` files next to it.
5. **Refresh the account.** In Daccord 0.2.29 or later, quit and reopen Daccord; in older versions, sign out and back in.

### Rotating the administrator password

The initial password is the `ACCORD_BOOTSTRAP_PASSWORD` value you typed at provisioning. The server stores only its Argon2 hash and reads the variable only while `--bootstrap-admin` runs. Bootstrap is not a password-reset command: changing `ACCORD_BOOTSTRAP_PASSWORD` has no effect on an existing account, and rerunning `--bootstrap-admin` with the same username fails because provisioning only inserts new users.

Rotate it after the first sign-in, and whenever it may have been exposed:

- **Daccord:** open **Settings → Account → Password & Security**, enter the current and new passwords, then choose **Change password**.
- **API:** send an authenticated `POST /api/v1/auth/change-password` with `{"old_password": "…", "new_password": "…"}`. Use the bearer `data.token` returned by `POST /api/v1/auth/login`. This Bash example needs `curl` 7.55 or later and `jq` 1.6 or later. It expects `ACCORD_URL` (the server base URL) and `ACCORD_TOKEN` (that token) in the environment. Both passwords stay out of shell history. The passwords and the token also stay out of process arguments, which other local users can read: `printf` is a Bash builtin, and process substitution passes the values to `jq` and `curl` as file descriptors.

  ```bash
  read -r -s -p 'Current password: ' OLD_PW; echo
  read -r -s -p 'New password: ' NEW_PW; echo
  jq -n --rawfile old <(printf '%s' "$OLD_PW") --rawfile new <(printf '%s' "$NEW_PW") \
      '{old_password: $old, new_password: $new}' |
    curl -fsS -X POST "$ACCORD_URL/api/v1/auth/change-password" \
      -H @<(printf 'Authorization: Bearer %s\n' "$ACCORD_TOKEN") \
      -H 'Content-Type: application/json' --data-binary @-
  unset OLD_PW NEW_PW
  ```

The server checks the current password (a wrong one returns 401) and stores the new hash. It revokes every other bearer token for the account, while the session that made the request stays signed in. Other devices must sign in again with the new password. The route accepts 8–128 characters. Keep administrator passwords at 16 or more, as provisioning requires. Two-factor settings are unchanged.

If the password is lost, another administrator can use **Server administration → Users → Reset password**, or `POST /api/v1/admin/users/{user_id}/reset-password` with `{"new_password": "…"}`. This sets a temporary password, revokes all of that user's sessions, disables its two-factor authentication and backup codes, and marks the account so the next login response includes `force_password_reset: true`. Clients should prompt for a new password; changing it clears the flag. If no other administrator exists, provision a second one locally with a new username, as described above, and reset the original account from it.

### If sign-in fails

If sign-in fails, first confirm that provisioning and the running server use the same database. Record the client and server versions, the exact error, and server/client logs from the attempt. Distinguish a failed `POST /api/v1/auth/login` from a gateway connection that never receives `ready`, or a client that receives `ready` but keeps displaying a loading indicator. An empty space list is a valid signed-in state; it does not require manually changing `is_admin` in the database. Remove passwords and bearer tokens before sharing logs.

## Deployment credentials and proxy addresses

Both Compose configurations require `LIVEKIT_API_KEY` and `LIVEKIT_API_SECRET`. Generate independent values with `openssl rand -hex 32` and store them in an untracked `.env` with mode 600. PostgreSQL additionally requires `POSTGRES_PASSWORD` and a matching `DATABASE_URL`; URL-encode its password. LiveKit's signaling endpoint is exposed through Caddy, with only media TCP/UDP ports published directly.

For an existing deployment using `devkey` / `secret`, generate new credentials, update both Accord and LiveKit, and restart both services. This disconnects current calls and invalidates tokens signed with the old key. Updating a PostgreSQL environment variable does not change an existing role's password: rotate it in PostgreSQL, then update the application's connection URL and restart. Preserve the database volume.

Forwarded IPs require both `TRUST_PROXY_HEADERS=true` and `TRUSTED_PROXY_IPS`, a comma-separated list of the actual reverse proxies' literal IP addresses. All other peers use their socket IP. Configure the proxy to replace incoming forwarding headers. Without an allowlist, clients behind a proxy share one IP budget.

## Curated game signing and legacy retirement

Lua/native plugin uploads and execution routes are retired. Old tables remain
an inert archive for operator export; there is no automatic package migration.
Retired native signing configuration no longer enables any executable route.

Provision `EXPERIENCE_TRUSTED_KEYS` with approved key ids and hex Ed25519 public
keys from the master-server operator. Set `EXPERIENCES_ENABLED=true` explicitly.
Community servers accept only exact reviewed immutable WASM releases; publication
uses a separate master-server reviewer credential. See [experiences](experiences.md)
for approval freshness, outage behavior, revocation, rollback and key rotation.

## Revocation, limits and deleted files

Gateway credentials are revalidated before inbound operations and broadcast delivery, and every two seconds when idle or parked. Revoked, expired, deleted or disabled identities lose their sessions and replay eligibility. Gateway admission allows at most 512 connections globally, 32 per IP and eight per authenticated user or guest token, including parked connections. Heartbeats have a separate budget of ten per ten seconds; normal negotiated heartbeats remain supported.

Preview tasks have a global limit of 32 and a per-user limit of two. At capacity, message creation succeeds without starting another preview. Authentication and request trackers each hold at most 10,000 entries, with expired entries reclaimed on admission.

Membership, channel permissions and account state are reconciled against active voice states after API mutations and every two seconds. Revocations enqueue a durable LiveKit eviction before clearing voice state; failed removals retry. New voice join tokens expire after 60 seconds. **Self-hosted LiveKit does not invalidate an already issued JWT when removing a participant**, so a cached, unexpired token can reconnect; short expiry reduces this window but does not eliminate it. LiveKit Cloud supports token revocation on removal. See [LiveKit token behavior](https://docs.livekit.io/frontends/reference/tokens-grants/). No end-to-end media test against a production LiveKit deployment was performed.

Attachment downloads require live metadata and use `Cache-Control: no-store`. Database deletion triggers enqueue local file removal, including cascades. Failed removals retry across restarts. An hourly scan removes pre-upgrade orphan files older than one hour; the grace period protects new uploads before their metadata commits. The general CDN serves only named asset directories, so encoded paths cannot fall back to serving orphan attachments. Existing downstream cached copies cannot be recalled.
