//! Which URLs this program is willing to request.
//!
//! Every URL comes from a third party, so each one, and every redirect
//! target, is parsed with a real URI parser and checked: https only, no
//! credentials, port 443 or none, and a host on an allow-list.

use std::fmt;
use ureq::http::Uri;

pub const METADATA_HOST: &str = "api.pureapk.com";
pub const DOWNLOAD_DOMAINS: &[&str] = &["pureapk.com", "apkpure.com", "winudf.com"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostRule {
    /// Host must equal this name; no subdomains.
    Exactly(&'static str),
    /// Host must be one of these names or a label-boundary subdomain of one.
    WithinDomains(&'static [&'static str]),
}

pub const METADATA: HostRule = HostRule::Exactly(METADATA_HOST);
pub const DOWNLOAD: HostRule = HostRule::WithinDomains(DOWNLOAD_DOMAINS);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    Unparseable(String),
    NotHttps(String),
    Credentials(String),
    Port(String, String),
    Host(String, String),
    RelativeRedirect(String),
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Refusal::Unparseable(u) => write!(f, "refusing {u:?}: it does not parse as a URL"),
            Refusal::NotHttps(s) => write!(f, "refusing a URL with scheme {s:?}: only https is allowed"),
            Refusal::Credentials(u) => write!(f, "refusing {u}: it carries credentials in its authority"),
            Refusal::Port(u, p) => write!(f, "refusing {u}: port {p} is not 443"),
            Refusal::Host(u, h) => write!(f, "refusing {u}: host {h:?} is not on the allow-list"),
            Refusal::RelativeRedirect(l) => {
                write!(f, "refusing redirect to {l:?}: only absolute https or origin-relative targets are followed")
            }
        }
    }
}

impl std::error::Error for Refusal {}

fn host_allowed(host: &str, rule: HostRule) -> bool {
    match rule {
        HostRule::Exactly(want) => host == want,
        HostRule::WithinDomains(domains) => domains.iter().any(|d| {
            host == *d
                || host
                    .strip_suffix(d)
                    .is_some_and(|prefix| prefix.len() > 1 && prefix.ends_with('.'))
        }),
    }
}

/// Check a URL against the policy; returns the parsed URI on success.
pub fn check(url: &str, rule: HostRule) -> Result<Uri, Refusal> {
    let uri: Uri = url.parse().map_err(|_| Refusal::Unparseable(url.to_owned()))?;
    let scheme = uri.scheme_str().ok_or_else(|| Refusal::Unparseable(url.to_owned()))?;
    if !scheme.eq_ignore_ascii_case("https") {
        return Err(Refusal::NotHttps(scheme.to_owned()));
    }
    let authority = uri.authority().ok_or_else(|| Refusal::Unparseable(url.to_owned()))?;
    if authority.as_str().contains('@') {
        return Err(Refusal::Credentials(url.to_owned()));
    }
    // An authority ending in ':' (empty port) is refused too.
    let explicit_port = authority.as_str().rsplit_once(':').filter(|(_, p)| !p.contains(']'));
    if let Some((_, p)) = explicit_port {
        if p != "443" {
            return Err(Refusal::Port(url.to_owned(), p.to_owned()));
        }
    }
    let mut host = authority.host().to_ascii_lowercase();
    if host.ends_with('.') {
        host.pop();
    }
    if host.is_empty() || !host_allowed(&host, rule) {
        return Err(Refusal::Host(url.to_owned(), host));
    }
    Ok(uri)
}

/// Resolve a redirect `Location` against the URI that answered with it.
/// The result still has to pass [`check`].
pub fn resolve_location(current: &Uri, location: &str) -> Result<String, Refusal> {
    let location = location.trim();
    if location.get(..8).is_some_and(|p| p.eq_ignore_ascii_case("https://")) {
        return Ok(location.to_owned());
    }
    if location.starts_with('/') {
        let authority = current.authority().ok_or_else(|| Refusal::Unparseable(current.to_string()))?;
        return Ok(format!("https://{}{}", authority.as_str(), location));
    }
    // "scheme:..." with any other scheme is refused as not https.
    if let Some((scheme, _)) = location.split_once(':') {
        let looks_like_scheme = scheme.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
            && scheme.chars().all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c));
        if looks_like_scheme {
            return Err(Refusal::NotHttps(scheme.to_owned()));
        }
    }
    Err(Refusal::RelativeRedirect(location.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_accepts() {
        for u in [
            "https://api.pureapk.com/x",
            "https://API.PUREAPK.COM/x",
            "https://api.pureapk.com./x",
            "https://api.pureapk.com:443/x",
            "HTTPS://api.pureapk.com/m/v3/cms/app_version?hl=en-US&package_name=com.roblox.client",
        ] {
            assert!(check(u, METADATA).is_ok(), "{u}");
        }
    }

    #[test]
    fn metadata_refuses() {
        assert!(matches!(check("https://cdn.api.pureapk.com/x", METADATA), Err(Refusal::Host(..))));
        assert!(matches!(check("https://download.winudf.com/a", METADATA), Err(Refusal::Host(..))));
        assert!(matches!(check("http://api.pureapk.com/x", METADATA), Err(Refusal::NotHttps(s)) if s == "http"));
        assert!(matches!(check("https://api.pureapk.com:8443/x", METADATA), Err(Refusal::Port(_, p)) if p == "8443"));
        assert!(matches!(check("https://api.pureapk.com:/x", METADATA), Err(Refusal::Port(..))));
        assert!(matches!(
            check("https://api.pureapk.com@evil.example/x", METADATA),
            Err(Refusal::Credentials(_))
        ));
        assert!(check("https://user:pw@api.pureapk.com/x", METADATA).is_err());
        assert!(check("not a url", METADATA).is_err());
        assert!(check("/relative", METADATA).is_err());
        assert!(check("ftp://api.pureapk.com/x", METADATA).is_err());
    }

    #[test]
    fn download_accepts() {
        for u in [
            "https://download.winudf.com/x.apk",
            "https://data.winudf.com/APK/abc?filename=Roblox.apk",
            "https://apkpure.com/x.apk",
            "https://download.pureapk.com/b/APK/abc",
            "https://winudf.com/x",
            "https://DATA.WINUDF.COM./x",
        ] {
            assert!(check(u, DOWNLOAD).is_ok(), "{u}");
        }
    }

    #[test]
    fn download_refuses() {
        for u in [
            "https://evilapkpure.com/x.apk",
            "https://apkpure.com.evil.example/x.apk",
            "https://d.cdnpure.com/b/APK/x",
            "https://evilwinudf.com/x",
            "https://winudf.com.evil.example/x",
            "https://.winudf.com/x",
            "http://data.winudf.com/x",
            "https://data.winudf.com:8443/x",
            "https://data.winudf.com@evil.example/x",
            "https://[::1]/x",
        ] {
            assert!(check(u, DOWNLOAD).is_err(), "{u}");
        }
    }

    #[test]
    fn redirects() {
        let cur: Uri = "https://good.example/a/b".parse().unwrap();
        assert_eq!(resolve_location(&cur, "/c/d").unwrap(), "https://good.example/c/d");
        assert_eq!(
            resolve_location(&cur, "https://data.winudf.com/x").unwrap(),
            "https://data.winudf.com/x"
        );
        assert!(matches!(resolve_location(&cur, "http://evil.example/x.apk"), Err(Refusal::NotHttps(s)) if s == "http"));
        assert!(matches!(resolve_location(&cur, "updates/latest.json"), Err(Refusal::RelativeRedirect(_))));
        assert!(matches!(resolve_location(&cur, "javascript:alert(1)"), Err(Refusal::NotHttps(_))));
        // Protocol-relative stays on the current host and is then re-checked.
        let r = resolve_location(&cur, "//evil.example/x").unwrap();
        assert_eq!(r, "https://good.example//evil.example/x");
        // A resolved target that leaves the allow-list is still refused.
        let cur: Uri = "https://download.pureapk.com/b/APK/x".parse().unwrap();
        let r = resolve_location(&cur, "https://evil.example/y").unwrap();
        assert!(check(&r, DOWNLOAD).is_err());
    }
}
