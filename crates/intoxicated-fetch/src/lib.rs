//! Fetch and verify the Roblox Android build for Intoxicated.
//!
//! The pieces, in the order the binary uses them:
//!
//! * [`wire`]: a small Protocol Buffers wire-format reader.
//! * [`catalog`]: turns APKPure's version-list response into releases.
//! * [`urlpolicy`]: decides which URLs (and redirect targets) may be fetched.
//! * [`net`]: the HTTP side, with redirects walked by hand.
//! * [`zipdir`]: End of Central Directory lookup and entry names.
//! * [`apksig`]: APK Signature Scheme v2 / v3 / v3.1 verification.
//! * [`pins`]: the pinned Roblox signing certificates.

pub mod apksig;
pub mod catalog;
pub mod der;
pub mod net;
pub mod pins;
pub mod urlpolicy;
pub mod wire;
pub mod zipdir;

/// Product token sent as the User-Agent. APKPure's CDN refuses generic
/// library defaults, so this names the program truthfully.
pub fn user_agent() -> String {
    format!("Intoxicated/{} (FreeBSD)", env!("CARGO_PKG_VERSION"))
}

/// Lowercase hex of a byte string.
pub fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(DIGITS[(b >> 4) as usize] as char);
        out.push(DIGITS[(b & 15) as usize] as char);
    }
    out
}
