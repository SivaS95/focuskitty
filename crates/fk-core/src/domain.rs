//! URL → host matching.
//!
//! Deliberately dependency-free and deliberately not a Public Suffix List. A
//! rule of `youtube.com` should match `www.youtube.com` and `m.youtube.com`;
//! that is the whole requirement, and PSL machinery would only matter for
//! rules written against a public suffix itself (`co.uk`), which no one does.

/// Extract the lowercased host from a URL, without scheme, userinfo, port or path.
pub fn host_of(url: &str) -> Option<String> {
    let rest = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
    let authority = rest.split(['/', '?', '#']).next()?;
    let authority = authority.rsplit_once('@').map(|(_, h)| h).unwrap_or(authority);

    // IPv6 literals are bracketed and contain colons, so the port strip differs.
    let host = if let Some(stripped) = authority.strip_prefix('[') {
        match stripped.split_once(']') {
            Some((inner, _)) => format!("[{inner}]"),
            None => authority.to_string(),
        }
    } else {
        authority
            .split_once(':')
            .map(|(h, _)| h.to_string())
            .unwrap_or_else(|| authority.to_string())
    };

    let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
    (!host.is_empty()).then_some(host)
}

fn strip_www(h: &str) -> &str {
    h.strip_prefix("www.").unwrap_or(h)
}

/// Does `url` belong to `rule_domain` (or any of its subdomains)?
pub fn matches_domain(url: &str, rule_domain: &str) -> bool {
    let Some(host) = host_of(url) else {
        return false;
    };
    let rule_lower = rule_domain.trim().trim_end_matches('.').to_ascii_lowercase();
    let rule = strip_www(&rule_lower);
    if rule.is_empty() {
        return false;
    }
    let host = strip_www(&host);
    host == rule || host.ends_with(&format!(".{rule}"))
}

/// The key a site rule is tracked under: its own domain, normalised.
pub fn canonical(rule_domain: &str) -> String {
    normalize(rule_domain)
}

/// Turn whatever the user typed into a bare host.
///
/// People paste URLs. A rule stored as `https://www.youtube.com/` can never
/// match anything, because matching compares against the *host* of the page —
/// so it silently counts nothing forever. Normalising on the way in is the
/// only place this can be fixed once.
pub fn normalize(input: &str) -> String {
    let t = input.trim();
    if t.is_empty() {
        return String::new();
    }
    // A pasted URL, or something with a path or port, goes through the parser.
    let host = if t.contains("://") || t.contains('/') || t.contains(':') {
        host_of(t).unwrap_or_default()
    } else {
        t.trim_end_matches('.').to_ascii_lowercase()
    };
    strip_www(&host).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_hosts() {
        assert_eq!(host_of("https://www.youtube.com/watch?v=x").as_deref(), Some("www.youtube.com"));
        assert_eq!(host_of("http://localhost:3000/a").as_deref(), Some("localhost"));
        assert_eq!(host_of("https://user:pw@Example.COM:8443/p").as_deref(), Some("example.com"));
        assert_eq!(host_of("https://[::1]:8080/x").as_deref(), Some("[::1]"));
        assert_eq!(host_of(""), None);

        // Schemeless URIs degrade to their scheme ("about:blank" -> "about").
        // Harmless: no rule domain can equal a bare scheme, so it matches nothing.
        assert_eq!(host_of("about:blank").as_deref(), Some("about"));
        assert!(!matches_domain("about:blank", "youtube.com"));
    }

    #[test]
    fn matches_subdomains_but_not_lookalikes() {
        assert!(matches_domain("https://www.youtube.com/feed", "youtube.com"));
        assert!(matches_domain("https://m.youtube.com/", "youtube.com"));
        assert!(matches_domain("https://youtube.com/", "www.youtube.com"));
        assert!(matches_domain("https://YouTube.com/", "youtube.com"));

        // The important negative: a suffix match must not be a substring match.
        assert!(!matches_domain("https://notyoutube.com/", "youtube.com"));
        assert!(!matches_domain("https://youtube.com.evil.net/", "youtube.com"));
        assert!(!matches_domain("https://example.com/?q=youtube.com", "youtube.com"));
    }

    #[test]
    fn pasted_urls_become_bare_hosts() {
        // People paste the address bar. A rule holding a URL matches nothing.
        for input in [
            "https://www.youtube.com/",
            "http://youtube.com",
            "www.youtube.com",
            "  YouTube.com  ",
            "youtube.com/watch?v=x",
        ] {
            assert_eq!(normalize(input), "youtube.com", "input was {input:?}");
        }
        assert_eq!(normalize(""), "");
        // And a normalised rule actually matches a real page.
        assert!(matches_domain("https://www.youtube.com/feed", &normalize("https://www.youtube.com/")));
    }

    #[test]
    fn empty_rule_never_matches() {
        assert!(!matches_domain("https://youtube.com/", ""));
        assert!(!matches_domain("https://youtube.com/", "  "));
    }
}
