//! Stein Chain's one TLS policy, adapted from Sigilant's `sigilant-tls`. Every
//! TLS client (and, in phase 3, every listener) gets its configuration here and
//! nowhere else.
//!
//! | | Enabled | Disabled |
//! |---|---|---|
//! | Protocol | TLS 1.3 | TLS 1.2 and below — not compiled into the binary |
//! | Cipher suite | `TLS_AES_256_GCM_SHA384` | everything else |
//! | Key exchange | `X25519MLKEM768`, `MLKEM1024`, `MLKEM768` | classical groups on their own |
//!
//! Resistance to *harvest now, decrypt later* comes from the key-exchange
//! group, so that is where it is enforced: a peer offering only classical
//! groups cannot connect. There is no fallback, because a fallback is exactly
//! what a downgrade attack would select. Cloudflare's edge, which carries the
//! devnet follower's traffic through a tunnel, negotiates `X25519MLKEM768`.
//!
//! Certificate *authentication* is not post-quantum yet: public CAs do not
//! issue post-quantum certificates. Authentication must be broken at connection
//! time, not years later, so it is not exposed to harvest-now-decrypt-later.

use std::sync::Arc;

use rustls::crypto::CryptoProvider;
use rustls::crypto::aws_lc_rs::{cipher_suite, default_provider, kx_group};
use rustls::{ClientConfig, RootCertStore};

pub use rustls;

/// AWS-LC, restricted to one cipher suite and post-quantum key exchange.
pub fn provider() -> Arc<CryptoProvider> {
    Arc::new(CryptoProvider {
        cipher_suites: vec![cipher_suite::TLS13_AES_256_GCM_SHA384],
        kx_groups: vec![
            kx_group::X25519MLKEM768,
            kx_group::MLKEM1024,
            kx_group::MLKEM768,
        ],
        ..default_provider()
    })
}

/// A client configuration under the policy, trusting the Mozilla root store.
pub fn client_config() -> Result<ClientConfig, rustls::Error> {
    let roots = RootCertStore {
        roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
    };
    Ok(ClientConfig::builder_with_provider(provider())
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .with_root_certificates(roots)
        .with_no_client_auth())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_post_quantum_groups_and_one_suite() {
        let p = provider();
        assert_eq!(p.cipher_suites.len(), 1);
        let names: Vec<String> = p
            .kx_groups
            .iter()
            .map(|g| format!("{:?}", g.name()))
            .collect();
        assert_eq!(names, ["X25519MLKEM768", "MLKEM1024", "MLKEM768"]);
        assert!(client_config().is_ok());
    }
}
