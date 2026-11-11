//! Listener-owned response origins, independent of request routing headers.

use std::io;
use std::net::IpAddr;

#[derive(Clone, Debug)]
pub(super) struct ResponseOrigin(String);

impl ResponseOrigin {
    pub(super) fn parse(value: &str) -> io::Result<Self> {
        if !value.is_ascii() {
            return Err(invalid());
        }

        if value
            .bytes()
            .any(|byte| byte.is_ascii_whitespace() || byte.is_ascii_control())
        {
            return Err(invalid());
        }

        if value.contains(['\\', '@', '#', '?']) {
            return Err(invalid());
        }

        let uri: http::Uri = value.parse().map_err(|_| invalid())?;
        let scheme = uri.scheme_str().ok_or_else(invalid)?.to_ascii_lowercase();

        if !matches!(scheme.as_str(), "http" | "https") {
            return Err(invalid());
        }

        if uri
            .path_and_query()
            .is_some_and(|path| path.as_str() != "/")
        {
            return Err(invalid());
        }

        let authority = uri.authority().ok_or_else(invalid)?;
        let host = authority.host().to_ascii_lowercase();
        let address = host.trim_start_matches('[').trim_end_matches(']');

        match address.parse::<IpAddr>() {
            Ok(address) if address.is_unspecified() => return Err(invalid()),
            Err(_) if !valid_dns_name(&host) => return Err(invalid()),
            Ok(_) | Err(_) => {}
        }

        let suffix = authority
            .as_str()
            .strip_prefix(authority.host())
            .ok_or_else(invalid)?;

        let port = if suffix.is_empty() {
            None
        } else {
            Some(
                suffix
                    .strip_prefix(':')
                    .ok_or_else(invalid)?
                    .parse::<u16>()
                    .map_err(|_| invalid())?,
            )
        };

        if port == Some(0) {
            return Err(invalid());
        }

        let default_port = if scheme == "http" { 80 } else { 443 };
        let port = port.filter(|port| *port != default_port);

        let value = port.map_or_else(
            || format!("{scheme}://{host}"),
            |port| format!("{scheme}://{host}:{port}"),
        );

        Ok(Self(value))
    }

    pub(super) fn as_str(&self) -> &str {
        &self.0
    }
}

fn valid_dns_name(host: &str) -> bool {
    if host.is_empty() || host.len() > 253 {
        return false;
    }

    host.strip_suffix('.')
        .unwrap_or(host)
        .split('.')
        .all(|label| {
            if label.is_empty() || label.len() > 63 {
                return false;
            }

            if label.starts_with('-') || label.ends_with('-') {
                return false;
            }

            label
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        })
}

fn invalid() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        "invalid public response origin",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_origins_without_normalizing_paths_or_resolving_hosts() {
        for (value, expected) in [
            ("HTTPS://EXAMPLE.INVALID:443/", "https://example.invalid"),
            ("http://localhost:80", "http://localhost"),
            (
                "https://xn--bcher-kva.invalid:8443",
                "https://xn--bcher-kva.invalid:8443",
            ),
            ("http://127.0.0.1:12345", "http://127.0.0.1:12345"),
            ("http://[::1]:12345/", "http://[::1]:12345"),
        ] {
            assert_eq!(
                ResponseOrigin::parse(value).unwrap().as_str(),
                expected,
                "{value:?}"
            );
        }

        for value in [
            "",
            "example.invalid",
            "ftp://example.invalid",
            "http://0.0.0.0",
            "http://[::]",
            "http://*.invalid",
            "http://name@host.invalid",
            "http://host.invalid:0",
            "http://host.invalid:65536",
            "http://host.invalid:",
            "http://host.invalid/path",
            "http://host.invalid/../",
            "http://host.invalid/./",
            "http://host.invalid/%2e/",
            "http://host.invalid//",
            "http://host.invalid?",
            "http://host.invalid#",
            "http://host.invalid\\",
            "http://host.invalid\r\n",
            "http://bücher.invalid",
            " http://host.invalid",
        ] {
            assert_eq!(
                ResponseOrigin::parse(value).err().map(|error| error.kind()),
                Some(io::ErrorKind::InvalidInput),
                "{value:?}"
            );
        }
    }
}
