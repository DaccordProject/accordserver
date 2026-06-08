use axum::extract::State;
use axum::response::Html;
use axum::Json;

use crate::state::AppState;

use super::seo::escape_html;

/// GET /update-status
///
/// Returns the desktop tray app's auto-update status, read from the
/// `update_status.json` file it writes into the shared data directory. The
/// landing page polls this to show an update banner. Returns a neutral
/// `unknown` status for standalone (non-desktop) deployments.
pub async fn update_status(State(state): State<AppState>) -> Json<serde_json::Value> {
    let default = serde_json::json!({
        "phase": "unknown",
        "current_version": env!("CARGO_PKG_VERSION"),
        "new_version": null,
        "message": null,
    });

    let Some(path) = state.update_status_path.as_ref() else {
        return Json(default);
    };

    match tokio::fs::read(path).await {
        Ok(bytes) => match serde_json::from_slice::<serde_json::Value>(&bytes) {
            Ok(v) => Json(v),
            Err(_) => Json(default),
        },
        Err(_) => Json(default),
    }
}

async fn count(state: &AppState, table: &str) -> i64 {
    // Table names are hardcoded literals below, never user input.
    sqlx::query_scalar::<_, i64>(&format!("SELECT COUNT(*) FROM {table}"))
        .fetch_one(&state.db)
        .await
        .unwrap_or(0)
}

/// GET /
///
/// Human-facing landing page for the server root. Shows live status, basic
/// statistics, and links for connecting a client and finding documentation.
/// This replaces the bare 404 that used to greet anyone opening the server
/// URL in a browser (e.g. via the desktop tray "Open in browser" item).
pub async fn landing(State(state): State<AppState>) -> Html<String> {
    let settings = state.settings.load();
    let server_name = escape_html(&settings.server_name);
    let motd = settings
        .motd
        .as_deref()
        .filter(|m| !m.is_empty())
        .map(escape_html);

    let version = env!("CARGO_PKG_VERSION");
    let voice = if state.livekit_client.is_some() {
        "LiveKit (WebRTC)"
    } else {
        "Disabled"
    };

    let users = count(&state, "users").await;
    let spaces = count(&state, "spaces").await;
    let channels = count(&state, "channels").await;
    let messages = count(&state, "messages").await;

    let motd_block = motd
        .map(|m| format!(r#"      <p class="motd">{m}</p>"#))
        .unwrap_or_default();

    let html = format!(
        r#"<!DOCTYPE html>
<html lang="en">
  <head>
    <meta charset="utf-8">
    <meta name="viewport" content="width=device-width, initial-scale=1">
    <title>{server_name} — Accord server</title>
    <style>
      * {{ margin: 0; padding: 0; box-sizing: border-box; }}
      body {{
        font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif;
        background: #1a1a2e; color: #e0e0e0; line-height: 1.5;
        min-height: 100vh; padding: 32px 20px;
      }}
      .wrap {{ max-width: 760px; margin: 0 auto; }}
      header {{ display: flex; align-items: center; gap: 12px; margin-bottom: 4px; }}
      header h1 {{ font-size: 1.6rem; color: #fff; }}
      .badge {{
        font-size: 0.75rem; font-weight: 600; padding: 3px 10px; border-radius: 999px;
        background: rgba(67, 196, 127, 0.15); color: #43c47f;
        display: inline-flex; align-items: center; gap: 6px;
      }}
      .badge::before {{
        content: ""; width: 8px; height: 8px; border-radius: 50%;
        background: #43c47f; box-shadow: 0 0 8px #43c47f;
      }}
      .sub {{ color: #a0a0b8; margin-bottom: 24px; font-size: 0.95rem; }}
      .motd {{
        background: #25253e; border-left: 3px solid #5b5bf0; border-radius: 6px;
        padding: 12px 16px; margin-bottom: 24px; color: #cfcfe6;
      }}
      .grid {{ display: grid; grid-template-columns: repeat(auto-fit, minmax(140px, 1fr)); gap: 12px; margin-bottom: 28px; }}
      .stat {{ background: #25253e; border-radius: 10px; padding: 16px; }}
      .stat .num {{ font-size: 1.7rem; font-weight: 700; color: #fff; }}
      .stat .label {{ font-size: 0.8rem; color: #a0a0b8; text-transform: uppercase; letter-spacing: 0.04em; }}
      .card {{ background: #25253e; border-radius: 10px; padding: 20px 24px; margin-bottom: 20px; }}
      .card h2 {{ font-size: 1.05rem; color: #fff; margin-bottom: 12px; }}
      .row {{ display: flex; justify-content: space-between; padding: 6px 0; border-bottom: 1px solid #2f2f4a; font-size: 0.92rem; }}
      .row:last-child {{ border-bottom: none; }}
      .row .k {{ color: #a0a0b8; }}
      .row .v {{ color: #e0e0e0; font-family: monospace; }}
      ol {{ margin: 0 0 0 20px; color: #cfcfe6; }}
      ol li {{ margin-bottom: 6px; }}
      code {{ background: #1a1a2e; padding: 2px 6px; border-radius: 4px; color: #7c7cf0; font-size: 0.88rem; }}
      .links {{ display: flex; flex-wrap: wrap; gap: 10px; }}
      .links a {{
        text-decoration: none; padding: 9px 16px; border-radius: 8px; font-size: 0.9rem; font-weight: 600;
        background: transparent; color: #7c7cf0; border: 1px solid #7c7cf0;
      }}
      .links a.primary {{ background: #5b5bf0; color: #fff; border-color: #5b5bf0; }}
      footer {{ color: #6c6c85; font-size: 0.82rem; text-align: center; margin-top: 28px; }}
      .update-banner {{ border-radius: 8px; padding: 12px 16px; margin-bottom: 20px; font-size: 0.92rem; font-weight: 600; }}
      .update-banner.info {{ background: rgba(91,91,240,0.15); color: #b9b9ff; border: 1px solid #5b5bf0; }}
      .update-banner.ready {{ background: rgba(67,196,127,0.15); color: #8fe6b5; border: 1px solid #43c47f; }}
    </style>
  </head>
  <body>
    <div class="wrap">
      <div id="update-banner" class="update-banner" style="display:none"></div>
      <header>
        <h1>{server_name}</h1>
        <span class="badge">Online</span>
      </header>
      <p class="sub">Self-hosted Accord chat &amp; voice server &middot; v{version}</p>
{motd_block}
      <div class="grid">
        <div class="stat"><div class="num">{users}</div><div class="label">Users</div></div>
        <div class="stat"><div class="num">{spaces}</div><div class="label">Spaces</div></div>
        <div class="stat"><div class="num">{channels}</div><div class="label">Channels</div></div>
        <div class="stat"><div class="num">{messages}</div><div class="label">Messages</div></div>
      </div>

      <div class="card">
        <h2>Server</h2>
        <div class="row"><span class="k">Version</span><span class="v">{version}</span></div>
        <div class="row"><span class="k">Voice backend</span><span class="v">{voice}</span></div>
        <div class="row"><span class="k">REST API</span><span class="v">/api/v1</span></div>
        <div class="row"><span class="k">Gateway (WebSocket)</span><span class="v">/ws</span></div>
        <div class="row"><span class="k">Health check</span><span class="v">/health</span></div>
      </div>

      <div class="card">
        <h2>Connect a client</h2>
        <ol>
          <li>Install the daccord client from <a href="https://www.daccord.gg" style="color:#7c7cf0">daccord.gg</a>.</li>
          <li>Add this server using its address (the URL in your browser bar).</li>
          <li>Register an account, or use an invite link if you have one.</li>
          <li>Bots connect to the gateway at <code>/ws</code> with a <code>Bot &lt;token&gt;</code> identify.</li>
        </ol>
      </div>

      <div class="card">
        <h2>Manage &amp; configure</h2>
        <p style="color:#a0a0b8; margin-bottom:12px; font-size:0.92rem;">
          Server settings, members, roles and bans are managed through an admin
          account in the daccord client. Use the desktop tray menu to open the
          data folder, view logs, or toggle start-on-login.
        </p>
        <div class="links">
          <a class="primary" href="https://www.daccord.gg">Get the client</a>
          <a href="https://www.daccord.gg/docs.html#deploying-a-server">Documentation</a>
        </div>
      </div>

      <footer>Accord &middot; powered by Axum &amp; LiveKit</footer>
    </div>
    <script>
      function renderUpdate(s) {{
        var el = document.getElementById('update-banner');
        if (!s || !s.phase) {{ el.style.display = 'none'; return; }}
        var msg = null, cls = 'info';
        if (s.phase === 'available' || s.phase === 'downloading') {{
          msg = 'Downloading update' + (s.new_version ? ' v' + s.new_version : '') + '…';
        }} else if (s.phase === 'ready') {{
          msg = 'Update' + (s.new_version ? ' v' + s.new_version : '') + ' ready — restart Accord to apply.';
          cls = 'ready';
        }}
        if (!msg) {{ el.style.display = 'none'; return; }}
        el.textContent = msg;
        el.className = 'update-banner ' + cls;
        el.style.display = 'block';
      }}
      function pollUpdate() {{
        fetch('/update-status').then(function (r) {{ return r.json(); }}).then(renderUpdate).catch(function () {{}});
      }}
      pollUpdate();
      setInterval(pollUpdate, 30000);
    </script>
  </body>
</html>"#
    );

    Html(html)
}
