use axum::extract::{Path, Query, State};
use axum::http::header;
use axum::http::HeaderMap;
use axum::response::{Html, IntoResponse};

use crate::db;
use crate::error::AppError;
use crate::models::message::MessageRow;
use crate::models::space::SpaceRow;
use crate::state::AppState;

const REPLIES_PER_PAGE: i64 = 25;

/// Resolve a stored CDN reference (icon/banner/splash) to an absolute URL.
/// Stored values may be a full URL, a root-relative "/cdn/..." path, or a
/// bare filename (legacy uploads); all three are normalised against `base`.
fn cdn_url(base: &str, category: &str, stored: &str) -> String {
    if stored.starts_with("http://") || stored.starts_with("https://") {
        stored.to_string()
    } else if stored.starts_with('/') {
        format!("{base}{stored}")
    } else {
        format!("{base}/cdn/{category}/{stored}")
    }
}

/// Pick the best social-card image for a space, falling back
/// banner → splash → icon. Returns an absolute URL.
fn space_social_image(base: &str, space: &SpaceRow) -> Option<String> {
    let pick = |stored: &Option<String>, cat: &str| {
        stored
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| cdn_url(base, cat, s))
    };
    pick(&space.banner, "banners")
        .or_else(|| pick(&space.splash, "splashes"))
        .or_else(|| pick(&space.icon, "icons"))
}

/// Build `og:image` + `twitter:image` meta tags for an optional absolute URL.
/// Returns an empty string (no tags) when there is no image.
fn image_meta(image: Option<&str>) -> String {
    match image {
        Some(url) => {
            let u = escape_html(url);
            format!(
                "    <meta property=\"og:image\" content=\"{u}\">\n    <meta name=\"twitter:image\" content=\"{u}\">",
            )
        }
        None => String::new(),
    }
}

/// Pick the Twitter card style: a large image card when an image is present,
/// otherwise the compact summary card.
fn twitter_card(image: Option<&str>) -> &'static str {
    if image.is_some() {
        "summary_large_image"
    } else {
        "summary"
    }
}

/// Build the `<link rel="alternate" ... +oembed>` discovery tag pointing at
/// this server's oEmbed endpoint for the given (absolute) page URL.
fn oembed_link(base: &str, page_url: &str) -> String {
    format!(
        r#"    <link rel="alternate" type="application/json+oembed" href="{base}/oembed?url={enc}&amp;format=json" title="daccord oEmbed">
"#,
        base = base,
        enc = url_seg(page_url),
    )
}

/// Channel types that are never exposed as public crawlable pages.
fn is_hidden_channel_type(t: &str) -> bool {
    matches!(t, "category" | "dm" | "group_dm" | "voice")
}

/// Percent-encode a single URL path segment (RFC 3986 unreserved set kept).
fn url_seg(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Decode a percent-encoded URL path segment (inverse of [`url_seg`]).
fn url_unseg(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = (bytes[i + 1] as char).to_digit(16);
            let lo = (bytes[i + 2] as char).to_digit(16);
            if let (Some(h), Some(l)) = (hi, lo) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Escape text for embedding in XML (sitemap).
fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('\'', "&apos;")
        .replace('"', "&quot;")
}

/// Take the date portion (YYYY-MM-DD) of a timestamp for `<lastmod>`.
fn lastmod_date(s: &str) -> String {
    s.chars().take(10).collect()
}

/// Convert a "YYYY-MM-DD HH:MM:SS" timestamp to ISO 8601 ("...THH:MM:SS")
/// as expected by Schema.org date fields.
fn iso_datetime(s: &str) -> String {
    s.replacen(' ', "T", 1)
}

/// Resolve a forum post's display title: prefer the dedicated `title` field,
/// fall back to the first non-empty line of content, then a placeholder.
/// Mirrors the client's `resolveForumPostTitle`.
fn resolve_post_title(post: &MessageRow) -> String {
    if let Some(t) = post.title.as_deref() {
        let t = t.trim();
        if !t.is_empty() {
            return t.to_string();
        }
    }
    let first = post
        .content
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    if !first.is_empty() {
        return first.to_string();
    }
    "Untitled post".to_string()
}

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

/// Normalise text for a meta/description attribute: collapse every run of
/// whitespace (including newlines) to a single space, truncate, then escape.
fn meta_text(s: &str, max: usize) -> String {
    let collapsed = s.split_whitespace().collect::<Vec<_>>().join(" ");
    escape_html(&truncate(&collapsed, max))
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
    let icon_meta = image_meta(icon_url);
    let card = twitter_card(icon_url);

    format!(
        r#"<!DOCTYPE html>
<html lang="en">
  <head>
    <meta charset="utf-8">
    <meta name="viewport" content="width=device-width, initial-scale=1">
    <title>{title} — daccord</title>
    <meta name="description" content="{description}">
    <meta property="og:title" content="{title}">
    <meta property="og:description" content="{description}">
    <meta property="og:type" content="website">
    <meta property="og:url" content="{http_url}">
    <meta property="og:site_name" content="daccord">
{icon_meta}
    <meta name="twitter:card" content="{card}">
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
        <a class="btn btn-tertiary" href="https://www.daccord.gg">Get daccord</a>
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
        let title = format!(
            "{} — {}",
            escape_html(&channel_name),
            escape_html(&space.name)
        );
        let desc = space
            .description
            .as_deref()
            .map(|d| meta_text(d, 200))
            .unwrap_or_else(|| escape_html(&space.name));
        let icon_url = space_social_image(&format!("https://{host}"), &space);
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

    let host = extract_host(&headers);
    let base = format!("https://{host}");
    let space_seg = url_seg(&space_slug);
    let chan_seg = url_seg(&channel_name);
    let is_forum = channel.channel_type == "forum";
    let canonical = format!("{base}/s/{space_seg}/{chan_seg}");
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
        .map(|t| meta_text(t, 200))
        .unwrap_or_else(|| {
            // Fall back to the space description so the card is never empty.
            space
                .description
                .as_deref()
                .map(|d| meta_text(d, 200))
                .unwrap_or_else(|| escape_html(&space.name))
        });

    let social_image = space_social_image(&base, &space);
    let icon_meta = image_meta(social_image.as_deref());
    let card = twitter_card(social_image.as_deref());
    let oembed_disc = oembed_link(&base, &canonical);

    // BreadcrumbList structured data: Space › Channel.
    let breadcrumb_ld = {
        let doc = serde_json::json!({
            "@context": "https://schema.org",
            "@type": "BreadcrumbList",
            "itemListElement": [
                {
                    "@type": "ListItem", "position": 1,
                    "name": space.name,
                    "item": format!("{base}/s/{space_seg}"),
                },
                {
                    "@type": "ListItem", "position": 2,
                    "name": channel.name.clone().unwrap_or_else(|| "channel".to_string()),
                    "item": canonical,
                },
            ],
        });
        let serialized = doc.to_string().replace("</", "<\\/");
        format!("    <script type=\"application/ld+json\">{serialized}</script>\n")
    };

    // Render the channel body. Forum channels list their top-level posts as
    // links to each post's dedicated page so crawlers follow through to them;
    // other channels render a chronological message snapshot.
    let mut body_html = String::new();
    if is_forum {
        for post in &messages {
            let author_name = authors
                .get(&post.author_id)
                .cloned()
                .unwrap_or_else(|| "Unknown".to_string());
            let post_title = resolve_post_title(post);
            let excerpt = truncate(&post.content, 200);
            body_html.push_str(&format!(
                r#"      <article class="post-summary">
        <h2><a href="{base}/s/{space_seg}/{chan_seg}/{post_seg}">{post_title}</a></h2>
        <p>By <strong>{author}</strong> <time datetime="{time}">{time}</time></p>
        <p>{excerpt}</p>
      </article>
"#,
                base = base,
                space_seg = space_seg,
                chan_seg = chan_seg,
                post_seg = url_seg(&post.id),
                post_title = escape_html(&post_title),
                author = escape_html(&author_name),
                time = escape_html(&post.created_at),
                excerpt = escape_html(&excerpt),
            ));
        }
    } else {
        // Messages come newest-first for main feed; reverse for chronological display.
        for msg in messages.iter().rev() {
            let author_name = authors
                .get(&msg.author_id)
                .cloned()
                .unwrap_or_else(|| "Unknown".to_string());
            body_html.push_str(&format!(
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
    }

    let html = format!(
        r#"<!DOCTYPE html>
<html lang="en">
  <head>
    <meta charset="utf-8">
    <title>{title}</title>
    <meta name="description" content="{description}">
    <meta property="og:title" content="{title}">
    <meta property="og:description" content="{description}">
    <meta property="og:type" content="website">
    <meta property="og:url" content="{canonical}">
    <meta property="og:site_name" content="{space_name}">
{icon_meta}
    <meta name="twitter:card" content="{card}">
    <meta name="twitter:title" content="{title}">
    <meta name="twitter:description" content="{description}">
    <link rel="canonical" href="{canonical}">
{oembed_disc}{breadcrumb_ld}  </head>
  <body>
    <nav><a href="{base}/s/{space_seg}">{space_name}</a> &rsaquo; {channel_display}</nav>
    <main>
{body_html}    </main>
    <footer>
      <p><a href="/{fragment_url}">Open in daccord</a></p>
    </footer>
  </body>
</html>"#,
        title = title,
        description = description,
        canonical = canonical,
        base = base,
        space_seg = space_seg,
        space_name = escape_html(&space.name),
        icon_meta = icon_meta,
        card = card,
        oembed_disc = oembed_disc,
        breadcrumb_ld = breadcrumb_ld,
        channel_display = escape_html(channel.name.as_deref().unwrap_or("channel")),
        body_html = body_html,
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
        let title = format!(
            "Post in {} — {}",
            escape_html(&channel_name),
            escape_html(&space.name)
        );
        let desc = escape_html(&space.name);
        let icon_url = space_social_image(&format!("https://{host}"), &space);
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

    let host = extract_host(&headers);
    let base = format!("https://{host}");
    let space_seg = url_seg(&space_slug);
    let chan_seg = url_seg(&channel_name);
    let post_seg = url_seg(&post_id);
    let canonical = format!("{base}/s/{space_seg}/{chan_seg}/{post_seg}");
    let fragment_url = format!(
        "#{}/{}/{}",
        escape_html(&space_slug),
        escape_html(&channel_name),
        escape_html(&post_id),
    );

    // Prefer the forum post's dedicated title; fall back to first content line.
    let post_title = resolve_post_title(&post);
    let title = format!(
        "{} — {} Forum",
        escape_html(&truncate(&post_title, 70)),
        escape_html(&space.name),
    );
    let description = meta_text(&post.content, 200);

    let social_image = space_social_image(&base, &space);
    let icon_meta = image_meta(social_image.as_deref());
    let card = twitter_card(social_image.as_deref());
    let oembed_disc = oembed_link(&base, &canonical);

    // Article timing + author meta for the social card / news indexers.
    let article_meta = {
        let mut m = format!(
            "    <meta property=\"article:published_time\" content=\"{}\">\n",
            escape_html(&iso_datetime(&post.created_at)),
        );
        if let Some(edited) = post.edited_at.as_deref() {
            m.push_str(&format!(
                "    <meta property=\"article:modified_time\" content=\"{}\">\n",
                escape_html(&iso_datetime(edited)),
            ));
        }
        m.push_str(&format!(
            "    <meta property=\"article:author\" content=\"{}\">\n",
            escape_html(&post_author_name),
        ));
        m
    };

    // Structured data for rich results (Schema.org DiscussionForumPosting).
    let json_ld = {
        let doc = serde_json::json!({
            "@context": "https://schema.org",
            "@type": "DiscussionForumPosting",
            "headline": post_title,
            "url": canonical,
            "datePublished": iso_datetime(&post.created_at),
            "author": { "@type": "Person", "name": post_author_name },
            "articleBody": post.content,
            "interactionStatistic": {
                "@type": "InteractionCounter",
                "interactionType": "https://schema.org/CommentAction",
                "userInteractionCount": total_replies,
            },
        });
        // Escape `</` so post content can't prematurely close the <script>.
        let serialized = doc.to_string().replace("</", "<\\/");
        format!("    <script type=\"application/ld+json\">{serialized}</script>\n")
    };

    // BreadcrumbList structured data: Space › Channel › Post.
    let breadcrumb_ld = {
        let doc = serde_json::json!({
            "@context": "https://schema.org",
            "@type": "BreadcrumbList",
            "itemListElement": [
                {
                    "@type": "ListItem", "position": 1,
                    "name": space.name,
                    "item": format!("{base}/s/{space_seg}"),
                },
                {
                    "@type": "ListItem", "position": 2,
                    "name": channel.name.clone().unwrap_or_else(|| "channel".to_string()),
                    "item": format!("{base}/s/{space_seg}/{chan_seg}"),
                },
                {
                    "@type": "ListItem", "position": 3,
                    "name": post_title,
                    "item": canonical,
                },
            ],
        });
        let serialized = doc.to_string().replace("</", "<\\/");
        format!("    <script type=\"application/ld+json\">{serialized}</script>\n")
    };

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
    <meta name="description" content="{description}">
    <meta property="og:title" content="{title}">
    <meta property="og:description" content="{description}">
    <meta property="og:type" content="article">
    <meta property="og:url" content="{canonical}">
    <meta property="og:site_name" content="{space_name}">
{icon_meta}
{article_meta}    <meta name="twitter:card" content="{card}">
    <meta name="twitter:title" content="{title}">
    <meta name="twitter:description" content="{description}">
    <link rel="canonical" href="{canonical}">
{oembed_disc}{pagination_links}{json_ld}{breadcrumb_ld}  </head>
  <body>
    <nav><a href="{base}/s/{space_seg}">{space_name}</a> &rsaquo; <a href="{base}/s/{space_seg}/{chan_seg}">{channel_display}</a></nav>
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
        base = base,
        space_seg = space_seg,
        chan_seg = chan_seg,
        space_name = escape_html(&space.name),
        icon_meta = icon_meta,
        article_meta = article_meta,
        card = card,
        oembed_disc = oembed_disc,
        pagination_links = pagination_links,
        json_ld = json_ld,
        breadcrumb_ld = breadcrumb_ld,
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
            .map(|d| meta_text(d, 200))
            .unwrap_or_else(|| escape_html(&space.name));
        let icon_url = space_social_image(&format!("https://{host}"), &space);
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
        .map(|d| meta_text(d, 200))
        .unwrap_or_default();

    let host = extract_host(&headers);
    let base = format!("https://{host}");
    let space_seg = url_seg(&space_slug);

    let social_image = space_social_image(&base, &space);
    let icon_meta = image_meta(social_image.as_deref());
    let card = twitter_card(social_image.as_deref());

    let canonical = format!("{base}/s/{space_seg}");
    let oembed_disc = oembed_link(&base, &canonical);

    // Structured data: the space is a discussion/collection page.
    let collection_ld = {
        let doc = serde_json::json!({
            "@context": "https://schema.org",
            "@type": "CollectionPage",
            "name": space.name,
            "url": canonical,
            "description": space.description,
        });
        let serialized = doc.to_string().replace("</", "<\\/");
        format!("    <script type=\"application/ld+json\">{serialized}</script>\n")
    };

    // Render channel list (skip categories, DMs and voice).
    let mut channel_list_html = String::new();
    for ch in &channels {
        if is_hidden_channel_type(&ch.channel_type) {
            continue;
        }
        if let Some(name) = &ch.name {
            channel_list_html.push_str(&format!(
                r#"        <li><a href="{base}/s/{space_seg}/{chan_seg}">{label}</a></li>
"#,
                base = base,
                space_seg = space_seg,
                chan_seg = url_seg(name),
                label = escape_html(name),
            ));
        }
    }

    let html = format!(
        r#"<!DOCTYPE html>
<html lang="en">
  <head>
    <meta charset="utf-8">
    <title>{title}</title>
    <meta name="description" content="{description}">
    <meta property="og:title" content="{title}">
    <meta property="og:description" content="{description}">
    <meta property="og:type" content="website">
    <meta property="og:url" content="{canonical}">
    <meta property="og:site_name" content="{title}">
{icon_meta}
    <meta name="twitter:card" content="{card}">
    <meta name="twitter:title" content="{title}">
    <meta name="twitter:description" content="{description}">
    <link rel="canonical" href="{canonical}">
{oembed_disc}{collection_ld}  </head>
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
        card = card,
        oembed_disc = oembed_disc,
        collection_ld = collection_ld,
        channel_list_html = channel_list_html,
        space_slug_escaped = escape_html(&space_slug),
    );

    Ok(Html(html))
}

// -------------------------------------------------------------------------
// GET /robots.txt
// -------------------------------------------------------------------------

/// Serves a robots.txt that allows crawling the public `/s/` snapshots and
/// points crawlers at the sitemap.
pub async fn robots(headers: HeaderMap) -> impl IntoResponse {
    let host = extract_host(&headers);
    let body = format!("User-agent: *\nAllow: /s/\nSitemap: https://{host}/sitemap.xml\n",);
    ([(header::CONTENT_TYPE, "text/plain; charset=utf-8")], body)
}

// -------------------------------------------------------------------------
// GET /sitemap.xml
// -------------------------------------------------------------------------

/// Serves an XML sitemap enumerating every public space, its public
/// text/forum channels, and every top-level forum post — so crawlers can
/// discover the per-post URLs that are otherwise only reachable from inside
/// the app.
pub async fn sitemap(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, AppError> {
    let host = extract_host(&headers);
    let base = format!("https://{host}");

    let mut entries = String::new();
    let mut push = |loc: String, lastmod: Option<&str>| {
        entries.push_str("  <url>\n");
        entries.push_str(&format!("    <loc>{}</loc>\n", xml_escape(&loc)));
        if let Some(m) = lastmod {
            entries.push_str(&format!("    <lastmod>{}</lastmod>\n", xml_escape(m)));
        }
        entries.push_str("  </url>\n");
    };

    let spaces = db::spaces::list_public_spaces(&state.db).await?;
    for space in &spaces {
        let space_seg = url_seg(&space.slug);
        push(format!("{base}/s/{space_seg}"), None);

        let channels = db::channels::list_channels_in_space(&state.db, &space.id).await?;
        for ch in &channels {
            if is_hidden_channel_type(&ch.channel_type) {
                continue;
            }
            let Some(name) = ch.name.as_deref() else {
                continue;
            };
            let chan_seg = url_seg(name);
            push(format!("{base}/s/{space_seg}/{chan_seg}"), None);

            if ch.channel_type == "forum" {
                let posts = db::messages::list_messages(&state.db, &ch.id, None, 200, None).await?;
                for p in &posts {
                    let lastmod = lastmod_date(p.edited_at.as_deref().unwrap_or(&p.created_at));
                    push(
                        format!("{base}/s/{space_seg}/{chan_seg}/{}", url_seg(&p.id)),
                        Some(&lastmod),
                    );
                }
            }
        }
    }

    let xml = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
<urlset xmlns=\"http://www.sitemaps.org/schemas/sitemap/0.9\">\n\
{entries}</urlset>\n"
    );

    Ok((
        [(header::CONTENT_TYPE, "application/xml; charset=utf-8")],
        xml,
    ))
}

// -------------------------------------------------------------------------
// GET /oembed?url=...&format=json
// -------------------------------------------------------------------------

#[derive(serde::Deserialize)]
pub struct OEmbedQuery {
    pub url: String,
    pub format: Option<String>,
    #[allow(dead_code)]
    pub maxwidth: Option<i64>,
    #[allow(dead_code)]
    pub maxheight: Option<i64>,
}

/// oEmbed provider endpoint. Given the `url` of a public space, channel, or
/// forum-post page, returns a `link`-type oEmbed document so chat apps and
/// social platforms can render a rich card. Only the JSON format is supported.
pub async fn oembed(
    State(state): State<AppState>,
    Query(query): Query<OEmbedQuery>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, AppError> {
    if let Some(fmt) = query.format.as_deref() {
        if !fmt.eq_ignore_ascii_case("json") {
            return Err(AppError::BadRequest(
                "only the json oembed format is supported".to_string(),
            ));
        }
    }

    let host = extract_host(&headers);
    let base = format!("https://{host}");

    // Extract the `/s/...` path out of the supplied URL and decode segments.
    let path = query
        .url
        .find("/s/")
        .map(|i| &query.url[i..])
        .ok_or_else(|| AppError::BadRequest("url is not a daccord resource".to_string()))?;
    let path = path.split(['?', '#']).next().unwrap_or(path);
    let segs: Vec<String> = path
        .trim_start_matches('/')
        .split('/')
        .filter(|s| !s.is_empty())
        .map(url_unseg)
        .collect();

    if segs.first().map(String::as_str) != Some("s") || segs.len() < 2 {
        return Err(AppError::BadRequest(
            "url is not a daccord resource".to_string(),
        ));
    }

    let space = db::spaces::get_space_by_slug(&state.db, &segs[1]).await?;
    if !space.public {
        return Err(AppError::NotFound("not_found".to_string()));
    }

    let thumbnail = space_social_image(&base, &space);
    let mut title = space.name.clone();
    let mut author_name = space.name.clone();

    if segs.len() >= 3 {
        let channels = db::channels::list_channels_in_space(&state.db, &space.id).await?;
        let channel = channels
            .iter()
            .find(|c| c.name.as_deref() == Some(segs[2].as_str()))
            .ok_or_else(|| AppError::NotFound("unknown_channel".to_string()))?;

        if segs.len() >= 4 {
            let post = db::messages::get_message_row(&state.db, &segs[3]).await?;
            if post.channel_id != channel.id {
                return Err(AppError::NotFound("message_not_in_channel".to_string()));
            }
            title = resolve_post_title(&post);
            author_name = db::users::get_user(&state.db, &post.author_id)
                .await
                .ok()
                .map(|u| u.display_name.unwrap_or(u.username))
                .unwrap_or_else(|| "Unknown".to_string());
        } else {
            title = format!(
                "{} — {}",
                channel.name.as_deref().unwrap_or("channel"),
                space.name,
            );
        }
    }

    let mut doc = serde_json::json!({
        "version": "1.0",
        "type": "link",
        "title": title,
        "author_name": author_name,
        "provider_name": "daccord",
        "provider_url": base,
        "cache_age": 3600,
    });
    if let Some(thumb) = thumbnail {
        doc["thumbnail_url"] = serde_json::Value::String(thumb);
    }

    Ok((
        [(header::CONTENT_TYPE, "application/json; charset=utf-8")],
        doc.to_string(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::message::MessageRow;

    fn blank_post() -> MessageRow {
        MessageRow {
            id: "1".into(),
            channel_id: "c".into(),
            space_id: None,
            author_id: "a".into(),
            content: String::new(),
            message_type: "default".into(),
            created_at: "2026-06-13 11:00:00".into(),
            edited_at: None,
            tts: false,
            pinned: false,
            mention_everyone: false,
            mentions: "[]".into(),
            mention_roles: "[]".into(),
            embeds: "[]".into(),
            reply_to: None,
            flags: 0,
            webhook_id: None,
            thread_id: None,
            title: None,
            origin: None,
        }
    }

    fn blank_space() -> SpaceRow {
        SpaceRow {
            id: "1".into(),
            name: "Space".into(),
            slug: "space".into(),
            description: None,
            icon: None,
            banner: None,
            splash: None,
            owner_id: "o".into(),
            verification_level: "none".into(),
            default_notifications: "all".into(),
            explicit_content_filter: "disabled".into(),
            vanity_url_code: None,
            preferred_locale: "en-US".into(),
            afk_channel_id: None,
            afk_timeout: 0,
            system_channel_id: None,
            rules_channel_id: None,
            nsfw_level: "default".into(),
            premium_tier: "none".into(),
            public: true,
            allow_guest_access: true,
            premium_subscription_count: 0,
            max_members: 0,
            created_at: "2026-06-13 11:00:00".into(),
        }
    }

    #[test]
    fn url_seg_keeps_unreserved_and_encodes_the_rest() {
        assert_eq!(url_seg("self-hosting"), "self-hosting");
        assert_eq!(url_seg("Dev Talk"), "Dev%20Talk");
        // Path-breaking characters must be percent-encoded.
        assert_eq!(url_seg("a/b?c#d"), "a%2Fb%3Fc%23d");
    }

    #[test]
    fn xml_escape_covers_all_five_entities() {
        assert_eq!(
            xml_escape(r#"<a href="x">&'"#),
            "&lt;a href=&quot;x&quot;&gt;&amp;&apos;"
        );
    }

    #[test]
    fn iso_datetime_inserts_t_separator() {
        assert_eq!(iso_datetime("2026-06-13 11:00:00"), "2026-06-13T11:00:00");
    }

    #[test]
    fn lastmod_date_takes_date_portion() {
        assert_eq!(lastmod_date("2026-06-13 11:00:00"), "2026-06-13");
    }

    #[test]
    fn resolve_post_title_prefers_title_then_content_then_placeholder() {
        let mut p = blank_post();
        p.title = Some("  Real Title  ".into());
        p.content = "ignored".into();
        assert_eq!(resolve_post_title(&p), "Real Title");

        p.title = None;
        p.content = "\n\n  first line\nsecond".into();
        assert_eq!(resolve_post_title(&p), "first line");

        p.content = "   \n  ".into();
        assert_eq!(resolve_post_title(&p), "Untitled post");
    }

    #[test]
    fn url_unseg_is_the_inverse_of_url_seg() {
        for raw in ["self-hosting", "Dev Talk", "a/b?c#d", "héllo", "100%"] {
            assert_eq!(
                url_unseg(&url_seg(raw)),
                raw,
                "round-trip failed for {raw:?}"
            );
        }
    }

    #[test]
    fn cdn_url_normalises_all_three_storage_conventions() {
        let base = "https://host";
        // Bare filename (legacy).
        assert_eq!(
            cdn_url(base, "icons", "x.png"),
            "https://host/cdn/icons/x.png"
        );
        // Root-relative path (current save_avatar_image output).
        assert_eq!(
            cdn_url(base, "icons", "/cdn/icons/x.png"),
            "https://host/cdn/icons/x.png"
        );
        // Absolute URL passes through untouched.
        assert_eq!(
            cdn_url(base, "icons", "https://cdn.example/x.png"),
            "https://cdn.example/x.png"
        );
    }

    #[test]
    fn space_social_image_prefers_banner_then_splash_then_icon() {
        let mut s = blank_space();
        s.icon = Some("i.png".into());
        assert_eq!(
            space_social_image("https://h", &s),
            Some("https://h/cdn/icons/i.png".into())
        );
        s.splash = Some("sp.png".into());
        assert_eq!(
            space_social_image("https://h", &s),
            Some("https://h/cdn/splashes/sp.png".into())
        );
        s.banner = Some("b.png".into());
        assert_eq!(
            space_social_image("https://h", &s),
            Some("https://h/cdn/banners/b.png".into())
        );
        let empty = blank_space();
        assert_eq!(space_social_image("https://h", &empty), None);
    }

    #[test]
    fn twitter_card_upgrades_when_image_present() {
        assert_eq!(twitter_card(Some("x")), "summary_large_image");
        assert_eq!(twitter_card(None), "summary");
    }

    #[test]
    fn hidden_channel_types_are_excluded() {
        for t in ["category", "dm", "group_dm", "voice"] {
            assert!(is_hidden_channel_type(t), "{t} should be hidden");
        }
        for t in ["text", "forum"] {
            assert!(!is_hidden_channel_type(t), "{t} should be public");
        }
    }
}
