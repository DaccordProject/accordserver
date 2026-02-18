/// Convert a name into a URL-safe slug.
///
/// Lowercases, replaces non-alphanumeric characters with hyphens,
/// collapses consecutive hyphens, trims leading/trailing hyphens,
/// and truncates to 100 characters.
pub fn slugify(name: &str) -> String {
    let slug: String = name
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();

    // Collapse consecutive hyphens
    let mut result = String::with_capacity(slug.len());
    let mut prev_hyphen = false;
    for c in slug.chars() {
        if c == '-' {
            if !prev_hyphen {
                result.push('-');
            }
            prev_hyphen = true;
        } else {
            result.push(c);
            prev_hyphen = false;
        }
    }

    // Trim leading/trailing hyphens
    let trimmed = result.trim_matches('-');

    // Truncate to 100 chars (on a char boundary, but all ASCII so safe)
    if trimmed.len() > 100 {
        trimmed[..100].trim_end_matches('-').to_string()
    } else {
        trimmed.to_string()
    }
}

/// Validate a user-provided slug.
///
/// Rules: non-empty, ≤100 chars, only `[a-z0-9-]`, no leading/trailing
/// hyphens, no consecutive hyphens.
pub fn validate_slug(slug: &str) -> Result<(), &'static str> {
    if slug.is_empty() {
        return Err("slug must not be empty");
    }
    if slug.len() > 100 {
        return Err("slug must be 100 characters or fewer");
    }
    if !slug
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        return Err("slug may only contain lowercase letters, digits, and hyphens");
    }
    if slug.starts_with('-') || slug.ends_with('-') {
        return Err("slug must not start or end with a hyphen");
    }
    if slug.contains("--") {
        return Err("slug must not contain consecutive hyphens");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_slugify_basic() {
        assert_eq!(slugify("My Cool Space"), "my-cool-space");
    }

    #[test]
    fn test_slugify_special_chars() {
        assert_eq!(slugify("Hello, World! (2024)"), "hello-world-2024");
    }

    #[test]
    fn test_slugify_leading_trailing() {
        assert_eq!(slugify("  --spaced-- "), "spaced");
    }

    #[test]
    fn test_slugify_consecutive_hyphens() {
        assert_eq!(slugify("a---b"), "a-b");
    }

    #[test]
    fn test_slugify_empty() {
        assert_eq!(slugify(""), "");
    }

    #[test]
    fn test_slugify_long_name() {
        let long = "a".repeat(200);
        let slug = slugify(&long);
        assert!(slug.len() <= 100);
    }

    #[test]
    fn test_validate_slug_valid() {
        assert!(validate_slug("my-cool-space").is_ok());
        assert!(validate_slug("abc123").is_ok());
        assert!(validate_slug("a").is_ok());
    }

    #[test]
    fn test_validate_slug_empty() {
        assert!(validate_slug("").is_err());
    }

    #[test]
    fn test_validate_slug_uppercase() {
        assert!(validate_slug("Hello").is_err());
    }

    #[test]
    fn test_validate_slug_leading_hyphen() {
        assert!(validate_slug("-abc").is_err());
    }

    #[test]
    fn test_validate_slug_trailing_hyphen() {
        assert!(validate_slug("abc-").is_err());
    }

    #[test]
    fn test_validate_slug_consecutive_hyphens() {
        assert!(validate_slug("a--b").is_err());
    }

    #[test]
    fn test_validate_slug_special_chars() {
        assert!(validate_slug("hello world").is_err());
        assert!(validate_slug("hello_world").is_err());
    }
}
