use crate::models::embed::{Embed, EmbedAuthor, EmbedImage};
use reqwest::Client;
use tracing::warn;

const MAX_URLS: usize = 5;
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
pub async fn unfurl_url(url: &str, client: &Client) -> Option<Embed> {
    let response = client
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

    let body = response.text().await.ok()?;
    parse_opengraph(&body, url)
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
    let lower = html.to_lowercase();

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
    let lower = html.to_lowercase();
    let mut search_from = 0;

    while let Some(meta_start) = lower[search_from..].find("<meta ") {
        let abs_start = search_from + meta_start;
        let segment = if let Some(end) = html[abs_start..].find('>') {
            &html[abs_start..abs_start + end + 1]
        } else {
            search_from = abs_start + 6;
            continue;
        };

        let property = extract_attr(segment, "property")
            .or_else(|| extract_attr(segment, "name"));
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
    let lower = tag.to_lowercase();
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
    format!("{}/{}", base.trim_end_matches('/'), url.trim_start_matches('/'))
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

    let client = match Client::builder()
        .timeout(FETCH_TIMEOUT)
        .redirect(reqwest::redirect::Policy::limited(3))
        .build()
    {
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
        let content = "https://a.com https://b.com https://c.com https://d.com https://e.com https://f.com";
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
        assert_eq!(embed.image.as_ref().unwrap().url, "https://example.com/img.png");
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
