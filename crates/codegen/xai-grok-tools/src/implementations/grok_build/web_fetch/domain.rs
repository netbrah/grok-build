//! Domain allowlist matching with precomputed host → path-prefix lookup.
//!
//! Entries are parsed once at construction into a `HashMap<host, HostEntry>`
//! giving O(1) host lookup followed by a tiny linear scan over path prefixes
//! for that host. `*.domain` wildcard entries (apex-blvg-A) match any
//! subdomain of `domain` — never the apex itself (RFC 4592 wildcard
//! semantics) — in either the host-only or the path-scoped form.

use std::collections::HashMap;

use url::Url;

use crate::types::output::WebFetchOutput;

// ───────────────────────────────────────────────────────────────────────────
// Domain normalization
// ───────────────────────────────────────────────────────────────────────────

/// Canonical form for domain comparison: trim whitespace, strip trailing
/// slashes and dots, remove `www.` prefix, and lowercase.
pub fn normalize_domain(raw: &str) -> String {
    let s = raw.trim().trim_end_matches('/').trim_end_matches('.');
    let s = s.strip_prefix("www.").unwrap_or(s);
    s.to_lowercase()
}

// ───────────────────────────────────────────────────────────────────────────
// Precomputed host entry
// ───────────────────────────────────────────────────────────────────────────

/// What a single host is allowed to serve.
#[derive(Debug, Clone)]
enum HostEntry {
    /// Any path on this host is allowed (host-only entry).
    AnyPath,
    /// Only paths matching one of these prefixes are allowed.
    /// Each prefix is normalized (leading `/`, no trailing `/`, lowercased).
    PathPrefixes(Vec<String>),
}

/// Insert/merge one (host, optional path) entry into a precomputed map:
/// host-only entries allow any path and override existing prefixes; path
/// entries accumulate as prefixes (a root path means host-only).
fn upsert_entry(map: &mut HashMap<String, HostEntry>, key: &str, path: Option<&str>) {
    match path {
        None => {
            // Host-only → any path allowed. Overrides any existing prefixes.
            map.insert(key.to_owned(), HostEntry::AnyPath);
        }
        Some(raw_path) => {
            // Don't downgrade AnyPath to PathPrefixes if a host-only
            // entry was already inserted.
            if matches!(map.get(key), Some(HostEntry::AnyPath)) {
                return;
            }

            // Normalize path: ensure leading '/', strip trailing '/'.
            let prefix = raw_path.trim_end_matches('/');
            let prefix = if prefix.is_empty() || prefix == "/" {
                // Entry like "example.com/" → treat as host-only.
                map.insert(key.to_owned(), HostEntry::AnyPath);
                return;
            } else if prefix.starts_with('/') {
                prefix.to_owned()
            } else {
                format!("/{prefix}")
            };

            map.entry(key.to_owned())
                .and_modify(|e| {
                    if let HostEntry::PathPrefixes(v) = e
                        && !v.contains(&prefix)
                    {
                        v.push(prefix.clone());
                    }
                })
                .or_insert_with(|| HostEntry::PathPrefixes(vec![prefix]));
        }
    }
}

// ───────────────────────────────────────────────────────────────────────────
// DomainMatcher
// ───────────────────────────────────────────────────────────────────────────

/// Precomputed domain allowlist. Built once from the raw allowlist entries,
/// provides O(1) host lookup + small linear scan over path prefixes.
#[derive(Debug, Clone)]
pub struct DomainMatcher {
    entries: HashMap<String, HostEntry>,
    /// Wildcard suffixes: `*.example.com` is stored under `example.com`.
    /// A host matches when it is a proper subdomain of the suffix (the
    /// host ends with `.` + suffix) — the apex itself never matches
    /// (RFC 4592 wildcard semantics).
    wildcards: HashMap<String, HostEntry>,
}

impl DomainMatcher {
    /// Build from raw allowlist entries like `"docs.rs"`, `"vercel.com/docs"`,
    /// or the wildcard `"*.netapp.com"` (any subdomain of `netapp.com`).
    pub fn new(raw_entries: &[String]) -> Self {
        let mut entries: HashMap<String, HostEntry> = HashMap::new();
        let mut wildcards: HashMap<String, HostEntry> = HashMap::new();

        for raw in raw_entries {
            let normalized = normalize_domain(raw);
            if normalized.is_empty() {
                continue;
            }

            // Split on first '/' to separate host from optional path.
            let (host, path) = match normalized.find('/') {
                Some(i) => (normalized[..i].to_owned(), Some(&normalized[i..])),
                None => (normalized, None),
            };

            // Wildcard entry (`*.example.com`): any subdomain of `example.com`
            // matches — never the apex itself (RFC 4592 wildcard semantics).
            if let Some(suffix) = host.strip_prefix("*.") {
                upsert_entry(&mut wildcards, suffix, path);
                continue;
            }
            upsert_entry(&mut entries, &host, path);
        }

        Self { entries, wildcards }
    }

    /// Returns `None` if the URL is permitted, or `Some(WebFetchOutput::DomainNotAllowed)`
    /// if it should be blocked. When `entries` is empty, all fetches are
    /// blocked. An exact entry for the host always wins; otherwise a
    /// wildcard suffix (`*.example.com`) permits any subdomain of
    /// `example.com` — never the apex itself (RFC 4592 wildcard semantics).
    pub fn check(&self, url: &Url) -> Option<WebFetchOutput> {
        let Some(raw_host) = url.host_str() else {
            return Some(WebFetchOutput::DomainNotAllowed(String::new()));
        };
        let host = normalize_domain(raw_host);

        // Wildcards are tested on the host with only the trailing dot
        // stripped (no `www.` collapse), so `www.sub.example.com` matches
        // `*.example.com` while the apex `example.com` does not.
        let entry = self.entries.get(&host).or_else(|| {
            let raw = raw_host.trim().trim_end_matches('.').to_ascii_lowercase();
            self.wildcards
                .iter()
                .find(|(suffix, _)| raw.ends_with(&format!(".{suffix}")))
                .map(|(_, entry)| entry)
        });

        let Some(entry) = entry else {
            tracing::debug!(
                domain = %host,
                allowed_count = self.entries.len(),
                "web_fetch domain not in allowlist"
            );
            return Some(WebFetchOutput::DomainNotAllowed(host));
        };

        match entry {
            HostEntry::AnyPath => None,
            HostEntry::PathPrefixes(prefixes) => {
                let url_path = url.path().to_lowercase();
                if prefixes.iter().any(|prefix| {
                    url_path == *prefix
                        || (url_path.starts_with(prefix.as_str())
                            && url_path.as_bytes().get(prefix.len()) == Some(&b'/'))
                }) {
                    return None;
                }
                tracing::debug!(
                    domain = %host,
                    path = %url_path,
                    allowed_prefixes = ?prefixes,
                    "web_fetch path not in allowlist for domain"
                );
                Some(WebFetchOutput::DomainNotAllowed(host))
            }
        }
    }

    /// Host-level membership (apex-blvg-A allowlist-for-direct): `true` if
    /// the host is listed — an exact normalized-host entry (path scope, if
    /// any, ignored) or a `*.domain` wildcard suffix match — independent of
    /// the URL path.
    pub fn host_matches(&self, raw_host: &str) -> bool {
        if self.entries.contains_key(&normalize_domain(raw_host)) {
            return true;
        }
        let raw = raw_host.trim().trim_end_matches('.').to_ascii_lowercase();
        self.wildcards
            .keys()
            .any(|suffix| raw.ends_with(&format!(".{suffix}")))
    }
}

/// Extract and normalize the domain from a raw URL string.
///
/// Returns `None` for unparseable URLs or URLs with no host.
pub fn domain_from_url(raw_url: &str) -> Option<String> {
    Url::parse(raw_url)
        .ok()
        .and_then(|u| u.host_str().map(normalize_domain))
}

// ───────────────────────────────────────────────────────────────────────────
// Tests
// ───────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn url(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    // ── normalize_domain ─────────────────────────────────────────────────

    #[test]
    fn normalize_strips_www_and_trailing_dot() {
        assert_eq!(normalize_domain("www.Example.COM."), "example.com");
    }

    #[test]
    fn normalize_trims_whitespace() {
        assert_eq!(normalize_domain("  docs.rs  "), "docs.rs");
    }

    // ── Host-only entries ────────────────────────────────────────────────

    #[test]
    fn allows_listed_domain() {
        let m = DomainMatcher::new(&["docs.rs".into(), "Example.Com".into()]);
        assert!(m.check(&url("https://docs.rs/reqwest/latest")).is_none());
        assert!(m.check(&url("https://example.com/page")).is_none());
    }

    #[test]
    fn case_insensitive_host() {
        let m = DomainMatcher::new(&["Example.Com".into()]);
        assert!(m.check(&url("https://example.com/page")).is_none());
        assert!(m.check(&url("https://EXAMPLE.COM/page")).is_none());
    }

    #[test]
    fn rejects_unlisted_domain() {
        let m = DomainMatcher::new(&["docs.rs".into()]);
        let blocked = m.check(&url("https://evil.com/steal"));
        assert!(matches!(blocked, Some(WebFetchOutput::DomainNotAllowed(d)) if d == "evil.com"));
    }

    #[test]
    fn blocks_all_when_empty() {
        let m = DomainMatcher::new(&[]);
        assert!(m.check(&url("https://docs.python.org/3/")).is_some());
        assert!(m.check(&url("https://example.com/")).is_some());
    }

    #[test]
    fn www_prefix_stripped() {
        let m = DomainMatcher::new(&["react.dev".into()]);
        assert!(m.check(&url("https://www.react.dev/learn")).is_none());
    }

    #[test]
    fn trailing_dot_stripped() {
        let m = DomainMatcher::new(&["react.dev".into()]);
        assert!(m.check(&url("https://react.dev./learn")).is_none());
    }

    // ── Path-scoped entries ──────────────────────────────────────────────

    #[test]
    fn path_scoped_allows_matching_path() {
        let m = DomainMatcher::new(&["vercel.com/docs".into()]);
        assert!(m.check(&url("https://vercel.com/docs")).is_none());
        assert!(m.check(&url("https://vercel.com/docs/foo")).is_none());
        assert!(
            m.check(&url("https://vercel.com/docs/platform/edge-functions"))
                .is_none()
        );
    }

    #[test]
    fn path_scoped_blocks_non_matching_path() {
        let m = DomainMatcher::new(&["vercel.com/docs".into()]);
        let blocked = m.check(&url("https://vercel.com/api"));
        assert!(matches!(blocked, Some(WebFetchOutput::DomainNotAllowed(h)) if h == "vercel.com"));
        assert!(m.check(&url("https://vercel.com/")).is_some());
    }

    #[test]
    fn path_scoped_blocks_wrong_host() {
        let m = DomainMatcher::new(&["vercel.com/docs".into()]);
        let blocked = m.check(&url("https://netlify.com/docs"));
        assert!(matches!(blocked, Some(WebFetchOutput::DomainNotAllowed(h)) if h == "netlify.com"));
    }

    #[test]
    fn path_scoped_rejects_sibling_prefix() {
        let m = DomainMatcher::new(&["vercel.com/docs".into()]);

        // "/docs-internal" shares "/docs" prefix but is a sibling, not a child.
        let blocked = m.check(&url("https://vercel.com/docs-internal"));
        assert!(
            matches!(blocked, Some(WebFetchOutput::DomainNotAllowed(h)) if h == "vercel.com"),
            "expected /docs-internal to be blocked"
        );

        // "/documentation" — also a sibling.
        let blocked2 = m.check(&url("https://vercel.com/documentation"));
        assert!(
            matches!(blocked2, Some(WebFetchOutput::DomainNotAllowed(h)) if h == "vercel.com"),
            "expected /documentation to be blocked"
        );

        // Actual child path is still allowed.
        assert!(
            m.check(&url("https://vercel.com/docs/guide")).is_none(),
            "expected /docs/guide to be allowed"
        );
    }

    #[test]
    fn path_scoped_case_insensitive() {
        let m = DomainMatcher::new(&["Vercel.COM/Docs".into()]);
        assert!(m.check(&url("https://vercel.com/docs")).is_none());
        assert!(m.check(&url("https://VERCEL.COM/DOCS/foo")).is_none());
    }

    #[test]
    fn root_path_entry_allows_any_path() {
        // "example.com/" should behave like host-only.
        let m = DomainMatcher::new(&["example.com/".into()]);
        assert!(m.check(&url("https://example.com/")).is_none());
        assert!(m.check(&url("https://example.com/anything")).is_none());
    }

    // ── Multiple path prefixes per host ──────────────────────────────────

    #[test]
    fn multiple_path_prefixes_per_host() {
        let m = DomainMatcher::new(&["github.com/org-a".into(), "github.com/org-b".into()]);
        assert!(
            m.check(&url("https://github.com/org-a/project-one"))
                .is_none()
        );
        assert!(
            m.check(&url("https://github.com/org-b/project-two"))
                .is_none()
        );
        assert!(
            m.check(&url("https://github.com/evil-org/malware"))
                .is_some()
        );
    }

    #[test]
    fn host_only_overrides_path_prefixes() {
        // If both "github.com" (host-only) and "github.com/docs" exist,
        // host-only wins — any path is allowed.
        let m = DomainMatcher::new(&["github.com/docs".into(), "github.com".into()]);
        assert!(m.check(&url("https://github.com/anything")).is_none());
    }

    // ── Wildcard (`*.domain`) entries (apex-blvg-A) ─────────────────────

    #[test]
    fn wildcard_matches_subdomains_at_any_depth() {
        let m = DomainMatcher::new(&["*.netapp.com".into()]);
        assert!(m.check(&url("https://sub.netapp.com/page")).is_none());
        assert!(m.check(&url("https://a.b.netapp.com/page")).is_none());
        assert!(m.check(&url("https://www.sub.netapp.com/page")).is_none());
    }

    #[test]
    fn wildcard_does_not_match_apex_or_lookalike() {
        let m = DomainMatcher::new(&["*.netapp.com".into()]);
        // The apex is not a subdomain of itself (RFC 4592 wildcard
        // semantics).
        assert!(m.check(&url("https://netapp.com/page")).is_some());
        // A lookalike suffix shares no subdomain relation.
        assert!(m.check(&url("https://evilnetapp.com/page")).is_some());
        assert!(m.check(&url("https://sub.other.com/page")).is_some());
    }

    #[test]
    fn wildcard_normalizes_entry_case_and_dot() {
        let m = DomainMatcher::new(&["*.NETAPP.COM.".into()]);
        assert!(m.check(&url("https://Sub.NetApp.com/x")).is_none());
    }

    #[test]
    fn wildcard_can_be_path_scoped() {
        let m = DomainMatcher::new(&["*.netapp.com/docs".into()]);
        assert!(m.check(&url("https://sub.netapp.com/docs")).is_none());
        assert!(m.check(&url("https://sub.netapp.com/docs/page")).is_none());
        assert!(m.check(&url("https://sub.netapp.com/api")).is_some());
        // The apex stays excluded even under a path-scoped wildcard.
        assert!(m.check(&url("https://netapp.com/docs")).is_some());
    }

    #[test]
    fn wildcard_coexists_with_exact_entries() {
        // Exact entries keep their behavior alongside wildcards.
        let m = DomainMatcher::new(&["docs.rs".into(), "*.netapp.com".into()]);
        assert!(m.check(&url("https://docs.rs/x")).is_none());
        assert!(m.check(&url("https://sub.netapp.com/x")).is_none());
        assert!(m.check(&url("https://netapp.com/x")).is_some());
        assert!(m.check(&url("https://evil.com/x")).is_some());
    }

    // ── host_matches (apex-blvg-A allowlist-for-direct) ──────────────────

    #[test]
    fn host_matches_is_path_independent_and_wildcard_aware() {
        let m = DomainMatcher::new(&["netapp.com".into(), "*.corp.com".into()]);
        assert!(m.host_matches("netapp.com"));
        assert!(m.host_matches("www.netapp.com")); // exact, www-collapsed
        assert!(m.host_matches("sub.corp.com"));
        assert!(m.host_matches("a.b.corp.com"));
        assert!(!m.host_matches("corp.com")); // apex not a subdomain of itself
        assert!(!m.host_matches("evilcorp.com"));
        assert!(!m.host_matches("example.com"));
        // A path-scoped exact entry still counts at host level (apex-blvg-A
        // routing is host-granular).
        let m2 = DomainMatcher::new(&["vercel.com/docs".into()]);
        assert!(m2.host_matches("vercel.com"));
        assert!(!m2.host_matches("other.com"));
    }

    // ── Model URL variants ───────────────────────────────────────────────

    #[test]
    fn model_url_variants() {
        let m = DomainMatcher::new(&[
            "docs.python.org".into(),
            "developer.mozilla.org".into(),
            "react.dev".into(),
            "api.example.com".into(),
        ]);

        // Should match.
        assert!(m.check(&url("https://react.dev")).is_none());
        assert!(
            m.check(&url("https://docs.python.org/3/library/asyncio.html"))
                .is_none()
        );
        assert!(
            m.check(&url(
                "https://developer.mozilla.org/en-US/docs/Web/API/fetch?v=2#syntax"
            ))
            .is_none()
        );
        assert!(m.check(&url("https://react.dev/")).is_none());
        assert!(m.check(&url("https://React.Dev/learn")).is_none());
        assert!(m.check(&url("https://api.example.com/v1/users")).is_none());
        assert!(m.check(&url("https://react.dev:443/reference")).is_none());
        assert!(m.check(&url("https://www.react.dev/learn")).is_none());

        // Should NOT match.
        assert!(m.check(&url("https://example.com/page")).is_some());
        assert!(m.check(&url("https://evil.example.com/")).is_some());
        assert!(m.check(&url("https://react-dev.com/")).is_some());
        assert!(
            m.check(&url("https://stackoverflow.com/questions/123"))
                .is_some()
        );
        assert!(m.check(&url("https://93.184.216.34/page")).is_some());
    }

    // ── domain_from_url ─────────────────────────────────────────────────

    #[test]
    fn domain_from_url_extracts_and_normalizes() {
        assert_eq!(
            domain_from_url("https://docs.python.org/3/library/asyncio.html"),
            Some("docs.python.org".to_string())
        );
        assert_eq!(
            domain_from_url("https://www.React.Dev/learn"),
            Some("react.dev".to_string())
        );
    }

    #[test]
    fn domain_from_url_returns_none_for_garbage() {
        assert_eq!(domain_from_url("not a url"), None);
        assert_eq!(domain_from_url(""), None);
    }
}
