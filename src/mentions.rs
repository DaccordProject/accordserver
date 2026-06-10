//! Parsing of `@`-mentions out of raw message content.
//!
//! Accord messages carry mentions inline as plain text (`@everyone`, `@here`,
//! `@username`) rather than as resolved `<@id>` tokens, mirroring the client's
//! own `RegExp(r'@(everyone|here)\b|@(\w+)')`. We parse them server-side so the
//! per-user mention counter (the red badge) is authoritative and survives
//! reconnects.

/// The mentions found in a message's content.
#[derive(Debug, Default, Clone)]
pub struct ParsedMentions {
    /// `@everyone` or `@here` was present.
    pub everyone: bool,
    /// Candidate `@username` handles, in order of appearance, de-duplicated
    /// case-insensitively. These still need resolving to member user IDs.
    pub usernames: Vec<String>,
}

/// Extracts `@everyone`/`@here` and `@username` tokens from [content].
///
/// A `@` only starts a mention at the beginning of the string or when preceded
/// by a non-alphanumeric byte, so the domain part of an email (`foo@bar`) is not
/// mistaken for a mention. Handle characters are `[A-Za-z0-9_]`, matching the
/// client's `\w`.
pub fn parse_mentions(content: &str) -> ParsedMentions {
    let bytes = content.as_bytes();
    let mut out = ParsedMentions::default();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'@' || (i > 0 && bytes[i - 1].is_ascii_alphanumeric()) {
            i += 1;
            continue;
        }
        let start = i + 1;
        let mut j = start;
        while j < bytes.len() && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'_') {
            j += 1;
        }
        if j == start {
            i += 1;
            continue;
        }
        // Safe slice: every byte in start..j is ASCII, so j is a char boundary.
        let word = &content[start..j];
        if word.eq_ignore_ascii_case("everyone") || word.eq_ignore_ascii_case("here") {
            out.everyone = true;
        } else if !out.usernames.iter().any(|u| u.eq_ignore_ascii_case(word)) {
            out.usernames.push(word.to_string());
        }
        i = j;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_everyone_and_here() {
        assert!(parse_mentions("hey @everyone!").everyone);
        assert!(parse_mentions("@here please").everyone);
        assert!(!parse_mentions("no pings here").everyone);
    }

    #[test]
    fn parses_usernames_deduped() {
        let m = parse_mentions("hi @alice and @Bob and @alice again");
        assert!(!m.everyone);
        assert_eq!(m.usernames, vec!["alice".to_string(), "Bob".to_string()]);
    }

    #[test]
    fn ignores_email_domains() {
        let m = parse_mentions("mail me at foo@bar.com");
        assert!(m.usernames.is_empty());
        assert!(!m.everyone);
    }

    #[test]
    fn handles_unicode_after_at() {
        // A `@` followed by a non-ASCII letter yields no ASCII handle.
        let m = parse_mentions("@é hello @world");
        assert_eq!(m.usernames, vec!["world".to_string()]);
    }
}
