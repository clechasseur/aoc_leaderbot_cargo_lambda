use clap::Args;
use miette::{Diagnostic, Result};
use rustls::{ClientConfig, RootCertStore, ServerConfig};
use rustls_pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject};
use serde::{Deserialize, Serialize, ser::SerializeStruct};
use std::{
    path::{Path, PathBuf},
    sync::OnceLock,
};
use thiserror::Error;

static CELL: OnceLock<bool> = OnceLock::new();

#[derive(Debug, Diagnostic, Error)]
pub enum TlsError {
    #[error("missing TLS certificate")]
    #[diagnostic()]
    MissingTlsCert,

    #[error("missing TLS key")]
    #[diagnostic()]
    MissingTlsKey,

    #[error("invalid TLS file: {0}, {1}")]
    #[diagnostic()]
    InvalidTlsFile(PathBuf, rustls_pki_types::pem::Error),

    #[error("failed to parse TLS key: {0}")]
    #[diagnostic()]
    FailedToParseTlsKey(String),

    #[error("failed to parse config: {0}")]
    #[diagnostic()]
    FailedToParseConfig(#[from] rustls::Error),

    #[error("failed to create verifier: {0}")]
    #[diagnostic()]
    FailedToCreateVerifier(rustls::Error),
}

#[derive(Args, Clone, Debug, Deserialize, Serialize)]
pub struct TlsOptions {
    /// Path to a TLS certificate file
    #[arg(long, conflicts_with = "remote")]
    #[serde(default)]
    pub tls_cert: Option<PathBuf>,
    /// Path to a TLS key file
    #[arg(long, conflicts_with = "remote")]
    #[serde(default)]
    pub tls_key: Option<PathBuf>,
    /// Path to a TLS CA file
    #[arg(long, conflicts_with = "remote")]
    #[serde(default)]
    pub tls_ca: Option<PathBuf>,

    #[cfg(test)]
    pub config_dir: PathBuf,
}

impl TlsOptions {
    #[cfg(not(test))]
    pub fn new(
        tls_cert: Option<PathBuf>,
        tls_key: Option<PathBuf>,
        tls_ca: Option<PathBuf>,
    ) -> Self {
        Self {
            tls_cert,
            tls_key,
            tls_ca,
        }
    }

    #[cfg(test)]
    pub fn new(
        tls_cert: Option<PathBuf>,
        tls_key: Option<PathBuf>,
        tls_ca: Option<PathBuf>,
    ) -> Self {
        Self {
            tls_cert,
            tls_key,
            tls_ca,
            config_dir: tempfile::TempDir::new().unwrap().path().to_path_buf(),
        }
    }

    pub fn is_secure(&self) -> bool {
        self.cert_path().is_some() && self.key_path().is_some()
    }

    pub fn server_config(&self) -> Result<Option<ServerConfig>> {
        if !self.is_secure() {
            return Ok(None);
        }

        CELL.get_or_init(install_default_tls_provider);

        let (mut cert_chain, key) =
            parse_cert_and_key(self.cert_path().as_ref(), self.key_path().as_ref())?;

        if let Some(path) = self.ca_path() {
            let certs = parse_certificates(path)?;
            if !certs.is_empty() {
                cert_chain.extend(certs);
            }
        }

        let mut config = ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(cert_chain, key)
            .map_err(TlsError::FailedToParseConfig)?;

        config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];

        Ok(Some(config))
    }

    pub fn client_config(&self) -> Result<ClientConfig> {
        CELL.get_or_init(install_default_tls_provider);

        let builder = if let Some(path) = self.ca_path() {
            let mut root_store = RootCertStore::empty();
            root_store.add_parsable_certificates(parse_certificates(path)?);
            ClientConfig::builder().with_root_certificates(root_store)
        } else {
            use rustls_platform_verifier::BuilderVerifierExt;
            ClientConfig::builder()
                .with_platform_verifier()
                .map_err(TlsError::FailedToCreateVerifier)?
        };

        let (cert, key) = parse_cert_and_key(self.cert_path().as_ref(), self.key_path().as_ref())?;

        let config = builder
            .with_client_auth_cert(cert, key)
            .map_err(TlsError::FailedToParseConfig)?;

        Ok(config)
    }

    fn cert_path(&self) -> Option<PathBuf> {
        self.tls_cert.clone().or_else(|| self.cached_cert_path())
    }

    fn key_path(&self) -> Option<PathBuf> {
        self.tls_key.clone().or_else(|| self.cached_key_path())
    }

    fn ca_path(&self) -> Option<PathBuf> {
        self.tls_ca.clone().or_else(|| self.cached_ca_path())
    }

    fn cached_cert_path(&self) -> Option<PathBuf> {
        let cache = self.config_dir().map(|p| p.join("cert.pem"));
        if cache.as_ref().is_some_and(|p| p.exists() && p.is_file()) {
            return cache;
        }

        None
    }

    fn cached_key_path(&self) -> Option<PathBuf> {
        let cache = self.config_dir().map(|p| p.join("key.pem"));
        if cache.as_ref().is_some_and(|p| p.exists() && p.is_file()) {
            return cache;
        }

        None
    }

    fn cached_ca_path(&self) -> Option<PathBuf> {
        let cache = self.config_dir().map(|p| p.join("ca.pem"));
        if cache.as_ref().is_some_and(|p| p.exists() && p.is_file()) {
            return cache;
        }

        None
    }

    #[cfg(not(test))]
    fn config_dir(&self) -> Option<PathBuf> {
        dirs::config_dir().map(|p| p.join("cargo-lambda"))
    }

    #[cfg(test)]
    fn config_dir(&self) -> Option<PathBuf> {
        Some(self.config_dir.clone())
    }

    pub fn count_fields(&self) -> usize {
        self.tls_cert.is_some() as usize
            + self.tls_key.is_some() as usize
            + self.tls_ca.is_some() as usize
    }

    pub fn serialize_fields<S>(
        &self,
        state: &mut <S as serde::Serializer>::SerializeStruct,
    ) -> Result<(), S::Error>
    where
        S: serde::Serializer,
    {
        if let Some(tls_cert) = &self.tls_cert {
            state.serialize_field("tls_cert", tls_cert)?;
        }
        if let Some(tls_key) = &self.tls_key {
            state.serialize_field("tls_key", tls_key)?;
        }
        if let Some(tls_ca) = &self.tls_ca {
            state.serialize_field("tls_ca", tls_ca)?;
        }
        Ok(())
    }
}

impl Default for TlsOptions {
    fn default() -> Self {
        Self::new(None, None, None)
    }
}

fn parse_certificates<P: AsRef<Path>>(path: P) -> Result<Vec<CertificateDer<'static>>> {
    let path = path.as_ref();
    let parser = CertificateDer::pem_file_iter(path)
        .map_err(|e| TlsError::InvalidTlsFile(path.to_path_buf(), e))?
        .collect::<Vec<_>>();

    let mut certs = Vec::with_capacity(parser.len());
    for cert in parser {
        certs.push(cert.map_err(|e| TlsError::InvalidTlsFile(path.to_path_buf(), e))?);
    }

    Ok(certs)
}

fn parse_cert_and_key(
    cert: Option<&PathBuf>,
    key: Option<&PathBuf>,
) -> Result<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>)> {
    let path = cert.ok_or(TlsError::MissingTlsCert)?;
    let cert = parse_certificates(path)?;

    let path = key.ok_or(TlsError::MissingTlsKey)?;
    let key = PrivateKeyDer::from_pem_file(path)
        .map_err(|e| TlsError::FailedToParseTlsKey(e.to_string()))?;

    Ok((cert, key))
}

fn install_default_tls_provider() -> bool {
    rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .expect("failed to install the default TLS provider");
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_file(source: &str, destination: &PathBuf) {
        std::fs::create_dir_all(destination.parent().unwrap()).unwrap();
        std::fs::copy(source, destination).unwrap();
    }

    #[tokio::test]
    async fn test_tls_options_default() {
        let opts = TlsOptions::default();
        assert!(!opts.is_secure());

        create_test_file(
            "../../tests/certs/cert.pem",
            &opts.config_dir.join("cert.pem"),
        );
        create_test_file(
            "../../tests/certs/key.pem",
            &opts.config_dir.join("key.pem"),
        );
        create_test_file("../../tests/certs/ca.pem", &opts.config_dir.join("ca.pem"));

        // Should return temp paths in test mode
        assert_eq!(opts.cert_path().unwrap(), opts.config_dir.join("cert.pem"));
        assert_eq!(opts.key_path().unwrap(), opts.config_dir.join("key.pem"));
        assert_eq!(opts.ca_path().unwrap(), opts.config_dir.join("ca.pem"));
        assert!(opts.is_secure());

        let config = opts.server_config().unwrap();
        assert!(config.is_some());
    }

    #[test]
    fn test_tls_options_with_paths() {
        let opts = TlsOptions::new(
            Some("../../tests/certs/cert.pem".into()),
            Some("../../tests/certs/key.pem".into()),
            Some("../../tests/certs/ca.pem".into()),
        );

        assert_eq!(
            opts.cert_path().unwrap(),
            PathBuf::from("../../tests/certs/cert.pem")
        );
        assert_eq!(
            opts.key_path().unwrap(),
            PathBuf::from("../../tests/certs/key.pem")
        );
        assert_eq!(
            opts.ca_path().unwrap(),
            PathBuf::from("../../tests/certs/ca.pem")
        );
        assert!(opts.is_secure());
    }

    #[test]
    fn test_cached_paths() {
        let opts = TlsOptions::default();

        assert!(opts.cached_cert_path().is_none());
        assert!(opts.cached_key_path().is_none());
        assert!(opts.cached_ca_path().is_none());

        create_test_file(
            "../../tests/certs/cert.pem",
            &opts.config_dir.join("cert.pem"),
        );
        create_test_file(
            "../../tests/certs/key.pem",
            &opts.config_dir.join("key.pem"),
        );
        create_test_file("../../tests/certs/ca.pem", &opts.config_dir.join("ca.pem"));

        assert_eq!(
            opts.cached_cert_path().unwrap(),
            opts.config_dir.join("cert.pem")
        );
        assert_eq!(
            opts.cached_key_path().unwrap(),
            opts.config_dir.join("key.pem")
        );
        assert_eq!(
            opts.cached_ca_path().unwrap(),
            opts.config_dir.join("ca.pem")
        );
    }

    #[tokio::test]
    async fn test_server_config_with_valid_files_in_temp_dir() {
        let opts = TlsOptions::new(
            Some("../../tests/certs/cert.pem".into()),
            Some("../../tests/certs/key.pem".into()),
            None,
        );

        assert!(opts.is_secure());

        let config = opts.server_config().unwrap();
        assert!(config.is_some());
    }

    #[tokio::test]
    async fn test_server_config_with_ca() {
        let opts = TlsOptions::default();

        create_test_file(
            "../../tests/certs/cert.pem",
            &opts.config_dir.join("cert.pem"),
        );
        create_test_file(
            "../../tests/certs/key.pem",
            &opts.config_dir.join("key.pem"),
        );
        create_test_file("../../tests/certs/ca.pem", &opts.config_dir.join("ca.pem"));

        let config = opts.server_config().unwrap();
        assert!(config.is_some());
    }

    #[tokio::test]
    async fn test_client_config_with_ca() {
        let opts = TlsOptions::default();

        create_test_file(
            "../../tests/certs/cert.pem",
            &opts.config_dir.join("cert.pem"),
        );
        create_test_file(
            "../../tests/certs/key.pem",
            &opts.config_dir.join("key.pem"),
        );
        create_test_file("../../tests/certs/ca.pem", &opts.config_dir.join("ca.pem"));

        let config = opts.client_config().unwrap();
        assert!(config.alpn_protocols.is_empty()); // Default client config has no ALPN protocols
    }

    #[tokio::test]
    async fn test_client_config_without_ca() {
        let opts = TlsOptions::default();

        create_test_file(
            "../../tests/certs/cert.pem",
            &opts.config_dir.join("cert.pem"),
        );
        create_test_file(
            "../../tests/certs/key.pem",
            &opts.config_dir.join("key.pem"),
        );

        let config = opts.client_config().unwrap();
        assert!(config.alpn_protocols.is_empty());
    }
}
