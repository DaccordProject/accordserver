use crate::models::embed::{Embed, EmbedAuthor, EmbedImage};
use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use reqwest::Client;
use std::sync::Arc;
use tracing::warn;

const MAX_URLS: usize = 5;
const MAX_HTML_BYTES: usize = 1024 * 1024;

// Validate literals separately: reqwest bypasses its DNS resolver for IP URLs.
fn validate_url(url: &reqwest::Url) -> bool {
    matches!(url.scheme(), "http" | "https")
        && url.username().is_empty()
        && url.password().is_none()
        && url.host_str().is_some_and(|host| {
            let host = host.trim_matches(['[', ']']);
            !host.trim_end_matches('.').eq_ignore_ascii_case("localhost")
                && host
                    .parse()
                    .map(|ip| !crate::federation::peers::is_private(&ip))
                    .unwrap_or(true)
        })
}

#[derive(Debug)]
struct PreviewResolver;

impl Resolve for PreviewResolver {
    fn resolve(&self, name: Name) -> Resolving {
        Box::pin(async move {
            let addresses: Vec<_> = tokio::net::lookup_host((name.as_str(), 0)).await?.collect();
            if addresses.is_empty()
                || addresses
                    .iter()
                    .any(|a| crate::federation::peers::is_private(&a.ip()))
            {
                return Err("link preview host is not a public address".into());
            }
            let addresses: Addrs = Box::new(addresses.into_iter());
            Ok(addresses)
        })
    }
}

fn preview_client_builder() -> reqwest::ClientBuilder {
    Client::builder()
        .timeout(FETCH_TIMEOUT)
        .no_proxy()
        .dns_resolver(Arc::new(PreviewResolver))
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if attempt.previous().len() >= 3 || !validate_url(attempt.url()) {
                attempt.stop()
            } else {
                attempt.follow()
            }
        }))
}
const FETCH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Extract URLs from message text content.
pub fn extract_urls(content: &str) -> Vec<String> {
    let mut urls = Vec::new();
    // Simple URL regex: match http(s)://... up to whitespace or common delimiters
    for word in content.split_whitespace() {
        let trimmed = word.trim_matches(|c: char| "<>()[]\"'".contains(c));
        if (trimmed.starts_with("http://") || trimmed.starts_with("https://"))
            && trimmed.contains('.')
        {
            urls.push(trimmed.to_string());
            if urls.len() >= MAX_URLS {
                break;
            }
        }
    }
    urls
}

/// Fetch OpenGraph metadata from a URL and build an Embed.
async fn unfurl_url(url: &str, client: &Client) -> Option<Embed> {
    if !validate_url(&reqwest::Url::parse(url).ok()?) {
        return None;
    }
    let mut response = client
        .get(url)
        .header("User-Agent", "AccordBot/1.0 (link preview)")
        .timeout(FETCH_TIMEOUT)
        .send()
        .await
        .ok()?;

    let status = response.status();
    if !status.is_success() {
        return None;
    }

    // Only parse HTML responses
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if !content_type.contains("text/html") {
        return None;
    }

    if response
        .content_length()
        .is_some_and(|len| len > MAX_HTML_BYTES as u64)
    {
        return None;
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.ok()? {
        if chunk.len() > MAX_HTML_BYTES - body.len() {
            return None;
        }
        body.extend_from_slice(&chunk);
    }
    parse_opengraph(&String::from_utf8_lossy(&body), url)
}

/// Parse OpenGraph meta tags from HTML body.
fn parse_opengraph(html: &str, source_url: &str) -> Option<Embed> {
    let mut og_title: Option<String> = None;
    let mut og_description: Option<String> = None;
    let mut og_image: Option<String> = None;
    let mut og_site_name: Option<String> = None;
    let mut og_type: Option<String> = None;
    let mut html_title: Option<String> = None;

    // Simple meta tag extraction without a full HTML parser.
    // Looks for <meta property="og:..." content="..."> patterns.
    let lower = html.to_ascii_lowercase();

    for meta in extract_meta_tags(html) {
        let property = meta.0.to_lowercase();
        let content = meta.1;
        match property.as_str() {
            "og:title" => og_title = Some(content),
            "og:description" => og_description = Some(content),
            "og:image" => og_image = Some(content),
            "og:site_name" => og_site_name = Some(content),
            "og:type" => og_type = Some(content),
            _ => {}
        }
    }

    // Fallback: try to extract <title> if no og:title
    if og_title.is_none() {
        if let Some(start) = lower.find("<title") {
            if let Some(tag_end) = html[start..].find('>') {
                let after_tag = start + tag_end + 1;
                if let Some(end) = lower[after_tag..].find("</title>") {
                    let title_text = html[after_tag..after_tag + end].trim();
                    if !title_text.is_empty() {
                        html_title = Some(decode_html_entities(title_text));
                    }
                }
            }
        }
    }

    let title = og_title.or(html_title);

    // Need at least a title or description to produce an embed
    if title.is_none() && og_description.is_none() {
        return None;
    }

    let embed_type = match og_type.as_deref() {
        Some("video") | Some("video.other") => Some("video".to_string()),
        _ => Some("link".to_string()),
    };

    let image = og_image.map(|img_url| {
        let resolved = resolve_url(&img_url, source_url);
        EmbedImage {
            url: resolved,
            width: None,
            height: None,
        }
    });

    let author = og_site_name.map(|name| EmbedAuthor {
        name,
        url: None,
        icon_url: None,
    });

    Some(Embed {
        title,
        embed_type,
        description: og_description,
        url: Some(source_url.to_string()),
        timestamp: None,
        color: None,
        footer: None,
        image,
        thumbnail: None,
        author,
        fields: None,
    })
}

/// Extract meta tag property/content pairs from HTML.
fn extract_meta_tags(html: &str) -> Vec<(String, String)> {
    let mut tags = Vec::new();
    let lower = html.to_ascii_lowercase();
    let mut search_from = 0;

    while let Some(meta_start) = lower[search_from..].find("<meta ") {
        let abs_start = search_from + meta_start;
        let segment = if let Some(end) = html[abs_start..].find('>') {
            &html[abs_start..abs_start + end + 1]
        } else {
            search_from = abs_start + 6;
            continue;
        };

        let property = extract_attr(segment, "property").or_else(|| extract_attr(segment, "name"));
        let content = extract_attr(segment, "content");

        if let (Some(prop), Some(cont)) = (property, content) {
            tags.push((prop, decode_html_entities(&cont)));
        }

        search_from = abs_start + segment.len();
    }

    tags
}

/// Extract an HTML attribute value from a tag string.
fn extract_attr(tag: &str, attr_name: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let pattern = format!("{}=\"", attr_name);
    if let Some(start) = lower.find(&pattern) {
        let value_start = start + pattern.len();
        if let Some(end) = tag[value_start..].find('"') {
            return Some(tag[value_start..value_start + end].to_string());
        }
    }
    // Try single quotes
    let pattern_sq = format!("{}='", attr_name);
    if let Some(start) = lower.find(&pattern_sq) {
        let value_start = start + pattern_sq.len();
        if let Some(end) = tag[value_start..].find('\'') {
            return Some(tag[value_start..value_start + end].to_string());
        }
    }
    None
}

/// Resolve a potentially relative URL against a base URL.
fn resolve_url(url: &str, base: &str) -> String {
    if url.starts_with("http://") || url.starts_with("https://") {
        return url.to_string();
    }
    if url.starts_with("//") {
        // Protocol-relative
        if base.starts_with("https://") {
            return format!("https:{}", url);
        }
        return format!("http:{}", url);
    }
    // Extract origin from base
    if let Some(slash_idx) = base
        .find("://")
        .and_then(|s| base[s + 3..].find('/').map(|i| i + s + 3))
    {
        if url.starts_with('/') {
            return format!("{}{}", &base[..slash_idx], url);
        }
        return format!("{}/{}", &base[..slash_idx], url);
    }
    // Last resort: just append
    format!(
        "{}/{}",
        base.trim_end_matches('/'),
        url.trim_start_matches('/')
    )
}

/// Decode common HTML entities.
fn decode_html_entities(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
}

/// Unfurl all URLs in a message and return generated embeds.
pub async fn unfurl_message_urls(content: &str) -> Vec<Embed> {
    let urls = extract_urls(content);
    if urls.is_empty() {
        return Vec::new();
    }

    let client = match preview_client_builder().build() {
        Ok(c) => c,
        Err(e) => {
            warn!("Failed to build HTTP client for unfurling: {e}");
            return Vec::new();
        }
    };

    let mut embeds = Vec::new();
    for url in &urls {
        if let Some(embed) = unfurl_url(url, &client).await {
            embeds.push(embed);
        }
    }
    embeds
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_non_public_literal_urls() {
        for url in [
            "http://127.0.0.1/",
            "http://2130706433/",
            "http://169.254.169.254/",
            "http://10.0.0.1/",
            "http://100.64.0.1/",
            "http://198.18.0.1/",
            "http://240.0.0.1/",
            "http://[::1]/",
            "http://[::ffff:127.0.0.1]/",
            "http://[fc00::1]/",
            "http://[ff02::1]/",
            "http://[64:ff9b::7f00:1]/",
            "http://localhost./",
            "file:///etc/passwd",
            "http://user:password@example.com/",
        ] {
            assert!(!validate_url(&reqwest::Url::parse(url).unwrap()), "{url}");
        }
        assert!(validate_url(
            &reqwest::Url::parse("https://example.com/").unwrap()
        ));
        assert!(validate_url(
            &reqwest::Url::parse("https://[2606:4700:4700::1111]/").unwrap()
        ));
    }

    #[test]
    fn unicode_case_mapping_cannot_corrupt_parser_offsets() {
        let html = "İİİİİİİİİİİİİİİİİİİİİİİİİİİİİİİİİİİİİİİİİİİİİİİİİİİİİİİİİİİİİİİİİİİİİİİİİİİİİİİİİİ<title>safe</title><meta property='og:description' content='safe'>";
        let embed = parse_opengraph(html, "https://example.com").unwrap();
        assert_eq!(embed.title.as_deref(), Some("safe"));
        assert_eq!(embed.description.as_deref(), Some("safe"));
    }

    #[tokio::test]
    async fn preview_client_blocks_internal_dns_and_redirect_targets() {
        use axum::{routing::get, Router};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let redirect = format!("http://{addr}/secret");
        let app = Router::new()
            .route(
                "/",
                get(move || async move { axum::response::Redirect::temporary(&redirect) }),
            )
            .route(
                "/secret",
                get(|| async { axum::response::Html("<title>secret</title>") }),
            );
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let client = preview_client_builder().build().unwrap();
        assert!(
            unfurl_url(&format!("http://localhost:{}/secret", addr.port()), &client)
                .await
                .is_none()
        );
        // Inject a public-host resolution solely to exercise the real redirect policy.
        let client = preview_client_builder()
            .resolve("preview.test", addr)
            .build()
            .unwrap();
        assert!(
            unfurl_url(&format!("http://preview.test:{}/", addr.port()), &client)
                .await
                .is_none()
        );
        task.abort();
    }

    #[tokio::test]
    async fn preview_body_limit_applies_without_content_length() {
        use axum::{body::Body, routing::get, Router};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = Router::new().route(
            "/",
            get(|| async {
                let chunks = futures_util::stream::iter([
                    Ok::<_, std::io::Error>(vec![b'a'; MAX_HTML_BYTES]),
                    Ok(b"<title>oversized</title>".to_vec()),
                ]);
                ([("content-type", "text/html")], Body::from_stream(chunks))
            }),
        );
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let client = Client::builder()
            .no_proxy()
            .resolve("preview.test", addr)
            .build()
            .unwrap();
        assert!(
            unfurl_url(&format!("http://preview.test:{}/", addr.port()), &client)
                .await
                .is_none()
        );
        task.abort();
    }

    #[test]
    fn test_extract_urls_basic() {
        let urls = extract_urls("Check out https://example.com and http://foo.bar/path?q=1");
        assert_eq!(urls.len(), 2);
        assert_eq!(urls[0], "https://example.com");
        assert_eq!(urls[1], "http://foo.bar/path?q=1");
    }

    #[test]
    fn test_extract_urls_no_urls() {
        let urls = extract_urls("Hello world, no links here");
        assert!(urls.is_empty());
    }

    #[test]
    fn test_extract_urls_max_limit() {
        let content =
            "https://a.com https://b.com https://c.com https://d.com https://e.com https://f.com";
        let urls = extract_urls(content);
        assert_eq!(urls.len(), MAX_URLS);
    }

    #[test]
    fn test_extract_urls_strips_brackets() {
        let urls = extract_urls("<https://example.com> and (https://other.com)");
        assert_eq!(urls[0], "https://example.com");
        assert_eq!(urls[1], "https://other.com");
    }

    #[test]
    fn test_parse_opengraph_basic() {
        let html = r#"
            <html><head>
                <meta property="og:title" content="Test Page">
                <meta property="og:description" content="A test description">
                <meta property="og:image" content="https://example.com/img.png">
                <meta property="og:site_name" content="Example">
            </head></html>
        "#;
        let embed = parse_opengraph(html, "https://example.com/page").unwrap();
        assert_eq!(embed.title.as_deref(), Some("Test Page"));
        assert_eq!(embed.description.as_deref(), Some("A test description"));
        assert_eq!(embed.url.as_deref(), Some("https://example.com/page"));
        assert_eq!(
            embed.image.as_ref().unwrap().url,
            "https://example.com/img.png"
        );
        assert_eq!(embed.author.as_ref().unwrap().name, "Example");
        assert_eq!(embed.embed_type.as_deref(), Some("link"));
    }

    #[test]
    fn test_parse_opengraph_fallback_title() {
        let html = r#"<html><head><title>Fallback Title</title></head></html>"#;
        let embed = parse_opengraph(html, "https://example.com").unwrap();
        assert_eq!(embed.title.as_deref(), Some("Fallback Title"));
    }

    #[test]
    fn test_parse_opengraph_no_metadata() {
        let html = r#"<html><body>No metadata here</body></html>"#;
        assert!(parse_opengraph(html, "https://example.com").is_none());
    }

    #[test]
    fn test_parse_opengraph_video_type() {
        let html = r#"
            <html><head>
                <meta property="og:title" content="Video">
                <meta property="og:type" content="video.other">
            </head></html>
        "#;
        let embed = parse_opengraph(html, "https://example.com").unwrap();
        assert_eq!(embed.embed_type.as_deref(), Some("video"));
    }

    #[test]
    fn test_resolve_url_absolute() {
        assert_eq!(
            resolve_url("https://img.example.com/a.png", "https://example.com"),
            "https://img.example.com/a.png"
        );
    }

    #[test]
    fn test_resolve_url_relative() {
        assert_eq!(
            resolve_url("/images/a.png", "https://example.com/page"),
            "https://example.com/images/a.png"
        );
    }

    #[test]
    fn test_resolve_url_protocol_relative() {
        assert_eq!(
            resolve_url("//cdn.example.com/a.png", "https://example.com"),
            "https://cdn.example.com/a.png"
        );
    }

    #[test]
    fn test_decode_html_entities() {
        assert_eq!(decode_html_entities("A &amp; B &lt;tag&gt;"), "A & B <tag>");
    }
}
