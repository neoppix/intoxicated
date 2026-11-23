//! HTTP: the metadata request and archive downloads.
//!
//! The client is https-only with automatic redirects off; [`get`] walks
//! redirects itself so each hop is re-checked by [`crate::urlpolicy`].

use crate::urlpolicy::{self, HostRule};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::Path;
use std::time::Duration;
use ureq::http::Response;
use ureq::{Agent, Body};

pub const METADATA_URL: &str =
    "https://api.pureapk.com/m/v3/cms/app_version?hl=en-US&package_name=com.roblox.client";
pub const METADATA_CAP: usize = 4 << 20;
pub const DOWNLOAD_CAP: u64 = 1 << 30;
pub const MAX_REDIRECTS: usize = 5;
const ERROR_BODY_CAP: u64 = 64 << 10;
const STREAM_BUF: usize = 256 << 10;

fn agent(total: Duration) -> Agent {
    Agent::config_builder()
        .https_only(true)
        .max_redirects(0)
        .http_status_as_error(false)
        .timeout_connect(Some(Duration::from_secs(10)))
        .timeout_global(Some(total))
        .user_agent(crate::user_agent())
        .build()
        .into()
}

fn is_redirect(code: u16) -> bool {
    matches!(code, 301 | 302 | 303 | 307 | 308)
}

/// GET `url`, following up to five redirects by hand, each target checked
/// against `rule`. Returns the final URL and response, whatever its status.
pub fn get(
    agent: &Agent,
    url: &str,
    rule: HostRule,
    headers: &[(&str, &str)],
) -> Result<(String, Response<Body>), String> {
    let mut current = url.to_owned();
    for hop in 0..=MAX_REDIRECTS {
        let uri = urlpolicy::check(&current, rule).map_err(|e| e.to_string())?;
        let mut req = agent.get(&current);
        for (k, v) in headers {
            req = req.header(*k, *v);
        }
        let resp = req.call().map_err(|e| format!("could not reach {current}: {e}"))?;
        let code = resp.status().as_u16();
        if !is_redirect(code) {
            return Ok((current, resp));
        }
        if hop == MAX_REDIRECTS {
            break;
        }
        let location = resp
            .headers()
            .get("location")
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| format!("{current} answered HTTP {code} without a Location header"))?;
        current = urlpolicy::resolve_location(&uri, location).map_err(|e| e.to_string())?;
    }
    Err(format!("too many redirects (more than {MAX_REDIRECTS}) starting from {url}"))
}

/// "<url> answered HTTP <status>[: <first line of body>]"
fn status_error(url: &str, resp: Response<Body>) -> String {
    let code = resp.status().as_u16();
    let mut body = Vec::new();
    let _ = resp.into_body().into_reader().take(ERROR_BODY_CAP).read_to_end(&mut body);
    let text = String::from_utf8_lossy(&body);
    let line: String = text.lines().next().unwrap_or("").trim().chars().take(200).collect();
    if line.is_empty() {
        format!("{url} answered HTTP {code}")
    } else {
        format!("{url} answered HTTP {code}: {line}")
    }
}

/// The x86_64 version list body.
pub fn fetch_metadata() -> Result<Vec<u8>, String> {
    let agent = agent(Duration::from_secs(60));
    let headers = [("x-sv", "29"), ("x-abis", "x86_64"), ("x-cv", "3172501"), ("x-gp", "1")];
    let (url, resp) = get(&agent, METADATA_URL, urlpolicy::METADATA, &headers)?;
    if !resp.status().is_success() {
        return Err(status_error(&url, resp));
    }
    let mut body = Vec::new();
    resp.into_body()
        .into_reader()
        .take(METADATA_CAP as u64)
        .read_to_end(&mut body)
        .map_err(|e| format!("could not read the version list from {url}: {e}"))?;
    if body.len() >= METADATA_CAP {
        return Err(format!("{url} sent {METADATA_CAP} bytes or more, which is not a version list"));
    }
    Ok(body)
}

/// Facts about a finished download.
#[derive(Debug, Clone)]
pub struct Landed {
    pub bytes: u64,
    pub sha1: String,
}

/// Stream `url` into `partial` (created fresh), reporting progress through
/// `progress(done, total)` at most once per MiB. On error the partial file
/// is removed.
pub fn download(url: &str, partial: &Path, progress: &mut dyn FnMut(u64, Option<u64>)) -> Result<Landed, String> {
    let result = download_inner(url, partial, progress);
    if result.is_err() {
        let _ = fs::remove_file(partial);
    }
    result
}

fn download_inner(url: &str, partial: &Path, progress: &mut dyn FnMut(u64, Option<u64>)) -> Result<Landed, String> {
    use sha1::Digest;
    let agent = agent(Duration::from_secs(900));
    let (final_url, resp) = get(&agent, url, urlpolicy::DOWNLOAD, &[])?;
    if !resp.status().is_success() {
        return Err(status_error(&final_url, resp));
    }
    let declared = resp
        .headers()
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<u64>().ok());
    if let Some(n) = declared.filter(|&n| n > DOWNLOAD_CAP) {
        return Err(format!("{final_url} declares {n} bytes, more than the 1 GiB limit"));
    }
    let fs_err = |e: std::io::Error| format!("{}: {e}", partial.display());
    let mut out = fs::OpenOptions::new().write(true).create_new(true).open(partial).map_err(fs_err)?;
    let mut reader = resp.into_body().into_reader();
    let mut buf = vec![0u8; STREAM_BUF];
    let mut done: u64 = 0;
    let mut hasher = sha1::Sha1::new();
    let mut next_report = 1u64 << 20;
    let mut reported = 0u64;
    progress(0, declared);
    loop {
        let n = reader.read(&mut buf).map_err(|e| format!("download from {final_url} broke off: {e}"))?;
        if n == 0 {
            break;
        }
        done += n as u64;
        if done > DOWNLOAD_CAP {
            return Err(format!("{final_url} sent more than the 1 GiB limit"));
        }
        hasher.update(&buf[..n]);
        out.write_all(&buf[..n]).map_err(fs_err)?;
        if done >= next_report {
            progress(done, declared);
            reported = done;
            next_report = done + (1 << 20);
        }
    }
    if reported != done {
        progress(done, declared);
    }
    if done == 0 {
        return Err(format!("{final_url} sent an empty body"));
    }
    out.sync_all().map_err(fs_err)?;
    drop(out);
    let mut head = [0u8; 4];
    File::open(partial).and_then(|mut f| f.read_exact(&mut head)).map_err(fs_err)?;
    if !crate::zipdir::has_zip_magic(&head) {
        return Err(format!("{final_url} did not send a ZIP file, so not an APK (it starts with {head:02x?})"));
    }
    Ok(Landed { bytes: done, sha1: crate::hex(&hasher.finalize()) })
}
