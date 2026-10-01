//! Post-quantum keys and signatures, algorithm-agile by design.
//!
//! Every public key and signature carries a [`Scheme`] tag, so the chain can
//! adopt new schemes (FN-DSA once FIPS 206 is final) without a format change.
//! Only schemes listed in [`Scheme`] exist in the code; a tag for anything
//! else fails to decode.
//!
//! Adapted from Sigilant's `sigilant-token::keys`: hedged signing, seed-derived
//! keys, and errors that never tell a forger *how* a forgery failed.

use std::fmt;
use std::io::{Read, Write};
use std::str::FromStr;
use std::sync::Arc;

use aws_lc_rs::signature::{
    KeyPair, ML_DSA_44, ML_DSA_44_SIGNING, ML_DSA_65, ML_DSA_65_SIGNING, ParsedPublicKey,
    PqdsaKeyPair, PqdsaSigningAlgorithm, PqdsaVerificationAlgorithm,
};
use borsh::{BorshDeserialize, BorshSerialize};
use rayon::prelude::*;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::address::Address;

/// Length of an ML-DSA key-generation seed (FIPS 204 `ξ`).
pub const SEED_LEN: usize = 32;

/// Prefix of every signed message. AWS-LC exposes pure ML-DSA with the empty
/// FIPS 204 context string, so domain separation is done here instead: the
/// prefix plus a [`Domain`] byte means a signature made for one purpose can
/// never verify as another.
const SIGNING_PREFIX: &[u8; 12] = b"STEIN-SIG-v1";

/// A signature scheme. The discriminant is the on-chain tag (SPEC §3.4).
///
/// Tag 3 is FN-DSA-512 per the **draft** FIPS 206. It exists only in builds
/// with the `fn-dsa-experimental` feature, for the FN-DSA test network
/// (SPEC §3.5); in every other build tag 3 does not decode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
#[repr(u8)]
pub enum Scheme {
    /// ML-DSA-44 (FIPS 204), NIST category 2. Devnet default for accounts.
    #[default]
    MlDsa44 = 1,
    /// ML-DSA-65 (FIPS 204), NIST category 3.
    MlDsa65 = 2,
    /// FN-DSA-512 (Falcon), draft FIPS 206, NIST category 1. EXPERIMENTAL:
    /// test network only, never mainnet.
    #[cfg(feature = "fn-dsa-experimental")]
    FnDsa512Draft = 3,
}

impl Scheme {
    /// Every scheme this build supports.
    #[cfg(not(feature = "fn-dsa-experimental"))]
    pub const ALL: [Scheme; 2] = [Scheme::MlDsa44, Scheme::MlDsa65];
    /// Every scheme this build supports.
    #[cfg(feature = "fn-dsa-experimental")]
    pub const ALL: [Scheme; 3] = [Scheme::MlDsa44, Scheme::MlDsa65, Scheme::FnDsa512Draft];

    /// FN-DSA-512 (draft FIPS 206) if this build includes it, else `None`.
    /// Lets the rest of the workspace offer the FN-DSA test network without
    /// feature flags of its own: a standard build simply has no such scheme.
    pub const fn fn_dsa_draft() -> Option<Scheme> {
        #[cfg(feature = "fn-dsa-experimental")]
        {
            Some(Scheme::FnDsa512Draft)
        }
        #[cfg(not(feature = "fn-dsa-experimental"))]
        {
            None
        }
    }

    /// Whether the scheme is experimental: never acceptable for mainnet.
    pub const fn is_experimental(self) -> bool {
        match self {
            Self::MlDsa44 | Self::MlDsa65 => false,
            #[cfg(feature = "fn-dsa-experimental")]
            Self::FnDsa512Draft => true,
        }
    }

    /// The on-chain tag.
    pub const fn tag(self) -> u8 {
        self as u8
    }

    /// The scheme with on-chain tag `tag`.
    pub fn from_tag(tag: u8) -> Option<Self> {
        Self::ALL.into_iter().find(|s| s.tag() == tag)
    }

    /// Canonical name, as used in key files and configuration.
    pub const fn name(self) -> &'static str {
        match self {
            Self::MlDsa44 => "ml-dsa-44",
            Self::MlDsa65 => "ml-dsa-65",
            #[cfg(feature = "fn-dsa-experimental")]
            Self::FnDsa512Draft => "fn-dsa-512-draft",
        }
    }

    /// Encoded public-key length (FIPS 204 Table 2).
    pub const fn public_key_len(self) -> usize {
        match self {
            Self::MlDsa44 => 1312,
            Self::MlDsa65 => 1952,
            #[cfg(feature = "fn-dsa-experimental")]
            Self::FnDsa512Draft => crate::fn_dsa_draft::PUBLIC_KEY_LEN,
        }
    }

    /// Signature length (FIPS 204 Table 2).
    pub const fn signature_len(self) -> usize {
        match self {
            Self::MlDsa44 => 2420,
            Self::MlDsa65 => 3309,
            #[cfg(feature = "fn-dsa-experimental")]
            Self::FnDsa512Draft => crate::fn_dsa_draft::SIGNATURE_LEN,
        }
    }

    /// The AWS-LC algorithms for ML-DSA schemes; `None` for FN-DSA.
    fn aws_lc(
        self,
    ) -> Option<(
        &'static PqdsaSigningAlgorithm,
        &'static PqdsaVerificationAlgorithm,
    )> {
        match self {
            Self::MlDsa44 => Some((&ML_DSA_44_SIGNING, &ML_DSA_44)),
            Self::MlDsa65 => Some((&ML_DSA_65_SIGNING, &ML_DSA_65)),
            #[cfg(feature = "fn-dsa-experimental")]
            Self::FnDsa512Draft => None,
        }
    }
}

impl fmt::Display for Scheme {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

impl FromStr for Scheme {
    type Err = KeyError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|x| x.name() == s)
            .ok_or(KeyError::UnsupportedScheme)
    }
}

impl BorshSerialize for Scheme {
    fn serialize<W: Write>(&self, writer: &mut W) -> std::io::Result<()> {
        writer.write_all(&[self.tag()])
    }
}

impl BorshDeserialize for Scheme {
    fn deserialize_reader<R: Read>(reader: &mut R) -> std::io::Result<Self> {
        let mut tag = [0u8; 1];
        reader.read_exact(&mut tag)?;
        Self::from_tag(tag[0]).ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, "unsupported scheme")
        })
    }
}

impl Serialize for Scheme {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.name())
    }
}

impl<'de> Deserialize<'de> for Scheme {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        <String as Deserialize>::deserialize(d)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}

/// What a signature authorises. Part of the signed message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Domain {
    /// A user transaction.
    Transaction = 1,
    /// A block proposal by the round's leader.
    Proposal = 2,
    /// A consensus vote (prevote/precommit).
    Vote = 3,
}

fn signing_message(domain: Domain, payload: &[u8]) -> Vec<u8> {
    let mut message = Vec::with_capacity(payload.len().saturating_add(13));
    message.extend_from_slice(SIGNING_PREFIX);
    message.push(domain as u8);
    message.extend_from_slice(payload);
    message
}

/// Why a key could not be made or used.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum KeyError {
    /// The encoded key or signature is not the length its scheme requires.
    #[error("expected {expected} bytes, got {got}")]
    WrongLength {
        /// Required length.
        expected: usize,
        /// Supplied length.
        got: usize,
    },
    /// The scheme tag or name is not one this build supports.
    #[error("unsupported signature scheme")]
    UnsupportedScheme,
    /// The cryptographic library refused the operation.
    #[error("the cryptographic library refused the operation")]
    Crypto,
}

/// A signature did not verify. Carries no detail by design.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("signature verification failed")]
pub struct InvalidSignature;

macro_rules! tagged_bytes {
    ($name:ident, $len:ident, $doc:literal) => {
        #[doc = $doc]
        ///
        /// Wire form: one scheme tag byte, then exactly the scheme's length of
        /// bytes. No length prefix, so each value has exactly one encoding.
        #[derive(Clone, PartialEq, Eq, Hash)]
        pub struct $name {
            scheme: Scheme,
            bytes: Box<[u8]>,
        }

        impl $name {
            /// Wraps encoded bytes for `scheme`, checking only the length.
            pub fn from_bytes(scheme: Scheme, bytes: &[u8]) -> Result<Self, KeyError> {
                if bytes.len() != scheme.$len() {
                    return Err(KeyError::WrongLength {
                        expected: scheme.$len(),
                        got: bytes.len(),
                    });
                }
                Ok(Self {
                    scheme,
                    bytes: bytes.into(),
                })
            }

            /// The scheme.
            pub fn scheme(&self) -> Scheme {
                self.scheme
            }

            /// The encoded bytes, without the tag.
            pub fn as_bytes(&self) -> &[u8] {
                &self.bytes
            }

            /// Length on the wire, tag included.
            pub fn encoded_len(&self) -> usize {
                self.bytes.len().saturating_add(1)
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(
                    f,
                    "{}({}, {}…)",
                    stringify!($name),
                    self.scheme,
                    hex::encode(&self.bytes[..8])
                )
            }
        }

        impl BorshSerialize for $name {
            fn serialize<W: Write>(&self, writer: &mut W) -> std::io::Result<()> {
                writer.write_all(&[self.scheme.tag()])?;
                writer.write_all(&self.bytes)
            }
        }

        impl BorshDeserialize for $name {
            fn deserialize_reader<R: Read>(reader: &mut R) -> std::io::Result<Self> {
                let scheme = Scheme::deserialize_reader(reader)?;
                let mut bytes = vec![0u8; scheme.$len()];
                reader.read_exact(&mut bytes)?;
                Ok(Self {
                    scheme,
                    bytes: bytes.into(),
                })
            }
        }

        /// JSON form: `"<scheme>:<hex>"`.
        impl Serialize for $name {
            fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                s.collect_str(&format_args!(
                    "{}:{}",
                    self.scheme,
                    hex::encode(&self.bytes)
                ))
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                use serde::de::Error;
                let text = <String as Deserialize>::deserialize(d)?;
                let (scheme, hex_part) = text
                    .split_once(':')
                    .ok_or_else(|| D::Error::custom("expected <scheme>:<hex>"))?;
                let scheme: Scheme = scheme.parse().map_err(D::Error::custom)?;
                let bytes = hex::decode(hex_part).map_err(D::Error::custom)?;
                Self::from_bytes(scheme, &bytes).map_err(D::Error::custom)
            }
        }
    };
}

tagged_bytes!(
    PublicKey,
    public_key_len,
    "An encoded public key with its scheme."
);
tagged_bytes!(
    Signature,
    signature_len,
    "An encoded signature with its scheme."
);

impl PublicKey {
    /// The address this key controls.
    pub fn address(&self) -> Address {
        Address::from_public_key(self)
    }
}

/// A public key parsed once and ready to verify many signatures.
///
/// Parsing an ML-DSA key expands its matrix; the node keeps one of these per
/// recently active account so the cost is paid once, not once per transaction.
#[derive(Clone)]
pub struct VerifyingKey {
    parsed: Parsed,
    scheme: Scheme,
    address: Address,
}

#[derive(Clone)]
enum Parsed {
    AwsLc(Arc<ParsedPublicKey>),
    #[cfg(feature = "fn-dsa-experimental")]
    FnDsa(crate::fn_dsa_draft::SharedVerifier),
}

impl VerifyingKey {
    /// Parses `key`.
    pub fn new(key: &PublicKey) -> Result<Self, KeyError> {
        let parsed = match key.scheme.aws_lc() {
            Some((_, verification)) => Parsed::AwsLc(Arc::new(
                ParsedPublicKey::new(verification, key.as_bytes()).map_err(|_| KeyError::Crypto)?,
            )),
            #[cfg(feature = "fn-dsa-experimental")]
            None => Parsed::FnDsa(Arc::new(crate::fn_dsa_draft::Verifier::new(
                key.as_bytes(),
            )?)),
            #[cfg(not(feature = "fn-dsa-experimental"))]
            None => return Err(KeyError::UnsupportedScheme),
        };
        Ok(Self {
            parsed,
            scheme: key.scheme,
            address: key.address(),
        })
    }

    /// The address of the key. Also its fingerprint: it commits to scheme and key.
    pub fn address(&self) -> Address {
        self.address
    }

    /// The key's scheme.
    pub fn scheme(&self) -> Scheme {
        self.scheme
    }

    /// Verifies `signature` over `payload` in `domain`. A signature of a
    /// different scheme than the key never verifies.
    pub fn verify(
        &self,
        domain: Domain,
        payload: &[u8],
        signature: &Signature,
    ) -> Result<(), InvalidSignature> {
        if signature.scheme != self.scheme {
            return Err(InvalidSignature);
        }
        let message = signing_message(domain, payload);
        let ok = match &self.parsed {
            Parsed::AwsLc(key) => key.verify_sig(&message, signature.as_bytes()).is_ok(),
            #[cfg(feature = "fn-dsa-experimental")]
            Parsed::FnDsa(key) => key.verify(&message, signature.as_bytes()),
        };
        if ok { Ok(()) } else { Err(InvalidSignature) }
    }
}

impl fmt::Debug for VerifyingKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VerifyingKey")
            .field("scheme", &self.scheme)
            .field("address", &self.address)
            .finish_non_exhaustive()
    }
}

/// Verifies many signatures across all cores. On failure, returns the index of
/// *a* failing item (not necessarily the first).
pub fn verify_batch(jobs: &[(&VerifyingKey, Domain, &[u8], &Signature)]) -> Result<(), usize> {
    match jobs
        .par_iter()
        .position_any(|(key, domain, payload, sig)| key.verify(*domain, payload, sig).is_err())
    {
        Some(index) => Err(index),
        None => Ok(()),
    }
}

/// A private key, held in memory.
pub struct SecretKey {
    signer: Signer,
    public: PublicKey,
    verifying: VerifyingKey,
}

enum Signer {
    AwsLc(PqdsaKeyPair),
    #[cfg(feature = "fn-dsa-experimental")]
    FnDsa(crate::fn_dsa_draft::Signer),
}

impl SecretKey {
    /// Generates a fresh key from the operating system's random source.
    pub fn generate(scheme: Scheme) -> Result<Self, KeyError> {
        let mut seed = [0u8; SEED_LEN];
        aws_lc_rs::rand::fill(&mut seed).map_err(|_| KeyError::Crypto)?;
        let key = Self::from_seed(scheme, &seed);
        zeroize::Zeroize::zeroize(&mut seed);
        key
    }

    /// Derives a key deterministically from a 32-byte seed (for ML-DSA, the
    /// FIPS 204 seed). The seed *is* the private key.
    pub fn from_seed(scheme: Scheme, seed: &[u8; SEED_LEN]) -> Result<Self, KeyError> {
        let (signer, public) = match scheme.aws_lc() {
            Some((signing, _)) => {
                let pair = PqdsaKeyPair::from_seed(signing, seed).map_err(|_| KeyError::Crypto)?;
                let public = pair.public_key().as_ref().to_vec();
                (Signer::AwsLc(pair), public)
            }
            #[cfg(feature = "fn-dsa-experimental")]
            None => {
                let (signer, public) = crate::fn_dsa_draft::Signer::from_seed(seed)?;
                (Signer::FnDsa(signer), public)
            }
            #[cfg(not(feature = "fn-dsa-experimental"))]
            None => return Err(KeyError::UnsupportedScheme),
        };
        let public = PublicKey::from_bytes(scheme, &public)?;
        let verifying = VerifyingKey::new(&public)?;
        Ok(Self {
            signer,
            public,
            verifying,
        })
    }

    /// The key's scheme.
    pub fn scheme(&self) -> Scheme {
        self.public.scheme
    }

    /// The public key.
    pub fn public_key(&self) -> &PublicKey {
        &self.public
    }

    /// The parsed public key.
    pub fn verifying_key(&self) -> &VerifyingKey {
        &self.verifying
    }

    /// The address this key controls.
    pub fn address(&self) -> Address {
        self.verifying.address
    }

    /// Signs `payload` in `domain`.
    ///
    /// Signing is *hedged* (FIPS 204 default): fresh randomness is mixed in, so
    /// two signatures over the same payload differ. Nothing on chain may
    /// therefore be identified by a hash that covers a signature.
    pub fn sign(&self, domain: Domain, payload: &[u8]) -> Result<Signature, KeyError> {
        let scheme = self.scheme();
        let message = signing_message(domain, payload);
        let signature = match &self.signer {
            Signer::AwsLc(pair) => {
                let mut signature = vec![0u8; scheme.signature_len()];
                let written = pair
                    .sign(&message, &mut signature)
                    .map_err(|_| KeyError::Crypto)?;
                if written != signature.len() {
                    return Err(KeyError::Crypto);
                }
                signature
            }
            #[cfg(feature = "fn-dsa-experimental")]
            Signer::FnDsa(signer) => signer.sign(&message)?,
        };
        Ok(Signature {
            scheme,
            bytes: signature.into(),
        })
    }
}

impl fmt::Debug for SecretKey {
    /// Never prints key material.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SecretKey")
            .field("scheme", &self.scheme())
            .field("address", &self.address())
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(scheme: Scheme, n: u8) -> SecretKey {
        SecretKey::from_seed(scheme, &[n; SEED_LEN]).expect("seed key")
    }

    #[test]
    fn lengths_match_fips_204() {
        for scheme in Scheme::ALL {
            let k = key(scheme, 1);
            assert_eq!(k.public_key().as_bytes().len(), scheme.public_key_len());
            let sig = k.sign(Domain::Transaction, b"m").expect("sign");
            assert_eq!(sig.as_bytes().len(), scheme.signature_len());
            assert_eq!(borsh::to_vec(&sig).expect("enc").len(), sig.encoded_len());
        }
        assert_eq!(
            (
                Scheme::MlDsa44.public_key_len(),
                Scheme::MlDsa44.signature_len()
            ),
            (1312, 2420)
        );
        assert_eq!(
            (
                Scheme::MlDsa65.public_key_len(),
                Scheme::MlDsa65.signature_len()
            ),
            (1952, 3309)
        );
    }

    #[test]
    fn a_signature_verifies_only_for_its_key_payload_and_domain() {
        for scheme in Scheme::ALL {
            let (a, b) = (key(scheme, 1), key(scheme, 2));
            let sig = a.sign(Domain::Transaction, b"pay 5").expect("sign");
            let vk = a.verifying_key();
            assert!(vk.verify(Domain::Transaction, b"pay 5", &sig).is_ok());
            assert!(vk.verify(Domain::Transaction, b"pay 6", &sig).is_err());
            assert!(vk.verify(Domain::Vote, b"pay 5", &sig).is_err());
            assert!(
                b.verifying_key()
                    .verify(Domain::Transaction, b"pay 5", &sig)
                    .is_err()
            );
        }
    }

    #[test]
    fn schemes_never_cross() {
        let (a44, a65) = (key(Scheme::MlDsa44, 1), key(Scheme::MlDsa65, 1));
        assert_ne!(
            a44.address(),
            a65.address(),
            "same seed, different scheme, different account"
        );
        let sig = a44.sign(Domain::Transaction, b"m").expect("sign");
        assert_eq!(
            a65.verifying_key().verify(Domain::Transaction, b"m", &sig),
            Err(InvalidSignature)
        );
        // Relabelling a signature's scheme tag cannot make it verify either.
        let relabelled = Signature {
            scheme: Scheme::MlDsa65,
            bytes: sig.bytes.clone(),
        };
        assert_eq!(
            a44.verifying_key()
                .verify(Domain::Transaction, b"m", &relabelled),
            Err(InvalidSignature)
        );
    }

    #[cfg(not(feature = "fn-dsa-experimental"))]
    #[test]
    fn a_standard_build_has_no_fn_dsa() {
        assert_eq!(Scheme::fn_dsa_draft(), None);
        assert!("fn-dsa-512-draft".parse::<Scheme>().is_err());
        assert_eq!(Scheme::from_tag(3), None);
    }

    #[cfg(feature = "fn-dsa-experimental")]
    #[test]
    fn fn_dsa_draft_keys_sign_and_verify() {
        let fn_dsa = Scheme::fn_dsa_draft().expect("experimental build");
        assert_eq!((fn_dsa.tag(), fn_dsa.name()), (3, "fn-dsa-512-draft"));
        assert!(fn_dsa.is_experimental() && !Scheme::MlDsa44.is_experimental());
        assert_eq!(
            (fn_dsa.public_key_len(), fn_dsa.signature_len()),
            (897, 666)
        );

        let (a, b) = (key(fn_dsa, 1), key(fn_dsa, 2));
        assert_eq!(
            key(fn_dsa, 1).public_key(),
            a.public_key(),
            "seeds are deterministic"
        );
        assert_ne!(a.address(), key(Scheme::MlDsa44, 1).address());

        let sig = a.sign(Domain::Transaction, b"pay 5").expect("sign");
        let again = a.sign(Domain::Transaction, b"pay 5").expect("sign");
        assert_ne!(sig, again, "signing is randomised");
        let vk = a.verifying_key();
        assert!(vk.verify(Domain::Transaction, b"pay 5", &sig).is_ok());
        assert!(vk.verify(Domain::Transaction, b"pay 5", &again).is_ok());
        assert!(vk.verify(Domain::Transaction, b"pay 6", &sig).is_err());
        assert!(vk.verify(Domain::Vote, b"pay 5", &sig).is_err());
        assert!(
            b.verifying_key()
                .verify(Domain::Transaction, b"pay 5", &sig)
                .is_err()
        );

        // Wire round trip with its tag.
        let wire = borsh::to_vec(a.public_key()).expect("enc");
        assert_eq!(wire[0], 3);
        assert_eq!(
            &PublicKey::try_from_slice(&wire).expect("dec"),
            a.public_key()
        );
    }

    #[cfg(feature = "fn-dsa-experimental")]
    #[test]
    fn fn_dsa_and_ml_dsa_never_cross() {
        let fn_dsa = Scheme::fn_dsa_draft().expect("experimental build");
        let (f, m) = (key(fn_dsa, 5), key(Scheme::MlDsa44, 5));
        let f_sig = f.sign(Domain::Transaction, b"m").expect("sign");
        let m_sig = m.sign(Domain::Transaction, b"m").expect("sign");
        assert!(
            m.verifying_key()
                .verify(Domain::Transaction, b"m", &f_sig)
                .is_err()
        );
        assert!(
            f.verifying_key()
                .verify(Domain::Transaction, b"m", &m_sig)
                .is_err()
        );
    }

    #[test]
    fn unknown_scheme_tags_do_not_decode() {
        let k = key(Scheme::MlDsa44, 3);
        let mut wire = borsh::to_vec(k.public_key()).expect("enc");
        assert_eq!(
            PublicKey::try_from_slice(&wire).expect("dec"),
            *k.public_key()
        );
        wire[0] = 4; // unassigned
        assert!(PublicKey::try_from_slice(&wire).is_err());
        assert!("fn-dsa-512".parse::<Scheme>().is_err());
    }

    #[test]
    fn signing_is_hedged() {
        let k = key(Scheme::MlDsa44, 3);
        let s1 = k.sign(Domain::Transaction, b"same").expect("sign");
        let s2 = k.sign(Domain::Transaction, b"same").expect("sign");
        assert_ne!(s1, s2);
    }

    #[test]
    fn a_tampered_signature_fails() {
        let k = key(Scheme::MlDsa44, 6);
        let sig = k.sign(Domain::Transaction, b"m").expect("sign");
        let mut bytes = sig.as_bytes().to_vec();
        bytes[100] ^= 1;
        let bad = Signature::from_bytes(Scheme::MlDsa44, &bytes).expect("length");
        assert_eq!(
            k.verifying_key().verify(Domain::Transaction, b"m", &bad),
            Err(InvalidSignature)
        );
    }

    #[test]
    fn batch_verification_finds_the_bad_one() {
        let keys: Vec<SecretKey> = (10..20).map(|n| key(Scheme::MlDsa44, n)).collect();
        let msgs: Vec<Vec<u8>> = (0..10u8).map(|i| vec![i; 40]).collect();
        let mut sigs: Vec<Signature> = keys
            .iter()
            .zip(&msgs)
            .map(|(k, m)| k.sign(Domain::Transaction, m).expect("sign"))
            .collect();
        let run = |sigs: &[Signature]| -> Result<(), usize> {
            let jobs: Vec<_> = keys
                .iter()
                .zip(&msgs)
                .zip(sigs)
                .map(|((k, m), s)| (k.verifying_key(), Domain::Transaction, m.as_slice(), s))
                .collect();
            verify_batch(&jobs)
        };
        assert_eq!(run(&sigs), Ok(()));
        sigs[7] = sigs[6].clone();
        assert_eq!(run(&sigs), Err(7));
    }

    #[test]
    fn json_form_names_the_scheme() {
        let k = key(Scheme::MlDsa44, 8);
        let json = serde_json::to_string(k.public_key()).expect("json");
        assert!(json.starts_with("\"ml-dsa-44:"));
        assert_eq!(
            &serde_json::from_str::<PublicKey>(&json).expect("json"),
            k.public_key()
        );
        assert!(PublicKey::from_bytes(Scheme::MlDsa44, &[0u8; 10]).is_err());
    }
}
