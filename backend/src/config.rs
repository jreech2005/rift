//! Backend configuration.
//!
//! Only non-secret settings are read here. Provider API keys (added in later
//! phases) must never be logged or forwarded to game clients.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConfigError {
    #[error("BACKEND_HOST is not a valid IP address")]
    InvalidHost,
    #[error("BACKEND_PORT is not a valid port number")]
    InvalidPort,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub host: IpAddr,
    pub port: u16,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            host: IpAddr::V4(Ipv4Addr::LOCALHOST),
            port: 3000,
        }
    }
}

impl Config {
    /// Read configuration from the process environment.
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_lookup(|key| std::env::var(key).ok())
    }

    fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Result<Self, ConfigError> {
        let defaults = Self::default();
        let non_empty = |key: &str| lookup(key).filter(|v| !v.trim().is_empty());
        let host = match non_empty("BACKEND_HOST") {
            Some(v) => v.trim().parse().map_err(|_| ConfigError::InvalidHost)?,
            None => defaults.host,
        };
        let port = match non_empty("BACKEND_PORT") {
            Some(v) => v.trim().parse().map_err(|_| ConfigError::InvalidPort)?,
            None => defaults.port,
        };
        Ok(Self { host, port })
    }

    pub fn addr(&self) -> SocketAddr {
        SocketAddr::new(self.host, self.port)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_when_unset() {
        let cfg = Config::from_lookup(|_| None).unwrap();
        assert_eq!(cfg.addr().to_string(), "127.0.0.1:3000");
    }

    #[test]
    fn reads_values() {
        let cfg = Config::from_lookup(|k| match k {
            "BACKEND_HOST" => Some("0.0.0.0".into()),
            "BACKEND_PORT" => Some("4100".into()),
            _ => None,
        })
        .unwrap();
        assert_eq!(cfg.addr().to_string(), "0.0.0.0:4100");
    }

    #[test]
    fn rejects_bad_port() {
        let err = Config::from_lookup(|k| (k == "BACKEND_PORT").then(|| "abc".into())).unwrap_err();
        assert_eq!(err, ConfigError::InvalidPort);
    }
}
