//! URL parsing for blit:// protocol

use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct RemoteDest {
    pub host: String,
    pub port: u16,
    pub path: PathBuf,
    pub tls: bool,
}

pub fn parse_remote_url(path: &Path) -> Option<RemoteDest> {
    let s = path.to_string_lossy();
    let s_trim = s.trim();
    let lower = s_trim.to_ascii_lowercase();
    let scheme_end = lower.find(':')?;
    let scheme_with_colon = &lower[..=scheme_end];
    let tls = match scheme_with_colon {
        "blit:" => false,
        "blits:" => true,
        _ => return None,
    };
    let mut rest = &s_trim[scheme_end + 1..];
    if let Some(r) = rest.strip_prefix("//") {
        rest = r;
    }
    let (hp, p) = rest.split_once('/').unwrap_or((rest, ""));
    if hp.is_empty() {
        return None;
    }
    let (host, port) = match hp.split_once(':') {
        Some((h, pr)) => match pr.parse::<u16>() {
            Ok(p) if p > 0 => (h.to_string(), p),
            _ => return None,
        },
        None => (hp.to_string(), 9031),
    };
    Some(RemoteDest {
        host,
        port,
        path: if p.is_empty() {
            PathBuf::from("/")
        } else {
            PathBuf::from(format!("/{}", p))
        },
        tls,
    })
}
