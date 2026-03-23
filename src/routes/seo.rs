use axum::extract::{Path, Query, State};
use axum::http::header;
use axum::http::HeaderMap;
use axum::response::Html;

use crate::db;
use crate::error::AppError;
use crate::state::AppState;

const REPLIES_PER_PAGE: i64 = 25;

/// Known crawler user-agent substrings.
const CRAWLER_AGENTS: &[&str] = &[
    "Googlebot",
    "Bingbot",
    "bingbot",
    "Slurp",
    "DuckDuckBot",
    "Baiduspider",
    "YandexBot",
    "facebookexternalhit",
    "Twitterbot",
    "LinkedInBot",
    "WhatsApp",
    "TelegramBot",
    "Discordbot",
    "Slackbot",
    "Applebot",
];

/// Returns true if the User-Agent header matches a known web crawler.
pub fn is_crawler(headers: &HeaderMap) -> bool {
    let ua = match headers.get(header::USER_AGENT) {
        Some(v) => match v.to_str() {
            Ok(s) => s,
            Err(_) => return false,
        },
        None => return false,
    };
    CRAWLER_AGENTS.iter().any(|bot| ua.contains(bot))
}

/// Escape HTML special characters.
pub fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Truncate content to a maximum length, appending "..." if truncated.
fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max).collect();
        format!("{truncated}...")
    }
}

/// Extract the host from the Host header (or fallback to "localhost").
fn extract_host(headers: &HeaderMap) -> String {
    headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("localhost")
        .to_string()
}

/// Builds an HTML landing page for human visitors that attempts a `daccord://`
/// protocol redirect, then falls back to the web client hash URL.
///
/// - `daccord_uri`: the `daccord://connect/...` URI to attempt
/// - `web_fragment`: the `#slug/channel` web client path to fall back to
/// - `title`: display title for the page
/// - `description`: description text
/// - `icon_url`: optional OG image URL
fn build_redirect_page(
    daccord_uri: &str,
    web_fragment: &str,
    http_url: &str,
    title: &str,
    description: &str,
    icon_url: Option<&str>,
) -> String {
    let icon_meta = icon_url
        .map(|url| format!(r#"    <meta property="og:image" content="{url}">"#))
        .unwrap_or_default();

    format!(
        r#"<!DOCTYPE html>
<html lang="en">
  <head>
    <meta charset="utf-8">
    <meta name="viewport" content="width=device-width, initial-scale=1">
    <title>{title} — daccord</title>
    <meta property="og:title" content="{title}">
    <meta property="og:description" content="{description}">
    <meta property="og:type" content="website">
    <meta property="og:url" content="{http_url}">
    <meta property="og:site_name" content="daccord">
{icon_meta}
    <meta name="twitter:card" content="summary">
    <meta name="twitter:title" content="{title}">
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
      .btn-tertiary {{
        background: transparent; color: #a0a0b8;
        margin-top: 8px; font-size: 0.9rem;
      }}
      .actions {{ display: flex; flex-direction: column; align-items: center; gap: 4px; }}
      .status {{ font-size: 0.85rem; color: #a0a0b8; margin-top: 16px; }}
      .status a {{ color: #7c7cf0; }}
    </style>
  </head>
  <body>
    <div class="card">
      <h1>{title}</h1>
      <p class="desc">{description}</p>
      <div class="actions">
        <a id="open-btn" class="btn btn-primary" href="{daccord_uri}">Open in daccord</a>
        <a class="btn btn-secondary" href="/{web_fragment}">Open in browser</a>
        <a class="btn btn-tertiary" href="https://daccord.cc">Get daccord</a>
      </div>
      <p id="status" class="status"></p>
    </div>
    <script>
      var uri = "{daccord_uri}";
      var opened = false;
      window.addEventListener("blur", function() {{ opened = true; }});
      setTimeout(function() {{ window.location.href = uri; }}, 100);
      setTimeout(function() {{
        if (!opened) {{
          document.getElementById("status").innerHTML =
            'daccord not detected &mdash; <a href="/{web_fragment}">continue in browser</a>';
        }}
      }}, 2000);
    </script>
  </body>
</html>"#,
        title = title,
        description = description,
        http_url = http_url,
        icon_meta = icon_meta,
        daccord_uri = daccord_uri,
        web_fragment = web_fragment,
    )
}

#[derive(serde::Deserialize)]
pub struct PageQuery {
    pub page: Option<i64>,
}

// -------------------------------------------------------------------------
// GET /s/{space_slug}/{channel_name}
// -------------------------------------------------------------------------

/// Serves an HTML snapshot of a channel's recent messages for crawlers,
/// or redirects human visitors to the web client.
pub async fn channel_snapshot(
    State(state): State<AppState>,
    Path((space_slug, channel_name)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Html<String>, AppError> {
    let space = db::spaces::get_space_by_slug(&state.db, &space_slug).await?;

    // Human visitors always get the redirect landing page (public or not).
    // The daccord client handles registration/invite prompts as needed.
    if !is_crawler(&headers) {
        let host = extract_host(&headers);
        let daccord_uri = format!(
            "daccord://connect/{}/{}",
            escape_html(&host),
            escape_html(&space_slug),
        );
        let web_fragment = format!(
            "#{}/{}",
            escape_html(&space_slug),
            escape_html(&channel_name),
        );
        let http_url = format!(
            "https://{}/s/{}/{}",
            escape_html(&host),
            escape_html(&space_slug),
            escape_html(&channel_name),
        );
        let title = format!("{} — {}", escape_html(&channel_name), escape_html(&space.name));
        let desc = space
            .description
            .as_deref()
            .map(|d| escape_html(&truncate(d, 200)))
            .unwrap_or_else(|| escape_html(&space.name));
        let icon_url = space.icon.as_ref().map(|icon| {
            format!("https://{}/cdn/icons/{}", escape_html(&host), icon)
        });
        let html = build_redirect_page(
            &daccord_uri,
            &web_fragment,
            &http_url,
            &title,
            &desc,
            icon_url.as_deref(),
        );
        return Ok(Html(html));
    }

    // Crawlers only get snapshots of public spaces.
    if !space.public {
        return Err(AppError::NotFound("not_found".to_string()));
    }

    let channels = db::channels::list_channels_in_space(&state.db, &space.id).await?;
    let channel = channels
        .iter()
        .find(|c| c.name.as_deref() == Some(&channel_name))
        .ok_or_else(|| AppError::NotFound("unknown_channel".to_string()))?;

    // Fetch recent messages (newest first, excluding thread replies).
    let messages = db::messages::list_messages(&state.db, &channel.id, None, 50, None).await?;

    // Collect unique author IDs and fetch display names.
    let author_ids: Vec<String> = messages
        .iter()
        .map(|m| m.author_id.clone())
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();
    let mut authors = std::collections::HashMap::new();
    for id in &author_ids {
        if let Ok(user) = db::users::get_user(&state.db, id).await {
            authors.insert(id.clone(), user.display_name.unwrap_or(user.username));
        }
    }

    let canonical = format!(
        "/s/{}/{}",
        escape_html(&space_slug),
        escape_html(&channel_name)
    );
    let fragment_url = format!(
        "#{}/{}",
        escape_html(&space_slug),
        escape_html(&channel_name)
    );
    let title = format!(
        "{} — {}",
        escape_html(channel.name.as_deref().unwrap_or("channel")),
        escape_html(&space.name),
    );
    let description = channel
        .topic
        .as_deref()
        .map(|t| escape_html(&truncate(t, 200)))
        .unwrap_or_default();

    let icon_meta = space
        .icon
        .as_ref()
        .map(|icon| format!(r#"    <meta property="og:image" content="/cdn/icons/{icon}">"#,))
        .unwrap_or_default();

    // Render messages as semantic HTML.
    let mut message_html = String::new();
    // Messages come newest-first for main feed; reverse for chronological display.
    for msg in messages.iter().rev() {
        let author_name = authors
            .get(&msg.author_id)
            .cloned()
            .unwrap_or_else(|| "Unknown".to_string());
        message_html.push_str(&format!(
            r#"      <article class="message">
        <header><strong>{}</strong> <time datetime="{}">{}</time></header>
        <p>{}</p>
      </article>
"#,
            escape_html(&author_name),
            escape_html(&msg.created_at),
            escape_html(&msg.created_at),
            escape_html(&msg.content),
        ));
    }

    let html = format!(
        r#"<!DOCTYPE html>
<html lang="en">
  <head>
    <meta charset="utf-8">
    <title>{title}</title>
    <meta property="og:title" content="{title}">
    <meta property="og:description" content="{description}">
    <meta property="og:type" content="website">
    <meta property="og:url" content="{canonical}">
    <meta property="og:site_name" content="{space_name}">
{icon_meta}
    <meta name="twitter:card" content="summary">
    <meta name="twitter:title" content="{title}">
    <meta name="twitter:description" content="{description}">
    <link rel="canonical" href="{canonical}">
  </head>
  <body>
    <nav><a href="/s/{space_slug_escaped}">{space_name}</a> &rsaquo; {channel_display}</nav>
    <main>
{message_html}
    </main>
    <footer>
      <p><a href="/{fragment_url}">Open in daccord</a></p>
    </footer>
  </body>
</html>"#,
        title = title,
        description = description,
        canonical = canonical,
        space_name = escape_html(&space.name),
        icon_meta = icon_meta,
        space_slug_escaped = escape_html(&space_slug),
        channel_display = escape_html(channel.name.as_deref().unwrap_or("channel")),
        message_html = message_html,
        fragment_url = fragment_url,
    );

    Ok(Html(html))
}

// -------------------------------------------------------------------------
// GET /s/{space_slug}/{channel_name}/{post_id}
// -------------------------------------------------------------------------

/// Serves an HTML snapshot of a forum post and its thread replies for crawlers,
/// with `rel="next"` pagination. Human visitors are redirected to the web client.
pub async fn post_snapshot(
    State(state): State<AppState>,
    Path((space_slug, channel_name, post_id)): Path<(String, String, String)>,
    Query(query): Query<PageQuery>,
    headers: HeaderMap,
) -> Result<Html<String>, AppError> {
    let space = db::spaces::get_space_by_slug(&state.db, &space_slug).await?;

    // Human visitors always get the redirect landing page (public or not).
    if !is_crawler(&headers) {
        let host = extract_host(&headers);
        let daccord_uri = format!(
            "daccord://connect/{}/{}",
            escape_html(&host),
            escape_html(&space_slug),
        );
        let web_fragment = format!(
            "#{}/{}/{}",
            escape_html(&space_slug),
            escape_html(&channel_name),
            escape_html(&post_id),
        );
        let http_url = format!(
            "https://{}/s/{}/{}/{}",
            escape_html(&host),
            escape_html(&space_slug),
            escape_html(&channel_name),
            escape_html(&post_id),
        );
        let title = format!("Post in {} — {}", escape_html(&channel_name), escape_html(&space.name));
        let desc = escape_html(&space.name);
        let icon_url = space.icon.as_ref().map(|icon| {
            format!("https://{}/cdn/icons/{}", escape_html(&host), icon)
        });
        let html = build_redirect_page(
            &daccord_uri,
            &web_fragment,
            &http_url,
            &title,
            &desc,
            icon_url.as_deref(),
        );
        return Ok(Html(html));
    }

    // Crawlers only get snapshots of public spaces.
    if !space.public {
        return Err(AppError::NotFound("not_found".to_string()));
    }

    let channels = db::channels::list_channels_in_space(&state.db, &space.id).await?;
    let channel = channels
        .iter()
        .find(|c| c.name.as_deref() == Some(&channel_name))
        .ok_or_else(|| AppError::NotFound("unknown_channel".to_string()))?;

    // Fetch the post (parent message).
    let post = db::messages::get_message_row(&state.db, &post_id).await?;
    if post.channel_id != channel.id {
        return Err(AppError::NotFound("message_not_in_channel".to_string()));
    }

    let post_author = db::users::get_user(&state.db, &post.author_id).await.ok();
    let post_author_name = post_author
        .as_ref()
        .and_then(|u| u.display_name.clone())
        .or_else(|| post_author.as_ref().map(|u| u.username.clone()))
        .unwrap_or_else(|| "Unknown".to_string());

    // Fetch thread replies with pagination.
    let page = query.page.unwrap_or(1).max(1);
    let offset_cursor = if page > 1 {
        // Skip (page-1)*REPLIES_PER_PAGE replies by fetching them and using
        // the last ID as the cursor.
        let skip_count = (page - 1) * REPLIES_PER_PAGE;
        let skipped =
            db::messages::list_messages(&state.db, &channel.id, None, skip_count, Some(&post_id))
                .await?;
        skipped.last().map(|m| m.id.clone())
    } else {
        None
    };

    let replies = db::messages::list_messages(
        &state.db,
        &channel.id,
        offset_cursor.as_deref(),
        REPLIES_PER_PAGE,
        Some(&post_id),
    )
    .await?;

    let total_replies = db::messages::get_thread_reply_count(&state.db, &post_id).await?;
    let total_pages = ((total_replies as f64) / (REPLIES_PER_PAGE as f64)).ceil() as i64;
    let has_next = page < total_pages;

    // Fetch reply author display names.
    let reply_author_ids: Vec<String> = replies
        .iter()
        .map(|m| m.author_id.clone())
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();
    let mut authors = std::collections::HashMap::new();
    for id in &reply_author_ids {
        if let Ok(user) = db::users::get_user(&state.db, id).await {
            authors.insert(id.clone(), user.display_name.unwrap_or(user.username));
        }
    }

    let canonical = format!(
        "/s/{}/{}/{}",
        escape_html(&space_slug),
        escape_html(&channel_name),
        escape_html(&post_id),
    );
    let fragment_url = format!(
        "#{}/{}/{}",
        escape_html(&space_slug),
        escape_html(&channel_name),
        escape_html(&post_id),
    );

    // Use first line of post content as title, rest as description.
    let post_title = post.content.lines().next().unwrap_or("Post").to_string();
    let title = format!(
        "{} — {} Forum",
        escape_html(&truncate(&post_title, 70)),
        escape_html(&space.name),
    );
    let description = escape_html(&truncate(&post.content, 200));

    let icon_meta = space
        .icon
        .as_ref()
        .map(|icon| format!(r#"    <meta property="og:image" content="/cdn/icons/{icon}">"#,))
        .unwrap_or_default();

    // Pagination link tags.
    let mut pagination_links = String::new();
    if page > 1 {
        if page == 2 {
            pagination_links.push_str(&format!(r#"    <link rel="prev" href="{canonical}">"#));
        } else {
            pagination_links.push_str(&format!(
                r#"    <link rel="prev" href="{canonical}?page={}">"#,
                page - 1,
            ));
        }
        pagination_links.push('\n');
    }
    if has_next {
        pagination_links.push_str(&format!(
            r#"    <link rel="next" href="{canonical}?page={}">"#,
            page + 1,
        ));
        pagination_links.push('\n');
    }

    // Render replies as semantic HTML.
    let mut replies_html = String::new();
    for reply in &replies {
        let author_name = authors
            .get(&reply.author_id)
            .cloned()
            .unwrap_or_else(|| "Unknown".to_string());
        replies_html.push_str(&format!(
            r#"        <article class="reply">
          <header><strong>{}</strong> <time datetime="{}">{}</time></header>
          <p>{}</p>
        </article>
"#,
            escape_html(&author_name),
            escape_html(&reply.created_at),
            escape_html(&reply.created_at),
            escape_html(&reply.content),
        ));
    }

    // Pagination nav.
    let mut pagination_nav = String::new();
    if total_pages > 1 {
        pagination_nav.push_str(r#"      <nav class="pagination">"#);
        if page > 1 {
            let prev_href = if page == 2 {
                canonical.clone()
            } else {
                format!("{canonical}?page={}", page - 1)
            };
            pagination_nav.push_str(&format!(
                r#" <a rel="prev" href="{prev_href}">&laquo; Previous</a>"#
            ));
        }
        pagination_nav.push_str(&format!(" Page {page} of {total_pages}"));
        if has_next {
            pagination_nav.push_str(&format!(
                r#" <a rel="next" href="{canonical}?page={}">Next &raquo;</a>"#,
                page + 1,
            ));
        }
        pagination_nav.push_str("</nav>\n");
    }

    let html = format!(
        r#"<!DOCTYPE html>
<html lang="en">
  <head>
    <meta charset="utf-8">
    <title>{title}</title>
    <meta property="og:title" content="{title}">
    <meta property="og:description" content="{description}">
    <meta property="og:type" content="article">
    <meta property="og:url" content="{canonical}">
    <meta property="og:site_name" content="{space_name}">
{icon_meta}
    <meta name="twitter:card" content="summary_large_image">
    <meta name="twitter:title" content="{title}">
    <meta name="twitter:description" content="{description}">
    <link rel="canonical" href="{canonical}">
{pagination_links}  </head>
  <body>
    <nav><a href="/s/{space_slug_escaped}">{space_name}</a> &rsaquo; <a href="/s/{space_slug_escaped}/{channel_name_escaped}">{channel_display}</a></nav>
    <main>
      <article class="post">
        <header>
          <h1>{post_title_escaped}</h1>
          <p>By <strong>{post_author_name}</strong> <time datetime="{post_time}">{post_time}</time></p>
        </header>
        <p>{post_content}</p>
      </article>
      <section class="replies">
        <h2>Replies ({total_replies})</h2>
{replies_html}{pagination_nav}      </section>
    </main>
    <footer>
      <p><a href="/{fragment_url}">Open in daccord</a></p>
    </footer>
  </body>
</html>"#,
        title = title,
        description = description,
        canonical = canonical,
        space_name = escape_html(&space.name),
        icon_meta = icon_meta,
        pagination_links = pagination_links,
        space_slug_escaped = escape_html(&space_slug),
        channel_name_escaped = escape_html(&channel_name),
        channel_display = escape_html(channel.name.as_deref().unwrap_or("channel")),
        post_title_escaped = escape_html(&post_title),
        post_author_name = escape_html(&post_author_name),
        post_time = escape_html(&post.created_at),
        post_content = escape_html(&post.content),
        replies_html = replies_html,
        pagination_nav = pagination_nav,
        fragment_url = fragment_url,
        total_replies = total_replies,
    );

    Ok(Html(html))
}

// -------------------------------------------------------------------------
// GET /s/{space_slug}
// -------------------------------------------------------------------------

/// Lists public channels in a space for crawlers. Redirects humans to the
/// web client.
pub async fn space_snapshot(
    State(state): State<AppState>,
    Path(space_slug): Path<String>,
    headers: HeaderMap,
) -> Result<Html<String>, AppError> {
    let space = db::spaces::get_space_by_slug(&state.db, &space_slug).await?;

    // Human visitors always get the redirect landing page (public or not).
    if !is_crawler(&headers) {
        let host = extract_host(&headers);
        let daccord_uri = format!(
            "daccord://connect/{}/{}",
            escape_html(&host),
            escape_html(&space_slug),
        );
        let web_fragment = format!("#{}", escape_html(&space_slug));
        let http_url = format!(
            "https://{}/s/{}",
            escape_html(&host),
            escape_html(&space_slug),
        );
        let title = escape_html(&space.name);
        let desc = space
            .description
            .as_deref()
            .map(|d| escape_html(&truncate(d, 200)))
            .unwrap_or_else(|| escape_html(&space.name));
        let icon_url = space.icon.as_ref().map(|icon| {
            format!("https://{}/cdn/icons/{}", escape_html(&host), icon)
        });
        let html = build_redirect_page(
            &daccord_uri,
            &web_fragment,
            &http_url,
            &title,
            &desc,
            icon_url.as_deref(),
        );
        return Ok(Html(html));
    }

    // Crawlers only get snapshots of public spaces.
    if !space.public {
        return Err(AppError::NotFound("not_found".to_string()));
    }

    let channels = db::channels::list_channels_in_space(&state.db, &space.id).await?;

    let title = escape_html(&space.name);
    let description = space
        .description
        .as_deref()
        .map(|d| escape_html(&truncate(d, 200)))
        .unwrap_or_default();

    let icon_meta = space
        .icon
        .as_ref()
        .map(|icon| format!(r#"    <meta property="og:image" content="/cdn/icons/{icon}">"#,))
        .unwrap_or_default();

    let canonical = format!("/s/{}", escape_html(&space_slug));

    // Render channel list (skip categories and DM types).
    let mut channel_list_html = String::new();
    for ch in &channels {
        let ch_type = ch.channel_type.as_str();
        if ch_type == "category" || ch_type == "dm" || ch_type == "group_dm" {
            continue;
        }
        if let Some(name) = &ch.name {
            channel_list_html.push_str(&format!(
                r#"        <li><a href="/s/{}/{}">{}</a></li>
"#,
                escape_html(&space_slug),
                escape_html(name),
                escape_html(name),
            ));
        }
    }

    let html = format!(
        r#"<!DOCTYPE html>
<html lang="en">
  <head>
    <meta charset="utf-8">
    <title>{title}</title>
    <meta property="og:title" content="{title}">
    <meta property="og:description" content="{description}">
    <meta property="og:type" content="website">
    <meta property="og:url" content="{canonical}">
    <meta property="og:site_name" content="{title}">
{icon_meta}
    <meta name="twitter:card" content="summary">
    <meta name="twitter:title" content="{title}">
    <meta name="twitter:description" content="{description}">
    <link rel="canonical" href="{canonical}">
  </head>
  <body>
    <main>
      <h1>{title}</h1>
      <p>{description}</p>
      <nav>
        <h2>Channels</h2>
        <ul>
{channel_list_html}        </ul>
      </nav>
    </main>
    <footer>
      <p><a href="/#{space_slug_escaped}">Open in daccord</a></p>
    </footer>
  </body>
</html>"#,
        title = title,
        description = description,
        canonical = canonical,
        icon_meta = icon_meta,
        channel_list_html = channel_list_html,
        space_slug_escaped = escape_html(&space_slug),
    );

    Ok(Html(html))
}
