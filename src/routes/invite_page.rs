use axum::extract::{Path, State};
use axum::http::{header, HeaderMap};
use axum::response::Html;

use crate::db;
use crate::error::AppError;
use crate::state::AppState;

use super::seo::{escape_html, is_crawler};

/// Extract the host from the Host header (or fallback to "localhost").
fn extract_host(headers: &HeaderMap) -> String {
    headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("localhost")
        .to_string()
}

/// GET /invite/{code}
///
/// Serves an HTML landing page for invite links shared via HTTP.
/// - For browsers: attempts a `daccord://` protocol redirect, with a fallback
///   landing page showing space info and download links.
/// - For crawlers: serves OG-tagged HTML with the space name and description.
pub async fn invite_page(
    State(state): State<AppState>,
    Path(code): Path<String>,
    headers: HeaderMap,
) -> Result<Html<String>, AppError> {
    let host = extract_host(&headers);
    // Validate invite code: alphanumeric only
    if !code.chars().all(|c| c.is_ascii_alphanumeric()) {
        return Err(AppError::NotFound("invalid invite code".to_string()));
    }

    // Look up the invite — 404 if expired/missing
    let invite = db::invites::get_invite(&state.db, &code).await?;
    let space = db::spaces::get_space_row(&state.db, &invite.space_id).await?;

    // Strip port from host for the daccord:// URI if it's the default
    let daccord_uri = format!(
        "daccord://invite/{}@{}",
        escape_html(&code),
        escape_html(&host)
    );
    let http_url = format!(
        "https://{}/invite/{}",
        escape_html(&host),
        escape_html(&code)
    );

    let space_name = escape_html(&space.name);
    let description = space
        .description
        .as_deref()
        .map(escape_html)
        .unwrap_or_else(|| format!("You've been invited to join {space_name}"));

    let icon_meta = space
        .icon
        .as_ref()
        .map(|icon| {
            format!(
                r#"    <meta property="og:image" content="https://{}/cdn/icons/{}">"#,
                escape_html(&host),
                icon
            )
        })
        .unwrap_or_default();

    if is_crawler(&headers) {
        // Minimal OG-tagged HTML for link previews
        let html = format!(
            r#"<!DOCTYPE html>
<html lang="en">
  <head>
    <meta charset="utf-8">
    <title>Join {space_name} on daccord</title>
    <meta property="og:title" content="Join {space_name}">
    <meta property="og:description" content="{description}">
    <meta property="og:type" content="website">
    <meta property="og:url" content="{http_url}">
    <meta property="og:site_name" content="daccord">
{icon_meta}
    <meta name="twitter:card" content="summary">
    <meta name="twitter:title" content="Join {space_name}">
    <meta name="twitter:description" content="{description}">
  </head>
  <body>
    <h1>Join {space_name}</h1>
    <p>{description}</p>
  </body>
</html>"#
        );
        return Ok(Html(html));
    }

    // For humans: try protocol redirect with JS, show fallback landing page
    let html = format!(
        r#"<!DOCTYPE html>
<html lang="en">
  <head>
    <meta charset="utf-8">
    <meta name="viewport" content="width=device-width, initial-scale=1">
    <title>Join {space_name} on daccord</title>
    <meta property="og:title" content="Join {space_name}">
    <meta property="og:description" content="{description}">
    <meta property="og:type" content="website">
    <meta property="og:url" content="{http_url}">
    <meta property="og:site_name" content="daccord">
{icon_meta}
    <meta name="twitter:card" content="summary">
    <meta name="twitter:title" content="Join {space_name}">
    <meta name="twitter:description" content="{description}">
    <style>
      * {{ margin: 0; padding: 0; box-sizing: border-box; }}
      body {{
        font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif;
        background: #1a1a2e; color: #e0e0e0;
        display: flex; align-items: center; justify-content: center;
        min-height: 100vh; padding: 20px;
      }}
      .card {{
        background: #25253e; border-radius: 12px; padding: 40px;
        max-width: 440px; width: 100%; text-align: center;
        box-shadow: 0 8px 32px rgba(0,0,0,0.3);
      }}
      h1 {{ font-size: 1.5rem; margin-bottom: 8px; color: #fff; }}
      .desc {{ color: #a0a0b8; margin-bottom: 24px; line-height: 1.5; }}
      .invite-code {{
        font-family: monospace; font-size: 1.1rem;
        background: #1a1a2e; padding: 8px 16px; border-radius: 6px;
        display: inline-block; margin-bottom: 24px; color: #7c7cf0;
        user-select: all;
      }}
      .btn {{
        display: inline-block; padding: 12px 32px; border-radius: 8px;
        font-size: 1rem; font-weight: 600; text-decoration: none;
        transition: opacity 0.2s;
      }}
      .btn:hover {{ opacity: 0.85; }}
      .btn-primary {{ background: #5b5bf0; color: #fff; }}
      .btn-secondary {{
        background: transparent; color: #7c7cf0;
        border: 1px solid #7c7cf0; margin-top: 12px;
      }}
      .actions {{ display: flex; flex-direction: column; align-items: center; gap: 4px; }}
      .status {{ font-size: 0.85rem; color: #a0a0b8; margin-top: 16px; }}
      .status a {{ color: #7c7cf0; }}
    </style>
  </head>
  <body>
    <div class="card">
      <h1>Join {space_name}</h1>
      <p class="desc">{description}</p>
      <div class="invite-code">{code_escaped}</div>
      <div class="actions">
        <a id="open-btn" class="btn btn-primary" href="{daccord_uri}">Open in daccord</a>
        <a class="btn btn-secondary" href="https://daccord.cc">Get daccord</a>
      </div>
      <p id="status" class="status"></p>
    </div>
    <script>
      // Attempt protocol handler redirect
      var uri = "{daccord_uri}";
      var opened = false;
      window.addEventListener("blur", function() {{ opened = true; }});
      setTimeout(function() {{
        window.location.href = uri;
      }}, 100);
      setTimeout(function() {{
        if (!opened) {{
          document.getElementById("status").innerHTML =
            'daccord not detected. <a href="https://daccord.cc">Download it</a> or copy the invite code above.';
        }}
      }}, 2000);
    </script>
  </body>
</html>"#,
        space_name = space_name,
        description = description,
        http_url = http_url,
        icon_meta = icon_meta,
        daccord_uri = daccord_uri,
        code_escaped = escape_html(&code),
    );

    Ok(Html(html))
}
