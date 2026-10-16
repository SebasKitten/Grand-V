use anyhow::{Result, bail};
use rcgen::{
    BasicConstraints, CertificateParams, DistinguishedName, DnType, ExtendedKeyUsagePurpose, IsCa,
    KeyPair, KeyUsagePurpose, SanType,
};
use sha2::{Digest, Sha256};
use std::{path::Path, sync::Arc};

pub fn load(cert: &Path, key: &Path) -> Result<tokio_rustls::TlsAcceptor> {
    let mut cert_reader = std::io::BufReader::new(std::fs::File::open(cert)?);
    let certs = rustls_pemfile::certs(&mut cert_reader).collect::<std::io::Result<Vec<_>>>()?;
    let mut key_reader = std::io::BufReader::new(std::fs::File::open(key)?);
    let key = rustls_pemfile::private_key(&mut key_reader)?
        .ok_or_else(|| anyhow::anyhow!("No private key in PEM"))?;
    let config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)?;
    Ok(tokio_rustls::TlsAcceptor::from(Arc::new(config)))
}
fn signing_key(path: &Path) -> Result<KeyPair> {
    let bytes = std::fs::read(path)?;
    let key = rustls_pemfile::private_key(&mut std::io::Cursor::new(bytes))?
        .ok_or_else(|| anyhow::anyhow!("Missing CA private key"))?;
    match key {
        rustls::pki_types::PrivateKeyDer::Pkcs1(key) => {
            // Python writes TraditionalOpenSSL RSA keys. Wrap, do not replace, the existing key as PKCS#8 for rcgen/ring.
            let pkcs8 = yasna::construct_der(|w| {
                w.write_sequence(|w| {
                    w.next().write_u8(0);
                    w.next().write_sequence(|w| {
                        w.next()
                            .write_oid(&yasna::models::ObjectIdentifier::from_slice(&[
                                1, 2, 840, 113549, 1, 1, 1,
                            ]));
                        w.next().write_null();
                    });
                    w.next().write_bytes(key.secret_pkcs1_der());
                })
            });
            Ok(KeyPair::try_from(pkcs8)?)
        }
        key => Ok(KeyPair::try_from(&key)?),
    }
}
pub fn generate(folder: &Path) -> Result<(String, String)> {
    let dir = folder.join("lan-tls");
    std::fs::create_dir_all(&dir)?;
    let cp = dir.join("lan-root-ca.pem");
    let kp = dir.join("lan-root-ca.key");
    if cp.exists() != kp.exists() {
        bail!("Incomplete LAN root CA pair");
    }
    let now = time::OffsetDateTime::now_utc();
    let (params, key, der, expiry) = if cp.exists() {
        let pem = std::fs::read_to_string(&cp)?;
        let key = signing_key(&kp)?;
        let (_, parsed) = x509_parser::pem::parse_x509_pem(pem.as_bytes())
            .map_err(|_| anyhow::anyhow!("Invalid CA PEM"))?;
        let (_, x509) = x509_parser::parse_x509_certificate(&parsed.contents)
            .map_err(|_| anyhow::anyhow!("Invalid CA certificate"))?;
        if !x509.is_ca()
            || !x509.validity().is_valid()
            || x509.public_key().raw != key.public_key_der()
        {
            bail!(
                "LAN root expired, invalid, or key mismatch; preserve it and fix the certificate pair"
            );
        }
        let expiry =
            time::OffsetDateTime::from_unix_timestamp(x509.validity().not_after.timestamp())?;
        (
            CertificateParams::from_ca_cert_pem(&pem)?,
            key,
            parsed.contents,
            expiry,
        )
    } else {
        let mut p = CertificateParams::new(Vec::<String>::new())?;
        p.distinguished_name = DistinguishedName::new();
        p.distinguished_name
            .push(DnType::CommonName, "GrandV LAN Local Root CA");
        p.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
        p.not_before = now - time::Duration::minutes(5);
        p.not_after = now + time::Duration::days(365);
        p.key_usages = vec![
            KeyUsagePurpose::DigitalSignature,
            KeyUsagePurpose::KeyCertSign,
            KeyUsagePurpose::CrlSign,
        ];
        let key = KeyPair::generate()?;
        let ca = p.clone().self_signed(&key)?;
        crate::storage::atomic_write(&kp, key.serialize_pem().as_bytes(), true)?;
        crate::storage::atomic_write(&cp, ca.pem().as_bytes(), false)?;
        let der = ca.der().to_vec();
        let expiry = p.not_after;
        (p, key, der, expiry)
    };
    // Existing root DER stays byte-for-byte stable. The reconstructed issuer is used only to sign the leaf.
    let issuer = params.self_signed(&key)?;
    let mut leaf = CertificateParams::new(vec!["localhost".into()])?;
    leaf.distinguished_name = DistinguishedName::new();
    leaf.distinguished_name
        .push(DnType::CommonName, "GrandV LAN Host");
    leaf.subject_alt_names
        .push(SanType::IpAddress("127.0.0.1".parse()?));
    for ip in crate::config::addresses() {
        leaf.subject_alt_names.push(SanType::IpAddress(ip.into()));
    }
    leaf.not_before = now - time::Duration::minutes(5);
    leaf.not_after = (now + time::Duration::days(90)).min(expiry);
    leaf.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    let leaf_key = KeyPair::generate()?;
    let cert = leaf.signed_by(&leaf_key, &issuer, &key)?;
    let cert_path = dir.join("lan-server.pem");
    let key_path = dir.join("lan-server.key");
    crate::storage::atomic_write(&key_path, leaf_key.serialize_pem().as_bytes(), true)?;
    crate::storage::atomic_write(&cert_path, cert.pem().as_bytes(), false)?;
    crate::storage::atomic_write(&dir.join("lan-root-ca.cer"), &der, false)?;
    Ok((
        cert_path.to_string_lossy().into(),
        key_path.to_string_lossy().into(),
    ))
}
pub fn public_root(cert_path: &Path) -> Result<(Vec<u8>, String)> {
    let path = cert_path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("Missing TLS folder"))?
        .join("lan-root-ca.cer");
    let der = std::fs::read(path)?;
    if der.len() > 65536 {
        bail!("Invalid root size");
    }
    let (remaining, parsed) = x509_parser::parse_x509_certificate(&der)
        .map_err(|_| anyhow::anyhow!("Invalid public DER"))?;
    if !remaining.is_empty() || !parsed.is_ca() {
        bail!("Not a CA certificate");
    }
    let fingerprint = hex::encode(Sha256::digest(&der));
    Ok((der, fingerprint))
}
