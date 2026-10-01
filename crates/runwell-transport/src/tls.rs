use crate::Error;
use rustls::{
    ClientConfig, RootCertStore, ServerConfig,
    pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject},
};
use std::{collections::BTreeSet, sync::Arc};
use x509_parser::{extensions::GeneralName, prelude::*};

/// Role and stable identity extracted exclusively from a verified URI SAN.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Identity {
    pub role: String,
    pub id: String,
}
impl Identity {
    pub fn node(id: &str) -> Self {
        Self {
            role: "node".into(),
            id: id.into(),
        }
    }
    pub fn controller(id: &str) -> Self {
        Self {
            role: "controller".into(),
            id: id.into(),
        }
    }
    pub fn uri(&self) -> String {
        format!("urn:runwell:{}:{}", self.role, self.id)
    }
    pub fn dns(&self) -> String {
        format!("{}.{}.runwell", self.id, self.role)
    }
    pub fn validate(&self) -> Result<(), Error> {
        if matches!(self.role.as_str(), "node" | "controller")
            && runwell_config::valid_identity(&self.id)
        {
            Ok(())
        } else {
            Err(Error::Tls)
        }
    }
    pub(crate) fn certificate(cert: &CertificateDer<'_>) -> Result<Self, Error> {
        let (_, cert) = X509Certificate::from_der(cert.as_ref()).map_err(|_| Error::Tls)?;
        let san = cert
            .subject_alternative_name()
            .map_err(|_| Error::Tls)?
            .ok_or(Error::Tls)?;
        let mut identities = san
            .value
            .general_names
            .iter()
            .filter_map(|name| match name {
                GeneralName::URI(uri) => uri.strip_prefix("urn:runwell:"),
                _ => None,
            });
        let (role, id) = identities
            .next()
            .and_then(|uri| uri.split_once(':'))
            .ok_or(Error::Tls)?;
        if identities.next().is_some() {
            return Err(Error::Tls);
        }
        let identity = Self {
            role: role.into(),
            id: id.into(),
        };
        identity.validate()?;
        Ok(identity)
    }
}
/// TLS configs enforce TLS 1.3, h2 ALPN, CA validation, and client authentication.
pub struct Tls {
    pub client: Arc<ClientConfig>,
    pub server: Arc<ServerConfig>,
}
impl Tls {
    pub fn load(config: &runwell_config::TransportConfig) -> Result<Self, Error> {
        let certs = CertificateDer::pem_file_iter(&config.certificate_file)
            .map_err(|_| Error::Tls)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| Error::Tls)?;
        let key = PrivateKeyDer::from_pem_file(&config.private_key_file).map_err(|_| Error::Tls)?;
        let mut roots = RootCertStore::empty();
        for cert in CertificateDer::pem_file_iter(&config.ca_file).map_err(|_| Error::Tls)? {
            roots
                .add(cert.map_err(|_| Error::Tls)?)
                .map_err(|_| Error::Tls)?;
        }
        if roots.is_empty() {
            return Err(Error::Tls);
        }
        let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
        let verifier = rustls::server::WebPkiClientVerifier::builder_with_provider(
            Arc::new(roots.clone()),
            provider.clone(),
        )
        .build()
        .map_err(|_| Error::Tls)?;
        let mut server = ServerConfig::builder_with_provider(provider.clone())
            .with_protocol_versions(&[&rustls::version::TLS13])
            .map_err(|_| Error::Tls)?
            .with_client_cert_verifier(verifier)
            .with_single_cert(certs.clone(), key.clone_key())
            .map_err(|_| Error::Tls)?;
        let mut client = ClientConfig::builder_with_provider(provider)
            .with_protocol_versions(&[&rustls::version::TLS13])
            .map_err(|_| Error::Tls)?
            .with_root_certificates(roots)
            .with_client_auth_cert(certs, key)
            .map_err(|_| Error::Tls)?;
        server.alpn_protocols = vec![b"h2".to_vec()];
        client.alpn_protocols = vec![b"h2".to_vec()];
        Ok(Self {
            client: Arc::new(client),
            server: Arc::new(server),
        })
    }
}
pub(crate) fn authorized(
    certs: Option<&[CertificateDer<'_>]>,
    allowed: &BTreeSet<Identity>,
) -> Result<Identity, Error> {
    let peer = Identity::certificate(certs.and_then(|c| c.first()).ok_or(Error::Tls)?)?;
    if allowed.contains(&peer) {
        Ok(peer)
    } else {
        Err(Error::Unauthorized)
    }
}
