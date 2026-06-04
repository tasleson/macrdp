//! TLS certificate generation and management

use anyhow::{bail, Context, Result};
use rcgen::{CertificateParams, DistinguishedName, KeyPair};
use std::fs;
use std::path::{Path, PathBuf};

use crate::config::{default_cert_path, default_key_path};

/// Generate a self-signed certificate and write PEM files
pub fn generate_self_signed_cert(cert_path: &Path, key_path: &Path) -> Result<()> {
    let mut params = CertificateParams::default();
    let mut dn = DistinguishedName::new();
    dn.push(rcgen::DnType::CommonName, "macrdp");
    dn.push(rcgen::DnType::OrganizationName, "macrdp");
    params.distinguished_name = dn;

    params.not_after = rcgen::date_time_ymd(2027, 3, 24);

    params.subject_alt_names = vec![
        rcgen::SanType::DnsName("localhost".try_into()?),
        rcgen::SanType::IpAddress(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)),
    ];

    let key_pair = KeyPair::generate()?;
    let cert = params.self_signed(&key_pair)?;

    if let Some(parent) = cert_path.parent() {
        fs::create_dir_all(parent)?;
    }
    if let Some(parent) = key_path.parent() {
        fs::create_dir_all(parent)?;
        // Lock the directory holding the private key so other local users
        // cannot enumerate it. The key file itself is mode 0o600, but
        // tightening the parent is defense-in-depth and matches the posture
        // sshd/openssl expect for key material.
        restrict_dir_to_owner(parent)
            .with_context(|| format!("failed to restrict {} to 0700", parent.display()))?;
    }

    fs::write(cert_path, cert.pem())?;
    write_private_file(key_path, key_pair.serialize_pem().as_bytes())
        .with_context(|| format!("failed to write private key to {}", key_path.display()))?;

    tracing::warn!(
        ?cert_path,
        ?key_path,
        "Generated self-signed TLS certificate — for development use; \
         production daemons should configure cert_path/key_path explicitly"
    );
    Ok(())
}

/// Write `contents` to `path` with mode 0o600 (Unix). The file is created
/// with the restricted mode atomically — there is no window during which the
/// key material exists on disk with broader permissions.
///
/// On non-Unix platforms this falls back to `fs::write` because the only OS
/// we target is macOS; the helper exists so the call site stays portable.
fn write_private_file(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        // O_CREAT|O_TRUNC|O_WRONLY with mode 0o600 — same shape as fs::write
        // but the permission bits apply at creation time rather than after.
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?;
        file.write_all(contents)?;
        // If the file already existed, `mode()` is ignored — re-apply to be
        // safe. ensure_tls_files only generates when missing, so this is just
        // belt and suspenders.
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
        Ok(())
    }
    #[cfg(not(unix))]
    {
        fs::write(path, contents)
    }
}

#[cfg(unix)]
fn restrict_dir_to_owner(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700))
}

#[cfg(not(unix))]
fn restrict_dir_to_owner(_dir: &Path) -> std::io::Result<()> {
    Ok(())
}

/// Resolve TLS cert and key paths for the daemon.
///
/// Behavior depends on whether the operator configured explicit paths:
///
/// - **Both `cert_path` and `key_path` set** (operator-supplied, durable):
///   the files must already exist. The daemon will not create or overwrite
///   them, so a misconfigured path surfaces as a clear startup error rather
///   than a silently-generated self-signed cert.
/// - **Neither set** (defaults under the macrdp config dir): a self-signed
///   cert is generated on first launch as a development convenience, then
///   reused on subsequent launches.
/// - **Only one of the two set**: rejected as a configuration error.
pub fn ensure_tls_files(
    cert_path: Option<&Path>,
    key_path: Option<&Path>,
) -> Result<(PathBuf, PathBuf)> {
    match (cert_path, key_path) {
        (Some(cert), Some(key)) => {
            if !cert.exists() || !key.exists() {
                bail!(
                    "Configured TLS cert/key not found (cert={}, key={}); \
                     the daemon will not generate certificates at operator-supplied paths — \
                     install the files or unset both paths to use the default dev location",
                    cert.display(),
                    key.display(),
                );
            }
            tracing::info!(?cert, ?key, "Using operator-supplied TLS certificate");
            Ok((cert.to_path_buf(), key.to_path_buf()))
        }
        (None, None) => {
            let cert = default_cert_path();
            let key = default_key_path();
            if !cert.exists() || !key.exists() {
                generate_self_signed_cert(&cert, &key)
                    .context("Failed to generate self-signed certificate")?;
            } else {
                tracing::info!(
                    ?cert,
                    ?key,
                    "Using existing TLS certificate at default path"
                );
            }
            Ok((cert, key))
        }
        (Some(_), None) | (None, Some(_)) => {
            bail!(
                "TLS cert_path and key_path must both be set or both unset; \
                 partial configuration is not supported"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_generate_self_signed_cert() {
        let dir = TempDir::new().unwrap();
        let cert_path = dir.path().join("cert.pem");
        let key_path = dir.path().join("key.pem");

        generate_self_signed_cert(&cert_path, &key_path).unwrap();

        assert!(cert_path.exists());
        assert!(key_path.exists());

        let cert_content = fs::read_to_string(&cert_path).unwrap();
        assert!(cert_content.contains("BEGIN CERTIFICATE"));

        let key_content = fs::read_to_string(&key_path).unwrap();
        assert!(key_content.contains("BEGIN PRIVATE KEY"));
    }

    #[test]
    fn explicit_paths_must_already_exist() {
        let dir = TempDir::new().unwrap();
        let cert_path = dir.path().join("cert.pem");
        let key_path = dir.path().join("key.pem");

        let err = ensure_tls_files(Some(&cert_path), Some(&key_path))
            .expect_err("operator-supplied paths that don't exist must error");
        let msg = format!("{err}");
        assert!(msg.contains("not found"), "got: {msg}");
        assert!(
            !cert_path.exists(),
            "must not have written a generated cert at operator-supplied path",
        );
    }

    #[test]
    fn explicit_paths_reused_when_present() {
        let dir = TempDir::new().unwrap();
        let cert_path = dir.path().join("cert.pem");
        let key_path = dir.path().join("key.pem");
        generate_self_signed_cert(&cert_path, &key_path).unwrap();
        let cert_before = fs::read(&cert_path).unwrap();

        let (c, k) = ensure_tls_files(Some(&cert_path), Some(&key_path)).unwrap();
        assert_eq!(c, cert_path);
        assert_eq!(k, key_path);
        assert_eq!(
            fs::read(&cert_path).unwrap(),
            cert_before,
            "must not regenerate existing operator-supplied cert"
        );
    }

    #[test]
    fn asymmetric_path_config_is_rejected() {
        let dir = TempDir::new().unwrap();
        let cert_path = dir.path().join("cert.pem");

        let err = ensure_tls_files(Some(&cert_path), None).expect_err("cert-only must error");
        assert!(format!("{err}").contains("both"));

        let key_path = dir.path().join("key.pem");
        let err = ensure_tls_files(None, Some(&key_path)).expect_err("key-only must error");
        assert!(format!("{err}").contains("both"));
    }

    #[cfg(unix)]
    #[test]
    fn generated_key_has_owner_only_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TempDir::new().unwrap();
        let cert_path = dir.path().join("cert.pem");
        let key_path = dir.path().join("key.pem");

        generate_self_signed_cert(&cert_path, &key_path).unwrap();
        let mode = fs::metadata(&key_path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "private key must be 0600; got {mode:o}");
    }

    #[cfg(unix)]
    #[test]
    fn generated_key_parent_dir_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TempDir::new().unwrap();
        // Use a fresh subdirectory so we test the dir tightening on a
        // directory the helper itself created, not the TempDir root.
        let tls_dir = dir.path().join("tls");
        let cert_path = tls_dir.join("cert.pem");
        let key_path = tls_dir.join("key.pem");

        generate_self_signed_cert(&cert_path, &key_path).unwrap();

        let mode = fs::metadata(&tls_dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode, 0o700,
            "tls dir holding the private key must be 0700; got {mode:o}",
        );
    }

    #[cfg(unix)]
    #[test]
    fn regenerating_over_existing_loose_key_still_tightens_perms() {
        // Defense-in-depth: if a stale, world-readable key happens to exist
        // (e.g. left over from an older version), regenerating must end with
        // mode 0o600, not leave the looser bits in place.
        use std::os::unix::fs::PermissionsExt;
        let dir = TempDir::new().unwrap();
        let cert_path = dir.path().join("cert.pem");
        let key_path = dir.path().join("key.pem");
        fs::write(&key_path, b"old-loose-key").unwrap();
        fs::set_permissions(&key_path, fs::Permissions::from_mode(0o644)).unwrap();

        generate_self_signed_cert(&cert_path, &key_path).unwrap();
        let mode = fs::metadata(&key_path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "private key must be 0600; got {mode:o}");
    }
}
