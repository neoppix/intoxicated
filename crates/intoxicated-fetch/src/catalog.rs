//! APKPure's app_version response, decoded into releases.
//!
//! Path to each record: top field 1 (envelope), field 7, repeated field 2
//! (one per version, newest first), field 3, field 2 (the record).

use crate::wire::{decode, Field, Message, WireError};

pub const PACKAGE: &str = "com.roblox.client";
pub const MAX_CANDIDATES: usize = 4;

/// One downloadable artefact as the mirror describes it. Nothing in here is
/// trusted; the size and SHA-1 are only used to fail early with a clearer
/// message than the signature check would give.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Asset {
    pub sha1: Option<String>,
    pub size: Option<u64>,
    pub kind: Option<String>,
    pub url: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Release {
    pub package: Option<String>,
    /// Android versionCode; `None` if absent, not decimal, or zero.
    pub version_code: Option<u64>,
    pub version_name: Option<String>,
    /// Mirror's claim about the signing certificate. Display only.
    pub cert_sha1: Option<String>,
    pub developer: Option<String>,
    pub asset: Option<Asset>,
}

fn sub<'a>(fields: &[Field<'a>], number: u32) -> Result<Option<Vec<Field<'a>>>, WireError> {
    fields.bytes_of(number).map(decode).transpose()
}

fn parse_asset(fields: &[Field<'_>]) -> Asset {
    Asset {
        sha1: fields.string_of(3),
        size: fields.varint_of(4),
        kind: fields.string_of(8),
        url: fields.string_of(9),
    }
}

fn parse_release(fields: &[Field<'_>]) -> Result<Release, WireError> {
    let version_code = fields
        .string_of(5)
        .and_then(|s| s.trim().parse::<u64>().ok())
        .filter(|&c| c != 0);
    Ok(Release {
        package: fields.string_of(4),
        version_code,
        version_name: fields.string_of(6),
        cert_sha1: fields.string_of(7),
        developer: fields.string_of(13),
        asset: sub(fields, 24)?.map(|a| parse_asset(&a)),
    })
}

/// Decode the response body into its releases, newest first.
pub fn parse_version_list(body: &[u8]) -> Result<Vec<Release>, WireError> {
    let top = decode(body)?;
    let Some(envelope) = sub(&top, 1)? else { return Ok(Vec::new()) };
    let Some(list) = sub(&envelope, 7)? else { return Ok(Vec::new()) };
    let mut out = Vec::new();
    for entry in list.all_bytes_of(2) {
        let entry = decode(entry)?;
        let Some(holder) = sub(&entry, 3)? else { continue };
        let Some(record) = sub(&holder, 2)? else { continue };
        out.push(parse_release(&record)?);
    }
    Ok(out)
}

/// What to download: the newest version and the archive URLs for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub version_name: String,
    pub version_code: u64,
    /// The newest record, for its asset hints and display fields.
    pub newest: Release,
    /// Every distinct https URL listed for that version, in list order,
    /// each paired with the asset it came from.
    pub downloads: Vec<(String, Asset)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanError {
    NoVersions,
    BadNewest,
    NoDownload(String),
    TooManyDownloads(String, usize),
}

impl std::fmt::Display for PlanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PlanError::NoVersions => write!(
                f,
                "the version list carries no versions; either APKPure changed its service or the endpoint does not speak this format"
            ),
            PlanError::BadNewest => write!(
                f,
                "the newest entry in the version list has no usable version code; the list's shape has changed"
            ),
            PlanError::NoDownload(v) => write!(f, "the mirror does not offer version {v}"),
            PlanError::TooManyDownloads(v, n) => write!(
                f,
                "the mirror lists {n} archives for version {v}, more than the {MAX_CANDIDATES} this program understands; refusing to download"
            ),
        }
    }
}

impl std::error::Error for PlanError {}

/// Pick the newest release (the first record of the x86_64 list) and
/// collect its download URLs.
pub fn plan(releases: &[Release]) -> Result<Plan, PlanError> {
    let newest = releases.first().ok_or(PlanError::NoVersions)?;
    let code = newest.version_code.ok_or(PlanError::BadNewest)?;
    let name = newest.version_name.clone().filter(|n| !n.is_empty()).ok_or(PlanError::BadNewest)?;
    let mut downloads: Vec<(String, Asset)> = Vec::new();
    for r in releases.iter().filter(|r| r.version_name.as_deref() == Some(name.as_str())) {
        let Some(asset) = &r.asset else { continue };
        let Some(url) = &asset.url else { continue };
        let is_https = url.get(..8).is_some_and(|p| p.eq_ignore_ascii_case("https://"));
        if !is_https || downloads.iter().any(|(u, _)| u == url) {
            continue;
        }
        downloads.push((url.clone(), asset.clone()));
    }
    if downloads.is_empty() {
        return Err(PlanError::NoDownload(name));
    }
    if downloads.len() > MAX_CANDIDATES {
        return Err(PlanError::TooManyDownloads(name, downloads.len()));
    }
    Ok(Plan { version_name: name, version_code: code, newest: newest.clone(), downloads })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rel(name: &str, code: &str, url: &str) -> Release {
        Release {
            version_name: Some(name.into()),
            version_code: code.parse().ok().filter(|&c| c != 0),
            asset: Some(Asset { url: Some(url.into()), ..Default::default() }),
            ..Default::default()
        }
    }

    #[test]
    fn plan_takes_first_and_collects_same_name() {
        let list = vec![
            rel("2.2", "20", "https://a.winudf.com/1"),
            rel("2.1", "10", "https://a.winudf.com/old"),
            rel("2.2", "20", "https://a.winudf.com/1"),
            rel("2.2", "20", "http://a.winudf.com/plain"),
            rel("2.2", "20", "https://a.winudf.com/2"),
        ];
        let p = plan(&list).unwrap();
        assert_eq!(p.version_name, "2.2");
        assert_eq!(p.version_code, 20);
        let urls: Vec<_> = p.downloads.iter().map(|(u, _)| u.as_str()).collect();
        assert_eq!(urls, ["https://a.winudf.com/1", "https://a.winudf.com/2"]);
    }

    #[test]
    fn plan_failures() {
        assert_eq!(plan(&[]), Err(PlanError::NoVersions));
        assert_eq!(plan(&[rel("2.2", "0", "https://x.winudf.com/")]), Err(PlanError::BadNewest));
        assert_eq!(plan(&[rel("2.2", "abc", "https://x.winudf.com/")]), Err(PlanError::BadNewest));
        assert!(matches!(plan(&[rel("2.2", "1", "http://x/")]), Err(PlanError::NoDownload(_))));
        let many: Vec<_> = (0..5).map(|i| rel("2.2", "1", &format!("https://x.winudf.com/{i}"))).collect();
        assert!(matches!(plan(&many), Err(PlanError::TooManyDownloads(_, 5))));
    }
}
