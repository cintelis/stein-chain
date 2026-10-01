//! Signing keys kept as a FIPS 204 seed in a file.
//!
//! Adapted from Sigilant's `sigilant-token::seed_file`. For a devnet or a
//! validator without an HSM. Production validators should keep the seed in an
//! HSM or remote signer (SPEC §9). The seed *is* the private key: the file must
//! be readable by its owner alone, and the seed is wiped from memory as soon as
//! the key has been made from it.
//!
//! File format: `<scheme>:<64 hex characters>`, e.g. `ml-dsa-44:9f…`. A bare
//! 64-character hex seed (the format before schemes) is read as ML-DSA-65.

use std::path::Path;

use zeroize::Zeroize;

use crate::keys::{SEED_LEN, Scheme, SecretKey};

/// Why a seed file could not become a key.
#[derive(Debug, thiserror::Error)]
pub enum SeedFileError {
    /// The file could not be read or written.
    #[error("seed file I/O: {0}")]
    Io(#[from] std::io::Error),
    /// The file is readable by users other than its owner.
    #[error("seed file has mode {0:o}; it must not be accessible to group or others (chmod 600)")]
    TooPermissive(u32),
    /// The file is not `<scheme>:<64 hex characters>`.
    #[error("seed file must hold <scheme>:<{} hex characters>", SEED_LEN * 2)]
    Malformed,
    /// The cryptographic library refused the key.
    #[error("the cryptographic library refused the key")]
    Crypto,
}

/// The key whose seed is in `path`, hex-encoded.
pub fn secret_key_from_seed_file(path: &Path) -> Result<SecretKey, SeedFileError> {
    check_permissions(path)?;
    let mut text = std::fs::read_to_string(path)?;
    let mut seed = [0u8; SEED_LEN];
    let (scheme, hex_part) = match text.trim().split_once(':') {
        Some((name, hex_part)) => (name.parse::<Scheme>().ok(), hex_part),
        None => (Some(Scheme::MlDsa65), text.trim()),
    };
    let parsed = match scheme {
        Some(scheme) => hex::decode_to_slice(hex_part, &mut seed)
            .map(|()| scheme)
            .map_err(|_| SeedFileError::Malformed),
        None => Err(SeedFileError::Malformed),
    };
    text.zeroize();
    let key = parsed
        .and_then(|scheme| SecretKey::from_seed(scheme, &seed).map_err(|_| SeedFileError::Crypto));
    seed.zeroize();
    key
}

/// Makes a new key and keeps its seed in `path`, readable by its owner alone.
/// Refuses to replace a file that exists: a key overwritten is a key lost, and
/// with it every coin the key controlled.
pub fn create_seed_file(path: &Path, scheme: Scheme) -> Result<SecretKey, SeedFileError> {
    use std::io::Write;

    let mut seed = [0u8; SEED_LEN];
    aws_lc_rs::rand::fill(&mut seed).map_err(|_| SeedFileError::Crypto)?;
    let key = SecretKey::from_seed(scheme, &seed).map_err(|_| SeedFileError::Crypto);
    let mut text = format!("{scheme}:{}", hex::encode(seed));
    seed.zeroize();
    let key = key?;

    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let written = options.open(path).and_then(|mut file| {
        file.write_all(text.as_bytes())
            .and_then(|()| file.sync_all())
    });
    text.zeroize();
    written?;
    Ok(key)
}

#[cfg(unix)]
fn check_permissions(path: &Path) -> Result<(), SeedFileError> {
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(path)?.permissions().mode() & 0o777;
    if mode & 0o077 != 0 {
        return Err(SeedFileError::TooPermissive(mode));
    }
    Ok(())
}

/// Windows permissions are ACLs and cannot be judged from a mode word. Windows
/// is a development platform here, not a validator platform.
#[cfg(not(unix))]
fn check_permissions(_: &Path) -> Result<(), SeedFileError> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_seed_file_holds_its_key_and_is_never_overwritten() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("validator.seed");
        let made = create_seed_file(&path, Scheme::MlDsa44).expect("create");
        let loaded = secret_key_from_seed_file(&path).expect("load");
        assert_eq!(made.public_key(), loaded.public_key());
        assert_eq!(loaded.scheme(), Scheme::MlDsa44);
        assert!(matches!(
            create_seed_file(&path, Scheme::MlDsa44),
            Err(SeedFileError::Io(_))
        ));
        assert_eq!(
            secret_key_from_seed_file(&path).expect("load").address(),
            made.address()
        );
    }

    #[test]
    fn a_bare_hex_seed_is_read_as_ml_dsa_65() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("old.seed");
        std::fs::write(&path, "07".repeat(32)).expect("write");
        let key = secret_key_from_seed_file(&path).expect("load");
        assert_eq!(key.scheme(), Scheme::MlDsa65);
        assert_eq!(
            key.public_key(),
            SecretKey::from_seed(Scheme::MlDsa65, &[7; 32])
                .expect("key")
                .public_key()
        );
    }

    #[test]
    fn a_malformed_seed_file_is_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("bad.seed");
        std::fs::write(&path, "ml-dsa-44:not hex").expect("write");
        assert!(matches!(
            secret_key_from_seed_file(&path),
            Err(SeedFileError::Malformed)
        ));
    }
}
