# SEO & social-embed layer

accordserver serves crawler-friendly, server-rendered snapshots of **public**
spaces, channels, and forum posts so that:

- search engines can index public community content (the Flutter web client is
  CanvasKit-rendered and therefore invisible to crawlers), and
- pasting a link into Discord, Slack, iMessage, X, Facebook, etc. produces a
  rich social card.

All of this is implemented in [`src/routes/seo.rs`](../src/routes/seo.rs) and
wired up in [`src/routes/mod.rs`](../src/routes/mod.rs). No external service or
separate origin is involved — the SEO pages live at the same origin as the API
and CDN.

## URL structure

| URL | Page |
|-----|------|
| `GET /s/{space_slug}` | Space overview (lists public channels) |
| `GET /s/{space_slug}/{channel_name}` | Channel page (forum: lists posts; text: recent messages) |
| `GET /s/{space_slug}/{channel_name}/{post_id}` | Forum post + threaded replies (paginated) |
| `GET /robots.txt` | Robots policy + sitemap pointer |
| `GET /sitemap.xml` | Enumerates every public space, channel, and forum post |
| `GET /oembed?url=...&format=json` | oEmbed provider endpoint |

Path segments are percent-encoded (`url_seg`) when generated and decoded
(`url_unseg`) when parsed, so spaces/channels with spaces or punctuation in
their names round-trip safely.

## Gating: "public space = public"

A space is crawlable iff its `public` column is `true`. There is **no**
per-channel opt-in flag required — any non-hidden channel in a public space is
rendered for crawlers. Hidden channel types (`category`, `dm`, `group_dm`,
`voice`) are never exposed.

- **Crawlers** (matched by `User-Agent`, see `CRAWLER_AGENTS`) get the
  server-rendered HTML snapshot. If the space is not public they get `404`.
- **Humans** always get a lightweight redirect landing page that tries the
  `daccord://connect/{host}/{slug}` deep link, then falls back to the web
  client. This applies whether or not the space is public — the client itself
  handles any registration/invite prompts.

## Meta tags

Every snapshot `<head>` includes:

- `<title>` and a plain `<meta name="description">`
- Open Graph: `og:title`, `og:description`, `og:type`
  (`website` for spaces/channels, `article` for posts), `og:url`,
  `og:site_name`, and `og:image` when the space has an image
- Twitter Card: `twitter:card` (`summary_large_image` when an image is
  present, else `summary`), `twitter:title`, `twitter:description`,
  `twitter:image`
- `<link rel="canonical">` (absolute URL)
- Forum posts additionally emit `article:published_time`,
  `article:modified_time` (when edited), and `article:author`

### Social-card image

`space_social_image()` picks the card image with a fallback chain:

```
banner → splash → icon
```

Stored references are normalised to absolute URLs by `cdn_url()`, which accepts
a full URL, a root-relative `/cdn/...` path, or a bare filename (legacy
uploads). Descriptions are whitespace-collapsed and truncated by `meta_text()`
so cards never contain raw newlines.

## Structured data (JSON-LD / Schema.org)

| Page | `@type`(s) |
|------|-----------|
| Space | `CollectionPage` |
| Channel | `BreadcrumbList` (Space › Channel) |
| Forum post | `DiscussionForumPosting` + `BreadcrumbList` (Space › Channel › Post) |

`DiscussionForumPosting` includes `headline`, `url`, `datePublished`,
`author`, `articleBody`, and an `interactionStatistic` comment counter. All
JSON-LD is emitted with `</` escaped to `<\/` so post content can't prematurely
close the `<script>` element.

## oEmbed

`GET /oembed?url={page-url}&format=json` returns a `link`-type oEmbed document:

```json
{
  "version": "1.0",
  "type": "link",
  "title": "How I self-host daccord on a Pi",
  "author_name": "Alice",
  "provider_name": "daccord",
  "provider_url": "https://your.server",
  "thumbnail_url": "https://your.server/cdn/banners/....png",
  "cache_age": 3600
}
```

- Only `format=json` is supported; any other value returns `400`.
- The `url` parameter is parsed for its `/s/...` path; the space must be public
  or the endpoint returns `404`.
- `title`/`author_name` are resolved at the most specific level present in the
  URL (post → channel → space).

Every snapshot page advertises this endpoint with a discovery link:

```html
<link rel="alternate" type="application/json+oembed"
      href="https://your.server/oembed?url=...&format=json" title="daccord oEmbed">
```

## Sitemap & robots

`robots.txt` allows `/s/` and points crawlers at `/sitemap.xml`. The sitemap
walks `list_public_spaces()` → channels → forum posts (up to 200 per channel),
emitting a `<url>` for each, with `<lastmod>` on posts (their `edited_at` or
`created_at` date). Hidden channel types are skipped.

## Forum post titles

`resolve_post_title()` mirrors the client's `resolveForumPostTitle`: it prefers
the message's dedicated `title`, falls back to the first non-empty line of
content, then `"Untitled post"`.

## Tests

Unit tests in `seo.rs` cover the pure helpers: `url_seg`/`url_unseg`
round-tripping, `cdn_url` normalisation, the `space_social_image` fallback
order, `twitter_card` selection, `xml_escape`, `iso_datetime`, `lastmod_date`,
`resolve_post_title`, and hidden-channel exclusion.

## Client share button

The Flutter client exposes a share affordance on forum posts with two options:

- **Share with those who have the app** → `daccord://navigate/{spaceId}/{channelId}?msg={postId}`
- **Share with the internet** → `https://{host}/s/{slug}/{channel}/{postId}`

The first opens the post directly in a daccord client; the second is the public,
crawlable URL documented above.
