//! Transactions: a small body that is executed, and a large witness (ML-DSA
//! signature, plus the public key on an account's first transaction) that
//! authorises it.

use borsh::{BorshDeserialize, BorshSerialize};
use serde::{Deserialize, Serialize};
use stein_crypto::{Address, Domain, Hash, KeyError, PublicKey, SecretKey, Signature, hash};

use crate::{Amount, ChainId, ChainParams, Height, Witness};

/// What a transaction does.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, BorshSerialize, BorshDeserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Action {
    /// Move STEIN to another account.
    Transfer {
        /// Recipient.
        to: Address,
        /// Base units to move. Must be non-zero.
        amount: Amount,
    },
}

/// The part of a transaction that is executed and kept forever.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, BorshSerialize, BorshDeserialize)]
pub struct TxBody {
    /// Format version ([`TxBody::VERSION`]). Bumped only by protocol upgrades;
    /// a node rejects versions it does not know (SHARD-READY I-7).
    pub version: u8,
    /// Network this transaction is valid on.
    pub chain_id: ChainId,
    /// Paying and authorising account.
    pub sender: Address,
    /// Must equal the sender's account nonce. Prevents replay.
    pub nonce: u64,
    /// Total fee offered, in base units.
    pub fee: Amount,
    /// Last height at which this transaction may be included.
    pub expires_at: Height,
    /// The operation.
    pub action: Action,
}

const TXID_CONTEXT: &str = "stein txid v1";

impl TxBody {
    /// The transaction format this software produces and accepts.
    pub const VERSION: u8 = 1;

    /// Canonical encoding: what is signed and what the id covers.
    pub fn encode(&self) -> Vec<u8> {
        borsh::to_vec(self).unwrap_or_default()
    }

    /// The transaction id. Covers the body only: ML-DSA signing is hedged, so
    /// one body has many valid signatures, and an id that covered the witness
    /// would let one payment appear under several ids. An id over the body is
    /// immune to witness malleability, and nonces make it unique per payment.
    pub fn id(&self) -> Hash {
        hash(TXID_CONTEXT, &[&self.encode()])
    }
}

/// A signed transaction, as gossiped and submitted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, BorshSerialize, BorshDeserialize)]
pub struct Transaction {
    /// What to execute.
    pub body: TxBody,
    /// Who authorised it.
    pub witness: Witness,
}

/// Why a transaction was rejected.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TxError {
    /// A transaction format this node does not support.
    #[error("unsupported transaction version {0}")]
    UnsupportedVersion(u8),
    /// Signed for another network.
    #[error("wrong chain id: expected {expected}, got {got}")]
    WrongChain {
        /// This network.
        expected: ChainId,
        /// The transaction's.
        got: ChainId,
    },
    /// The attached public key does not hash to the sender address.
    #[error("public key does not belong to the sender")]
    KeyMismatch,
    /// No key attached and none registered for the sender.
    #[error("sender has no registered key; attach the public key to the first transaction")]
    KeyMissing,
    /// A key is attached although one is already registered.
    #[error("sender's key is already registered; do not attach it again")]
    KeyAlreadyRegistered,
    /// The key or signature uses a scheme this chain does not accept.
    #[error("signature scheme not accepted on this chain")]
    UnsupportedScheme,
    /// The public key could not be parsed.
    #[error("malformed public key")]
    BadKey,
    /// The signature does not verify.
    #[error("invalid signature")]
    BadSignature,
    /// Nonce is not the account's next nonce.
    #[error("bad nonce: expected {expected}, got {got}")]
    BadNonce {
        /// The account's next nonce.
        expected: u64,
        /// The transaction's.
        got: u64,
    },
    /// Fee below the per-byte minimum.
    #[error("fee too low: need at least {required}")]
    FeeTooLow {
        /// Minimum acceptable fee.
        required: Amount,
    },
    /// Balance cannot cover amount plus fee.
    #[error("insufficient balance")]
    InsufficientBalance,
    /// The transaction has expired or sets an expiry too far ahead.
    #[error("expired or expiry too far in the future")]
    Expiry,
    /// A transfer of zero.
    #[error("amount must be non-zero")]
    ZeroAmount,
    /// Arithmetic would overflow.
    #[error("amount overflow")]
    Overflow,
}

impl Transaction {
    /// Signs `body` with `key`. Attach the public key (`register_key`) only on
    /// the sender's first transaction.
    pub fn sign(body: TxBody, key: &SecretKey, register_key: bool) -> Result<Self, KeyError> {
        let signature = key.sign(Domain::Transaction, &body.encode())?;
        let public_key = register_key.then(|| key.public_key().clone());
        Ok(Self {
            body,
            witness: Witness {
                public_key,
                signature,
            },
        })
    }

    /// Encoded size in bytes: what fees and block limits are measured in.
    pub fn encoded_len(&self) -> u64 {
        let key = self
            .witness
            .public_key
            .as_ref()
            .map_or(0, PublicKey::encoded_len);
        // body + option tag + key (tagged) + signature (tagged)
        [
            self.body.encode().len(),
            1,
            key,
            self.witness.signature.encoded_len(),
        ]
        .iter()
        .fold(0u64, |sum, n| sum.saturating_add(*n as u64))
    }

    /// Minimum fee for this transaction under `params`.
    pub fn min_fee(&self, params: &ChainParams) -> Amount {
        params
            .min_fee_per_byte
            .saturating_mul(Amount::from(self.encoded_len()))
    }

    /// Checks that need no account state and no signature verification. Cheap;
    /// run before anything expensive so junk is dropped at the door.
    pub fn check_stateless(
        &self,
        chain_id: ChainId,
        params: &ChainParams,
        height: Height,
    ) -> Result<(), TxError> {
        if self.body.version != TxBody::VERSION {
            return Err(TxError::UnsupportedVersion(self.body.version));
        }
        if self.body.chain_id != chain_id {
            return Err(TxError::WrongChain {
                expected: chain_id,
                got: self.body.chain_id,
            });
        }
        if self.body.expires_at < height
            || self.body.expires_at > height.saturating_add(params.max_tx_ttl_blocks)
        {
            return Err(TxError::Expiry);
        }
        let required = self.min_fee(params);
        if self.body.fee < required {
            return Err(TxError::FeeTooLow { required });
        }
        match &self.body.action {
            Action::Transfer { amount: 0, .. } => return Err(TxError::ZeroAmount),
            Action::Transfer { .. } => {}
        }
        if !params.tx_schemes.contains(&self.witness.signature.scheme()) {
            return Err(TxError::UnsupportedScheme);
        }
        if let Some(key) = &self.witness.public_key {
            if key.scheme() != self.witness.signature.scheme() {
                return Err(TxError::BadSignature);
            }
            if key.address() != self.body.sender {
                return Err(TxError::KeyMismatch);
            }
        }
        Ok(())
    }

    /// The transaction id (see [`TxBody::id`]).
    pub fn id(&self) -> Hash {
        self.body.id()
    }

    /// The signature.
    pub fn signature(&self) -> &Signature {
        &self.witness.signature
    }

    /// The attached public key, if any.
    pub fn public_key(&self) -> Option<&PublicKey> {
        self.witness.public_key.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> SecretKey {
        SecretKey::from_seed(stein_crypto::Scheme::MlDsa44, &[9; 32]).expect("key")
    }

    fn body(sender: Address) -> TxBody {
        TxBody {
            version: TxBody::VERSION,
            chain_id: 7,
            sender,
            nonce: 0,
            fee: 1_000_000,
            expires_at: 10,
            action: Action::Transfer {
                to: Address([1; 32]),
                amount: 5,
            },
        }
    }

    #[test]
    fn the_id_ignores_the_witness() {
        let k = key();
        let a = Transaction::sign(body(k.address()), &k, true).expect("sign");
        let b = Transaction::sign(body(k.address()), &k, false).expect("sign");
        assert_ne!(a.witness, b.witness);
        assert_eq!(a.id(), b.id());
    }

    #[test]
    fn sizes_reflect_the_key_registration_saving() {
        let k = key();
        let first = Transaction::sign(body(k.address()), &k, true).expect("sign");
        let later = Transaction::sign(body(k.address()), &k, false).expect("sign");
        assert_eq!(first.encoded_len() - later.encoded_len(), 1 + 1312);
        assert_eq!(
            borsh::to_vec(&first).expect("enc").len() as u64,
            first.encoded_len()
        );
        assert_eq!(
            borsh::to_vec(&later).expect("enc").len() as u64,
            later.encoded_len()
        );
    }

    #[test]
    fn stateless_checks() {
        let k = key();
        let p = ChainParams::default();
        let ok = Transaction::sign(body(k.address()), &k, true).expect("sign");
        assert_eq!(ok.check_stateless(7, &p, 5), Ok(()));
        assert!(matches!(
            ok.check_stateless(8, &p, 5),
            Err(TxError::WrongChain { .. })
        ));
        assert_eq!(ok.check_stateless(7, &p, 11), Err(TxError::Expiry));

        let mut stolen = body(Address([2; 32]));
        stolen.nonce = 0;
        let forged = Transaction::sign(stolen, &k, true).expect("sign");
        assert_eq!(forged.check_stateless(7, &p, 5), Err(TxError::KeyMismatch));

        let mut cheap = body(k.address());
        cheap.fee = 1;
        let cheap = Transaction::sign(cheap, &k, false).expect("sign");
        assert!(matches!(
            cheap.check_stateless(7, &p, 5),
            Err(TxError::FeeTooLow { .. })
        ));
    }

    #[test]
    fn only_schemes_the_chain_accepts_are_admitted() {
        let only_44 = ChainParams {
            tx_schemes: vec![stein_crypto::Scheme::MlDsa44],
            ..ChainParams::default()
        };
        let k65 = SecretKey::from_seed(stein_crypto::Scheme::MlDsa65, &[9; 32]).expect("key");
        let mut b = body(k65.address());
        b.fee = 10_000_000;
        let tx = Transaction::sign(b, &k65, true).expect("sign");
        assert_eq!(tx.check_stateless(7, &ChainParams::default(), 5), Ok(()));
        assert_eq!(
            tx.check_stateless(7, &only_44, 5),
            Err(TxError::UnsupportedScheme)
        );
    }
}
