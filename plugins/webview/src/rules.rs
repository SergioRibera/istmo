//! Navigation rule evaluation, the reference every backend follows.
//!
//! URLs are normalised with the WHATWG URL parser (the one the engines
//! use): scheme and host lowercase, internationalised hosts in ASCII
//! (punycode), IPv4 literals in dotted-decimal (`0x7f.1` →
//! `127.0.0.1`), dot segments resolved, `\` read as `/` in special
//! schemes. On top of that:
//!
//! - URLs carrying credentials (`https://good.example@evil.example`)
//!   never match a rule and are denied;
//! - a trailing dot on the host is ignored (`example.com.`);
//! - a pattern without a port matches only the scheme's default port;
//! - `*.example.com` matches strict subdomains only (`a.example.com`,
//!   not `example.com`, `xexample.com` or `example.com.evil`);
//! - `path_prefix` matches on segment boundaries (`/cb` matches `/cb`
//!   and `/cb/x`, not `/cb-x` or `/cb%2Fx`);
//! - `javascript:`, `intent:`, `data:` and (unless allowed) `file:`
//!   top-level navigations are always denied.
//!
//! `tests/rule_vectors.tsv` pins these semantics; the Kotlin and Swift
//! backends run the same vectors.

use url::Url;

use crate::{NavigationRules, RuleAction, UrlPattern, WebViewError};

/// Schemes no rule can allow for a top-level navigation.
const BLOCKED_SCHEMES: &[&str] = &["javascript", "intent", "data", "vbscript"];

/// A [`UrlPattern`] parsed once.
#[derive(Debug, Clone, PartialEq, Eq)]
struct CompiledPattern {
    /// `None` = any scheme.
    scheme: Option<String>,
    host: HostPattern,
    port: Option<u16>,
    /// Normalised, without a trailing `/` (except the root `/`).
    path_prefix: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum HostPattern {
    Any,
    Exact(String),
    /// Strict subdomains of the stored domain.
    Subdomains(String),
}

/// [`NavigationRules`] compiled for repeated evaluation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompiledRules {
    default_action: RuleAction,
    rules: Vec<(CompiledPattern, RuleAction)>,
}

/// Outcome of [`CompiledRules::evaluate`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verdict {
    /// What to do with the navigation.
    pub action: RuleAction,
    /// Normalised URL, `None` when it did not parse.
    pub url: Option<Url>,
}

impl CompiledRules {
    /// Validate and compile `rules`.
    ///
    /// # Errors
    /// [`WebViewError::InvalidArgument`] naming the first malformed
    /// pattern.
    pub fn new(rules: &NavigationRules) -> Result<Self, WebViewError> {
        let compiled = rules
            .rules
            .iter()
            .map(|rule| Ok((CompiledPattern::new(&rule.pattern)?, rule.action)))
            .collect::<Result<_, WebViewError>>()?;
        Ok(Self {
            default_action: rules.default_action,
            rules: compiled,
        })
    }

    /// Action for a top-level navigation to `raw`. `file_access`
    /// mirrors [`WebViewSettings::file_access`](crate::WebViewSettings::file_access).
    #[must_use]
    pub fn evaluate(&self, raw: &str, file_access: bool) -> Verdict {
        let Ok(url) = Url::parse(raw) else {
            return Verdict {
                action: RuleAction::Deny,
                url: None,
            };
        };
        let action = self.action_for(&url, file_access);
        Verdict {
            action,
            url: Some(url),
        }
    }

    fn action_for(&self, url: &Url, file_access: bool) -> RuleAction {
        let scheme = url.scheme();
        if BLOCKED_SCHEMES.contains(&scheme)
            || (scheme == "file" && !file_access)
            || !url.username().is_empty()
            || url.password().is_some()
        {
            return RuleAction::Deny;
        }
        self.rules
            .iter()
            .find(|(pattern, _)| pattern.matches(url))
            .map_or(self.default_action, |(_, action)| *action)
    }
}

impl CompiledPattern {
    fn new(pattern: &UrlPattern) -> Result<Self, WebViewError> {
        let invalid = |why: &str| {
            WebViewError::InvalidArgument(format!(
                "url pattern `{}://{}{}`: {why}",
                pattern.scheme, pattern.host, pattern.path_prefix
            ))
        };
        let scheme = match pattern.scheme.as_str() {
            "*" => None,
            raw => {
                let lower = raw.to_ascii_lowercase();
                let valid = lower.chars().next().is_some_and(|c| c.is_ascii_lowercase())
                    && lower
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'));
                if !valid {
                    return Err(invalid("bad scheme"));
                }
                Some(lower)
            }
        };
        // Parse through the same URL parser so the pattern normalises
        // exactly like the URLs it is compared with.
        let probe_scheme = scheme.as_deref().unwrap_or("https");
        let host = match pattern.host.as_str() {
            "*" => HostPattern::Any,
            raw => {
                let (wildcard, domain) = raw
                    .strip_prefix("*.")
                    .map_or((false, raw), |rest| (true, rest));
                if domain.is_empty() || domain.contains(['*', '/', '@', '?', '#']) {
                    return Err(invalid("bad host"));
                }
                let probe = Url::parse(&format!("{probe_scheme}://{domain}/"))
                    .map_err(|_| invalid("bad host"))?;
                let normalised = probe
                    .host_str()
                    .map(|h| h.trim_end_matches('.').to_owned())
                    .filter(|h| !h.is_empty())
                    .ok_or_else(|| invalid("bad host"))?;
                if wildcard {
                    HostPattern::Subdomains(normalised)
                } else {
                    HostPattern::Exact(normalised)
                }
            }
        };
        let path_prefix = if pattern.path_prefix.is_empty() {
            String::new()
        } else {
            if !pattern.path_prefix.starts_with('/') {
                return Err(invalid("path_prefix must start with `/`"));
            }
            let probe = Url::parse(&format!("https://h{}", pattern.path_prefix))
                .map_err(|_| invalid("bad path_prefix"))?;
            let path = probe.path();
            if path.len() > 1 {
                path.trim_end_matches('/').to_owned()
            } else {
                path.to_owned()
            }
        };
        Ok(Self {
            scheme,
            host,
            port: pattern.port,
            path_prefix,
        })
    }

    fn matches(&self, url: &Url) -> bool {
        if self.scheme.as_deref().is_some_and(|s| s != url.scheme()) {
            return false;
        }
        let host = url.host_str().map(|h| h.trim_end_matches('.'));
        let host_ok = match (&self.host, host) {
            (HostPattern::Any, _) => true,
            (HostPattern::Exact(want), Some(got)) => want == got,
            (HostPattern::Subdomains(domain), Some(got)) => got
                .strip_suffix(domain.as_str())
                .is_some_and(|rest| rest.len() > 1 && rest.ends_with('.')),
            (_, None) => false,
        };
        if !host_ok {
            return false;
        }
        // No port in the pattern means the default port only: an explicit
        // non-default port is a different service.
        let port_ok = self.port.map_or_else(
            || url.port().is_none(),
            |want| url.port_or_known_default() == Some(want),
        );
        port_ok && path_matches(&self.path_prefix, url.path())
    }
}

fn path_matches(prefix: &str, path: &str) -> bool {
    if prefix.is_empty() || prefix == "/" {
        return true;
    }
    path.strip_prefix(prefix)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
}

/// ASCII serialisation of `url`'s origin (`https://example.com:8443`).
///
/// `None` for opaque origins (`data:`, `blob:` of an opaque origin,
/// `about:`, sandboxed documents), which never match an allowed origin.
#[must_use]
pub fn origin_of(url: &Url) -> Option<String> {
    let origin = url.origin();
    if origin.is_tuple() {
        return Some(origin.ascii_serialization());
    }
    // WHATWG gives custom schemes opaque origins, but engines key the
    // documents an app scheme handler serves by `scheme://host[:port]`;
    // so does every check here.
    if matches!(
        url.scheme(),
        "file" | "data" | "blob" | "about" | "javascript"
    ) {
        return None;
    }
    let host = url
        .host_str()
        .filter(|h| !h.is_empty())?
        .to_ascii_lowercase();
    let scheme = url.scheme();
    Some(url.port().map_or_else(
        || format!("{scheme}://{host}"),
        |port| format!("{scheme}://{host}:{port}"),
    ))
}

/// Normalise an origin written by the app (`HTTPS://Example.COM:443`
/// → `https://example.com`).
///
/// # Errors
/// [`WebViewError::InvalidArgument`] when `raw` is not a tuple origin
/// or carries a path, query, fragment or credentials.
pub fn normalize_origin(raw: &str) -> Result<String, WebViewError> {
    let invalid = || WebViewError::InvalidArgument(format!("origin `{raw}`"));
    let url = Url::parse(raw).map_err(|_| invalid())?;
    let bare = matches!(url.path(), "" | "/")
        && url.query().is_none()
        && url.fragment().is_none()
        && url.username().is_empty()
        && url.password().is_none();
    if !bare {
        return Err(invalid());
    }
    origin_of(&url).ok_or_else(invalid)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::NavigationRule;

    fn rules(patterns: &[(&str, &str, Option<u16>, &str, RuleAction)]) -> CompiledRules {
        CompiledRules::new(&NavigationRules {
            default_action: RuleAction::Deny,
            rules: patterns
                .iter()
                .map(|&(scheme, host, port, path, action)| NavigationRule {
                    pattern: UrlPattern {
                        scheme: scheme.into(),
                        host: host.into(),
                        port,
                        path_prefix: path.into(),
                    },
                    action,
                })
                .collect(),
        })
        .unwrap()
    }

    #[test]
    fn first_matching_rule_wins() {
        let r = rules(&[
            (
                "https",
                "login.example.com",
                None,
                "/cb",
                RuleAction::Intercept,
            ),
            ("https", "*.example.com", None, "", RuleAction::Allow),
        ]);
        assert_eq!(
            r.evaluate("https://login.example.com/cb?code=1", false)
                .action,
            RuleAction::Intercept
        );
        assert_eq!(
            r.evaluate("https://login.example.com/other", false).action,
            RuleAction::Allow
        );
        assert_eq!(
            r.evaluate("https://example.org/", false).action,
            RuleAction::Deny
        );
    }

    #[test]
    fn blocked_schemes_and_credentials_are_denied() {
        let r = rules(&[("*", "*", None, "", RuleAction::Allow)]);
        for url in [
            "javascript:alert(1)",
            "data:text/html,hi",
            "intent://x#Intent;end",
            "file:///etc/passwd",
            "https://good.example@evil.example/",
            "https://:pw@example.com/",
            "not a url",
        ] {
            assert_eq!(r.evaluate(url, false).action, RuleAction::Deny, "{url}");
        }
        assert_eq!(r.evaluate("file:///tmp/x", true).action, RuleAction::Allow);
    }

    #[test]
    fn rejects_malformed_patterns() {
        for (scheme, host, path) in [
            ("ht tp", "a.com", ""),
            ("https", "", ""),
            ("https", "a*.com", ""),
            ("https", "a.com", "nope"),
            ("https", "u@a.com", ""),
        ] {
            let built = CompiledRules::new(&NavigationRules {
                default_action: RuleAction::Deny,
                rules: vec![NavigationRule {
                    pattern: UrlPattern {
                        scheme: scheme.into(),
                        host: host.into(),
                        port: None,
                        path_prefix: path.into(),
                    },
                    action: RuleAction::Allow,
                }],
            });
            assert!(built.is_err(), "{scheme}://{host}{path}");
        }
    }

    #[test]
    fn origins_normalise_and_reject_opaque() {
        assert_eq!(
            normalize_origin("HTTPS://Example.COM:443").unwrap(),
            "https://example.com"
        );
        assert_eq!(
            normalize_origin("http://127.0.0.1:8080/").unwrap(),
            "http://127.0.0.1:8080"
        );
        for bad in [
            "data:text/plain,x",
            "https://a.com/path",
            "https://u@a.com",
            "nope",
        ] {
            assert!(normalize_origin(bad).is_err(), "{bad}");
        }
        let blob = Url::parse("about:blank").unwrap();
        assert_eq!(origin_of(&blob), None);
    }
}
