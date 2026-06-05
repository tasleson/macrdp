use std::fs::File;
use std::io::BufReader;
use std::path::Path;
use std::sync::Arc;

use anyhow::Context as _;
use rustls_pemfile::{certs, pkcs8_private_keys};
use tokio_rustls::rustls::pki_types::pem::PemObject as _;
use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer};
use tokio_rustls::{rustls, TlsAcceptor};

pub struct TlsIdentityCtx {
    pub certs: Vec<CertificateDer<'static>>,
    pub priv_key: PrivateKeyDer<'static>,
    pub pub_key: Vec<u8>,
}

impl TlsIdentityCtx {
    /// A constructor to create a `TlsIdentityCtx` from the given certificate and key paths.
    ///
    /// The file format can be either PEM (if the file extension ends with .pem) or DER.
    pub fn init_from_paths(cert_path: &Path, key_path: &Path) -> anyhow::Result<Self> {
        let certs = if cert_path.extension().is_some_and(|ext| ext == "pem") {
            CertificateDer::pem_file_iter(cert_path)
                .with_context(|| format!("reading server cert `{cert_path:?}`"))?
                .collect::<Result<Vec<_>, _>>()
                .with_context(|| format!("collecting server cert `{cert_path:?}`"))?
        } else {
            certs(&mut BufReader::new(File::open(cert_path).with_context(
                || format!("opening server cert `{cert_path:?}`"),
            )?))
            .collect::<Result<Vec<_>, _>>()
            .with_context(|| format!("collecting server cert `{cert_path:?}`"))?
        };

        let priv_key = if key_path.extension().is_some_and(|ext| ext == "pem") {
            PrivateKeyDer::from_pem_file(key_path)
                .with_context(|| format!("reading server key `{key_path:?}`"))?
        } else {
            pkcs8_private_keys(&mut BufReader::new(File::open(key_path)?))
                .next()
                .context("no private key")?
                .map(PrivateKeyDer::from)?
        };

        let pub_key = {
            let cert = certs
                .first()
                .ok_or_else(|| std::io::Error::other("invalid cert"))?;
            credssp_public_key_from_cert_der(cert.as_ref())?
        };

        Ok(Self {
            certs,
            priv_key,
            pub_key,
        })
    }

    pub fn make_acceptor(&self) -> anyhow::Result<TlsAcceptor> {
        let mut server_config = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(self.certs.clone(), self.priv_key.clone_key())
            .context("bad certificate/key")?;

        // This adds support for the SSLKEYLOGFILE env variable (https://wiki.wireshark.org/TLS#using-the-pre-master-secret)
        server_config.key_log = Arc::new(rustls::KeyLogFile::new());

        Ok(TlsAcceptor::from(Arc::new(server_config)))
    }
}

fn credssp_public_key_from_cert_der(cert_der: &[u8]) -> anyhow::Result<Vec<u8>> {
    use x509_cert::der::Decode as _;

    let cert = x509_cert::Certificate::from_der(cert_der).context("parsing server certificate")?;

    cert.tbs_certificate
        .subject_public_key_info
        .subject_public_key
        .as_bytes()
        .ok_or_else(|| std::io::Error::other("subject public key BIT STRING is not aligned"))
        .map(ToOwned::to_owned)
        .context("reading CredSSP SubjectPublicKey bytes")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credssp_public_key_uses_raw_subject_public_key_bytes() {
        let key_pair = rcgen::KeyPair::generate().unwrap();
        let cert = rcgen::CertificateParams::default()
            .self_signed(&key_pair)
            .unwrap();
        let cert = CertificateDer::from_pem_slice(cert.pem().as_bytes()).unwrap();

        let public_key = credssp_public_key_from_cert_der(cert.as_ref()).unwrap();

        assert_eq!(
            public_key.first(),
            Some(&0x04),
            "P-256 SubjectPublicKey bytes must start with the uncompressed EC point marker"
        );
    }
}
