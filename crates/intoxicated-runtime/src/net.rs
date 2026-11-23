//! A small HTTPS GET for text documents, such as Roblox's client settings.
//!
//! The rules, all checked before any connection is made and again on every
//! redirect hop:
//!
//! * the scheme is `https` (the agent is also built `https_only`),
//! * the URL carries no user info and names no port other than 443,
//! * a redirect stays on the host the request was sent to, and at most
//!   [`MAX_REDIRECTS`] are followed (ureq's own redirect handling is off, so
//!   no hop escapes the check),
//! * the body is read up to [`TEXT_CAP`] bytes and anything larger is
//!   refused rather than truncated.
//!
//! A non-2xx answer is an error that carries the first line of the body,
//! because that body is usually where the server says what went wrong.

use std::fmt;
use std::io::Read;
use std::time::Duration;
use ureq::http::Uri;

/// How long to wait for the TCP and TLS handshake.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Upper bound on the whole request, body included.
pub const TOTAL_TIMEOUT: Duration = Duration::from_secs(20);
/// Largest text body accepted. The client settings document is about 1.2 MB.
pub const TEXT_CAP: u64 = 8 << 20;
/// Redirect hops followed before giving up.
pub const MAX_REDIRECTS: usize = 5;

/// Why a fetch produced no text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FetchError {
    /// The URL (or a redirect target) broke the policy; nothing was sent to it.
    Refused { url: String, why: String },
    /// DNS, TLS, connection, timeout or a broken body.
    Transport { url: String, why: String },
    /// The server answered with a status outside 2xx.
    Status { url: String, status: u16, first_line: String },
    /// The body was larger than the cap.
    TooLarge { url: String, cap: u64 },
    /// The body was not UTF-8.
    NotText { url: String },
}

impl fmt::Display for FetchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FetchError::Refused { url, why } => write!(f, "refused to fetch {url}: {why}"),
            FetchError::Transport { url, why } => write!(f, "could not reach {url}: {why}"),
            FetchError::Status { url, status, first_line } if first_line.is_empty() => {
                write!(f, "{url} answered HTTP {status}")
            }
            FetchError::Status { url, status, first_line } => {
                write!(f, "{url} answered HTTP {status}: {first_line}")
            }
            FetchError::TooLarge { url, cap } => {
                write!(f, "{url} sent more than {cap} bytes; refused")
            }
            FetchError::NotText { url } => write!(f, "{url} sent a body that is not UTF-8"),
        }
    }
}

impl std::error::Error for FetchError {}

/// The User-Agent header: the program's real name and version.
pub fn user_agent() -> String {
    format!("Intoxicated/{} (FreeBSD)", env!("CARGO_PKG_VERSION"))
}

/// Check `url` against the policy and return its lowercased host.
///
/// `same_host`, when given, is the host the request started on; a URL naming
/// any other host is refused.
pub fn check(url: &str, same_host: Option<&str>) -> Result<String, FetchError> {
    let refuse = |why: &str| FetchError::Refused { url: url.to_owned(), why: why.to_owned() };
    let uri: Uri = url.parse().map_err(|_| refuse("not a URL"))?;
    match uri.scheme_str() {
        Some(s) if s.eq_ignore_ascii_case("https") => {}
        Some(s) => return Err(refuse(&format!("scheme {s} is not https"))),
        None => return Err(refuse("no scheme; only absolute https URLs are fetched")),
    }
    let authority = uri.authority().ok_or_else(|| refuse("no host"))?;
    if authority.as_str().contains('@') {
        return Err(refuse("the URL carries credentials"));
    }
    if let Some(port) = uri.port_u16() {
        if port != 443 {
            return Err(refuse(&format!("port {port} is not 443")));
        }
    }
    let host = authority.host().trim_end_matches('.').to_ascii_lowercase();
    if host.is_empty() {
        return Err(refuse("no host"));
    }
    if let Some(start) = same_host {
        if host != start {
            return Err(refuse(&format!("redirect left {start} for {host}")));
        }
    }
    Ok(host)
}

/// Turn a `Location` header into an absolute URL. Only absolute URLs and
/// path-absolute references (`/x`) are accepted; anything else is refused
/// rather than resolved by guesswork.
fn next_hop(current: &str, location: &str) -> Result<String, FetchError> {
    if location.contains("://") {
        return Ok(location.to_owned());
    }
    if location.starts_with('/') && !location.starts_with("//") {
        let uri: Uri = current.parse().map_err(|_| FetchError::Refused {
            url: current.to_owned(),
            why: "not a URL".to_owned(),
        })?;
        let authority = uri.authority().map(|a| a.as_str()).unwrap_or_default();
        return Ok(format!("https://{authority}{location}"));
    }
    Err(FetchError::Refused {
        url: current.to_owned(),
        why: format!("redirect target {location:?} is not absolute"),
    })
}

/// Read at most `cap` bytes of `body` as UTF-8, refusing anything longer.
pub fn read_capped<R: Read>(body: R, cap: u64, url: &str) -> Result<String, FetchError> {
    let mut bytes = Vec::new();
    body.take(cap + 1).read_to_end(&mut bytes).map_err(|e| FetchError::Transport {
        url: url.to_owned(),
        why: e.to_string(),
    })?;
    if bytes.len() as u64 > cap {
        return Err(FetchError::TooLarge { url: url.to_owned(), cap });
    }
    String::from_utf8(bytes).map_err(|_| FetchError::NotText { url: url.to_owned() })
}

fn first_line(text: &str) -> String {
    let line = text.lines().next().unwrap_or_default().trim();
    line.chars().take(200).collect()
}

/// GET `url` over HTTPS and return the body as text.
pub fn get_text(url: &str) -> Result<String, FetchError> {
    let host = check(url, None)?;
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .https_only(true)
        .max_redirects(0)
        .http_status_as_error(false)
        .timeout_connect(Some(CONNECT_TIMEOUT))
        .timeout_global(Some(TOTAL_TIMEOUT))
        .user_agent(user_agent())
        .build()
        .into();

    let mut current = url.to_owned();
    for _ in 0..=MAX_REDIRECTS {
        check(&current, Some(&host))?;
        let response = agent.get(&current).call().map_err(|e| FetchError::Transport {
            url: current.clone(),
            why: e.to_string(),
        })?;
        let status = response.status().as_u16();
        if matches!(status, 301 | 302 | 303 | 307 | 308) {
            let location = response
                .headers()
                .get("location")
                .and_then(|v| v.to_str().ok())
                .ok_or_else(|| FetchError::Refused {
                    url: current.clone(),
                    why: format!("HTTP {status} without a usable Location"),
                })?
                .to_owned();
            current = next_hop(&current, &location)?;
            continue;
        }
        let body = read_capped(response.into_body().into_reader(), TEXT_CAP, &current);
        if !(200..300).contains(&status) {
            let first_line = match &body {
                Ok(text) => first_line(text),
                Err(_) => String::new(),
            };
            return Err(FetchError::Status { url: current, status, first_line });
        }
        return body;
    }
    Err(FetchError::Refused {
        url: url.to_owned(),
        why: format!("more than {MAX_REDIRECTS} redirects"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refused(r: Result<impl fmt::Debug, FetchError>) -> bool {
        matches!(r, Err(FetchError::Refused { .. }))
    }

    #[test]
    fn plain_http_is_refused_before_any_connection() {
        let url = "http://clientsettingscdn.roblox.com/v2/settings/application/AndroidApp";
        assert!(refused(check(url, None)));
        // get_text checks before building a request, so this needs no network.
        assert!(refused(get_text(url)));
        assert!(refused(get_text("HTTP://example.com/")));
        assert!(refused(get_text("ftp://example.com/x")));
        assert!(refused(get_text("/relative/path")));
    }

    #[test]
    fn https_on_the_default_port_is_allowed() {
        assert_eq!(
            check("https://ClientSettingsCDN.roblox.com/v2/x", None).unwrap(),
            "clientsettingscdn.roblox.com"
        );
        assert!(check("https://clientsettingscdn.roblox.com:443/v2/x", None).is_ok());
    }

    #[test]
    fn credentials_and_odd_ports_are_refused() {
        assert!(refused(check("https://roblox.com@evil.example/", None)));
        assert!(refused(check("https://user:pw@roblox.com/", None)));
        assert!(refused(check("https://roblox.com:8443/", None)));
    }

    #[test]
    fn a_redirect_must_stay_on_the_starting_host() {
        let start = Some("clientsettingscdn.roblox.com");
        assert!(check("https://clientsettingscdn.roblox.com/other", start).is_ok());
        assert!(refused(check("https://evil.example/", start)));
        assert!(refused(check("https://cdn.clientsettingscdn.roblox.com/", start)));
        assert!(refused(check("http://clientsettingscdn.roblox.com/", start)));
    }

    #[test]
    fn redirect_targets_resolve_only_when_unambiguous() {
        let cur = "https://a.example/x/y";
        assert_eq!(next_hop(cur, "/z?q=1").unwrap(), "https://a.example/z?q=1");
        assert_eq!(next_hop(cur, "https://b.example/").unwrap(), "https://b.example/");
        assert!(refused(next_hop(cur, "z")));
        assert!(refused(next_hop(cur, "//b.example/z")));
        // An absolute http target is passed on, and then refused by `check`.
        let hop = next_hop(cur, "http://a.example/").unwrap();
        assert!(refused(check(&hop, Some("a.example"))));
    }

    #[test]
    fn an_oversized_body_is_refused_not_truncated() {
        let cap = 16;
        assert_eq!(read_capped(&[b'a'; 16][..], cap, "u").unwrap(), "a".repeat(16));
        assert_eq!(
            read_capped(&[b'a'; 17][..], cap, "u"),
            Err(FetchError::TooLarge { url: "u".into(), cap })
        );
        // A body far beyond the cap is not read past cap + 1 bytes.
        let mut source = std::io::repeat(b'x').take(TEXT_CAP * 4);
        assert!(matches!(
            read_capped(&mut source, TEXT_CAP, "u"),
            Err(FetchError::TooLarge { .. })
        ));
        assert_eq!(source.limit(), TEXT_CAP * 4 - (TEXT_CAP + 1));
    }

    #[test]
    fn a_body_that_is_not_utf8_is_refused() {
        assert_eq!(
            read_capped(&[0xff, 0xfe][..], 16, "u"),
            Err(FetchError::NotText { url: "u".into() })
        );
    }

    #[test]
    fn errors_read_as_sentences() {
        let e = FetchError::Status { url: "https://a/".into(), status: 500, first_line: "oops".into() };
        assert_eq!(e.to_string(), "https://a/ answered HTTP 500: oops");
        assert_eq!(first_line("one\ntwo"), "one");
    }
}
