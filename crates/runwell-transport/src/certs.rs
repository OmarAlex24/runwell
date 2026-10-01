//! Offline P-256 CA and leaf issuance. Rotation writes a new generation directory.
use crate::{Error, Identity};
use rcgen::{
    BasicConstraints, CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair,
    KeyUsagePurpose, SanType,
};
use std::{fs, io::Write, path::Path};

fn params(days: u32) -> Result<CertificateParams, Error> {
    if !(1..=3650).contains(&days) {
        return Err(Error::Protocol);
    }
    let date = jiff::Timestamp::now()
        .to_zoned(jiff::tz::TimeZone::UTC)
        .date();
    let mut p = CertificateParams::default();
    p.not_before = rcgen::date_time_ymd(date.year().into(), date.month() as u8, date.day() as u8);
    p.not_after = p.not_before + std::time::Duration::from_secs(u64::from(days) * 86400);
    Ok(p)
}
fn directory(path: &Path) -> Result<(), Error> {
    fs::create_dir_all(path).map_err(|_| Error::Io)?;
    if fs::symlink_metadata(path)
        .map_err(|_| Error::Io)?
        .is_symlink()
    {
        return Err(Error::Io);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|_| Error::Io)?;
    }
    Ok(())
}
fn write(path: &Path, bytes: &[u8]) -> Result<(), Error> {
    let mut options = fs::OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path).map_err(|_| Error::Io)?;
    file.write_all(bytes).map_err(|_| Error::Io)?;
    file.sync_all().map_err(|_| Error::Io)
}
/// Create a new offline CA. Existing files are never overwritten.
pub fn create_ca(dir: &Path, days: u32) -> Result<(), Error> {
    directory(dir)?;
    let key = KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256).map_err(|_| Error::Tls)?;
    let mut p = params(days)?;
    p.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    p.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    p.distinguished_name
        .push(DnType::CommonName, "runwell offline CA");
    let cert = p.self_signed(&key).map_err(|_| Error::Tls)?;
    write(&dir.join("ca-key.pem"), key.serialize_pem().as_bytes())?;
    write(&dir.join("ca.pem"), cert.pem().as_bytes())
}
/// Issue or rotate a peer into a fresh directory. Both leaf generations are trusted
/// during their validity overlap; the authenticated identity remains unchanged.
pub fn issue(ca_dir: &Path, output: &Path, identity: &Identity, days: u32) -> Result<(), Error> {
    identity.validate()?;
    directory(output)?;
    let ca_pem = fs::read_to_string(ca_dir.join("ca.pem")).map_err(|_| Error::Io)?;
    let ca_key =
        KeyPair::from_pem(&fs::read_to_string(ca_dir.join("ca-key.pem")).map_err(|_| Error::Io)?)
            .map_err(|_| Error::Tls)?;
    let issuer = Issuer::from_ca_cert_pem(&ca_pem, ca_key).map_err(|_| Error::Tls)?;
    let key = KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256).map_err(|_| Error::Tls)?;
    let mut p = params(days)?;
    p.subject_alt_names = vec![
        SanType::DnsName(identity.dns().try_into().map_err(|_| Error::Tls)?),
        SanType::URI(identity.uri().try_into().map_err(|_| Error::Tls)?),
    ];
    p.distinguished_name.push(DnType::CommonName, &identity.id);
    p.key_usages = vec![KeyUsagePurpose::DigitalSignature];
    p.extended_key_usages = vec![
        ExtendedKeyUsagePurpose::ClientAuth,
        ExtendedKeyUsagePurpose::ServerAuth,
    ];
    let cert = p.signed_by(&key, &issuer).map_err(|_| Error::Tls)?;
    write(&output.join("key.pem"), key.serialize_pem().as_bytes())?;
    write(&output.join("identity.pem"), cert.pem().as_bytes())?;
    write(&output.join("ca.pem"), ca_pem.as_bytes())
}
