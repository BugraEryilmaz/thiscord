//! Optional direct HTTPS. Configuring either PEM path requires a valid pair.
use crate::BoxError;
use axum_server::tls_rustls::RustlsConfig;
use std::{env, path::PathBuf};

pub struct Tls {
    pub config: RustlsConfig,
    certificate: PathBuf,
    key: PathBuf,
}

fn paths(
    certificate: Option<String>,
    key: Option<String>,
) -> Result<Option<(PathBuf, PathBuf)>, BoxError> {
    match (certificate, key) {
        (None, None) => Ok(None),
        (Some(certificate), Some(key)) if !certificate.trim().is_empty() && !key.trim().is_empty() =>
            Ok(Some((certificate.into(), key.into()))),
        _ => Err("Set both TLS_CERT_PATH and TLS_KEY_PATH to readable PEM files, or leave both unset for HTTP".into()),
    }
}

impl Tls {
    pub async fn from_env() -> Result<Option<Self>, BoxError> {
        let get = |name| match env::var(name) {
            Ok(value) => Ok(Some(value)),
            Err(env::VarError::NotPresent) => Ok(None),
            Err(_) => Err("TLS certificate paths must contain valid Unicode"),
        };
        let Some((certificate, key)) = paths(get("TLS_CERT_PATH")?, get("TLS_KEY_PATH")?)? else {
            return Ok(None);
        };
        Self::from_paths(certificate, key).await.map(Some)
    }

    pub async fn from_paths(certificate: PathBuf, key: PathBuf) -> Result<Self, BoxError> {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let config = RustlsConfig::from_pem_file(&certificate, &key).await
            .map_err(|_| "TLS initialization failed: check TLS_CERT_PATH and TLS_KEY_PATH permissions, PEM format and matching certificate/key; HTTP fallback is disabled")?;
        Ok(Self {
            config,
            certificate,
            key,
        })
    }

    /// Call after certificate renewal; failed reloads retain the previous config.
    pub async fn reload(&self) -> Result<(), BoxError> {
        self.config
            .reload_from_pem_file(&self.certificate, &self.key)
            .await
            .map_err(|_| {
                "TLS certificate reload failed; previous certificate remains active".into()
            })
    }

    pub async fn watch_renewals(self) {
        #[cfg(unix)]
        {
            let mut signal =
                match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup()) {
                    Ok(signal) => signal,
                    Err(_) => {
                        tracing::warn!(
                            "TLS reload signal unavailable; restart after certificate renewal"
                        );
                        return;
                    }
                };
            while signal.recv().await.is_some() {
                match self.reload().await {
                    Ok(()) => tracing::info!("TLS certificate reloaded"),
                    Err(_) => tracing::error!(
                        "TLS certificate reload failed; previous certificate remains active"
                    ),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn partial_tls_configuration_never_falls_back_to_http() {
        assert!(paths(None, None).unwrap().is_none());
        assert!(paths(Some("cert.pem".into()), None).is_err());
        assert!(paths(None, Some("key.pem".into())).is_err());
        assert!(paths(Some("".into()), Some("key.pem".into())).is_err());
        assert!(
            paths(Some("cert.pem".into()), Some("key.pem".into()))
                .unwrap()
                .is_some()
        );
    }
}
