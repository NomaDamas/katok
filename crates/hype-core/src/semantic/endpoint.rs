use crate::{config::HypeConfig, Error, Result};
use std::net::IpAddr;

pub(crate) fn validate_embedding_endpoint(config: &HypeConfig) -> Result<()> {
    if config.allow_remote_embeddings || is_loopback_url(&config.embedder_base_url) {
        return Ok(());
    }
    Err(Error::RemoteEmbeddingEndpoint)
}

fn is_loopback_url(url: &str) -> bool {
    let Some(rest) = url
        .strip_prefix("http://")
        .or_else(|| url.strip_prefix("https://"))
    else {
        return false;
    };
    let authority = rest
        .split_once('/')
        .map_or(rest, |(authority, _)| authority);
    let authority = authority
        .rsplit_once('@')
        .map_or(authority, |(_, authority)| authority);
    let host = authority
        .strip_prefix('[')
        .and_then(|inner| inner.split_once(']').map(|(ipv6, _)| ipv6))
        .unwrap_or_else(|| {
            authority
                .split_once(':')
                .map_or(authority, |(name, _)| name)
        });
    host == "localhost" || host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

#[cfg(test)]
mod tests {
    use super::is_loopback_url;

    #[test]
    fn accepts_loopback_urls_when_path_is_present() {
        assert!(is_loopback_url("http://127.0.0.1:8080/v1"));
        assert!(is_loopback_url("https://localhost/embed"));
        assert!(is_loopback_url("http://[::1]:8080"));
    }

    #[test]
    fn rejects_lookalike_loopback_hosts() {
        assert!(!is_loopback_url("https://localhost.example.com"));
        assert!(!is_loopback_url("http://127.evil.com"));
        assert!(!is_loopback_url("http://example.com@remote.test"));
        assert!(!is_loopback_url("ftp://127.0.0.1"));
    }
}
