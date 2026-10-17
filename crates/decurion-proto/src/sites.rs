//! Which websites an agent's box may reach.
//!
//! A box has no route to the internet. Its only way out is the gate, a small
//! proxy next to it that lets through only the sites on its list. An entry
//! like `stripe.com` covers that site and every address under it, such as
//! `api.stripe.com`.

/// Sites every box may reach: Claude itself, and the package stores agents
/// install libraries from. The owner adds others per app.
pub const BUILT_IN: &[&str] = &[
    "api.anthropic.com",
    "registry.npmjs.org",
    "pypi.org",
    "files.pythonhosted.org",
    "crates.io",
    "index.crates.io",
    "static.crates.io",
];

/// At most this many extra sites per app.
pub const MAX_SITES: usize = 100;

/// Turns what the owner typed (`https://Stripe.com/docs`, `*.stripe.com`,
/// `stripe.com.`) into the plain name kept on the list (`stripe.com`).
/// Refuses IP addresses, single words such as `com` (which would cover a
/// whole ending), and anything that is not a valid site name.
pub fn normalize(input: &str) -> Option<String> {
    let mut s = input.trim().to_ascii_lowercase();
    if let Some(i) = s.find("://") {
        s.drain(..i + 3);
    }
    if let Some(i) = s.find(['/', '?', '#']) {
        s.truncate(i);
    }
    if let Some(i) = s.rfind('@') {
        s.drain(..=i);
    }
    if let Some(i) = s.find(':') {
        s.truncate(i);
    }
    let s = s.strip_prefix("*.").unwrap_or(&s);
    let s = s.strip_suffix('.').unwrap_or(s);
    is_site_name(s).then(|| s.to_owned())
}

/// A lower-case DNS name with at least two labels and a non-numeric ending,
/// so it can never be an IP address.
pub fn is_site_name(s: &str) -> bool {
    if s.is_empty() || s.len() > 253 {
        return false;
    }
    let labels: Vec<&str> = s.split('.').collect();
    let label_ok = |l: &&str| {
        !l.is_empty()
            && l.len() <= 63
            && !l.starts_with('-')
            && !l.ends_with('-')
            && l.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    };
    labels.len() >= 2
        && labels.iter().all(label_ok)
        && labels
            .last()
            .is_some_and(|t| t.bytes().any(|b| b.is_ascii_lowercase()))
}

/// Whether `host` is `site` or an address under it.
pub fn covers(site: &str, host: &str) -> bool {
    let host = host.strip_suffix('.').unwrap_or(host);
    host.eq_ignore_ascii_case(site)
        || (host.len() > site.len()
            && host.as_bytes()[host.len() - site.len() - 1] == b'.'
            && host[host.len() - site.len()..].eq_ignore_ascii_case(site))
}

/// The built-in sites plus an app's own, without repeats.
pub fn for_app(extra: &[String]) -> Vec<String> {
    let mut all: Vec<String> = BUILT_IN.iter().map(|s| s.to_string()).collect();
    for s in extra {
        if !all.contains(s) {
            all.push(s.clone());
        }
    }
    all
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owner_input_is_cleaned_up() {
        assert_eq!(
            normalize("https://Stripe.com/docs?x=1").as_deref(),
            Some("stripe.com")
        );
        assert_eq!(normalize("*.github.io").as_deref(), Some("github.io"));
        assert_eq!(normalize("example.com.").as_deref(), Some("example.com"));
        assert_eq!(
            normalize("http://user@api.example.com:8443").as_deref(),
            Some("api.example.com")
        );
        assert_eq!(normalize(" docs.rs ").as_deref(), Some("docs.rs"));
    }

    #[test]
    fn whole_endings_ip_addresses_and_junk_are_refused() {
        for bad in [
            "",
            "com",
            "*",
            "*.com",
            "10.0.0.1",
            "192.168.1.1:80",
            "[::1]",
            "exa mple.com",
            "-bad.com",
            "a..b",
            "ex_ample.com",
            "例え.jp",
        ] {
            assert_eq!(normalize(bad), None, "{bad}");
        }
    }

    #[test]
    fn a_site_covers_itself_and_addresses_under_it_only() {
        assert!(covers("stripe.com", "stripe.com"));
        assert!(covers("stripe.com", "api.stripe.com"));
        assert!(covers("stripe.com", "API.Stripe.com."));
        assert!(!covers("stripe.com", "evilstripe.com"));
        assert!(!covers("stripe.com", "stripe.com.evil.net"));
        assert!(!covers("api.stripe.com", "stripe.com"));
    }

    #[test]
    fn built_ins_come_first_without_repeats() {
        let all = for_app(&["pypi.org".into(), "stripe.com".into()]);
        assert_eq!(all.len(), BUILT_IN.len() + 1);
        assert_eq!(all[0], "api.anthropic.com");
        assert_eq!(all.last().map(String::as_str), Some("stripe.com"));
        assert!(BUILT_IN.iter().all(|s| normalize(s).as_deref() == Some(*s)));
    }
}
