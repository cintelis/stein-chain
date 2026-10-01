//! The genesis document: the network's starting state and rules.

use std::collections::BTreeSet;

use borsh::{BorshDeserialize, BorshSerialize};
use serde::{Deserialize, Serialize};
use stein_crypto::{Address, Hash, PublicKey, VerifyingKey, hash};

use crate::{Amount, ChainId, ChainParams};

const GENESIS_CONTEXT: &str = "stein genesis v1";

/// An initial balance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, BorshSerialize, BorshDeserialize)]
pub struct Allocation {
    /// Account.
    pub address: Address,
    /// Base units.
    pub amount: Amount,
}

/// An initial validator.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, BorshSerialize, BorshDeserialize)]
pub struct GenesisValidator {
    /// Consensus key (ML-DSA-65).
    pub public_key: PublicKey,
    /// Voting power.
    pub power: u64,
}

/// The genesis document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, BorshSerialize, BorshDeserialize)]
pub struct Genesis {
    /// Network id.
    pub chain_id: ChainId,
    /// Timestamp of the genesis state; the first block must be later.
    pub genesis_time_ms: u64,
    /// Consensus rules.
    pub params: ChainParams,
    /// Initial balances.
    pub allocations: Vec<Allocation>,
    /// Initial validator set.
    pub validators: Vec<GenesisValidator>,
}

/// Why a genesis document is unusable.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GenesisError {
    /// No validators.
    #[error("genesis needs at least one validator")]
    NoValidators,
    /// A validator has zero power or a key that does not parse.
    #[error("invalid validator entry")]
    BadValidator,
    /// Two validators share a key.
    #[error("duplicate validator")]
    DuplicateValidator,
    /// Two allocations to one address.
    #[error("duplicate allocation for {0}")]
    DuplicateAllocation(Address),
    /// Total supply overflows.
    #[error("total supply overflows")]
    SupplyOverflow,
}

impl Genesis {
    /// Commits to the whole document. Nodes refuse peers on a different hash.
    pub fn hash(&self) -> Hash {
        hash(GENESIS_CONTEXT, &[&borsh::to_vec(self).unwrap_or_default()])
    }

    /// Total initial supply.
    pub fn total_supply(&self) -> Option<Amount> {
        self.allocations
            .iter()
            .try_fold(0u128, |sum, a| sum.checked_add(a.amount))
    }

    /// Rejects documents a node must not start from.
    pub fn validate(&self) -> Result<(), GenesisError> {
        if self.validators.is_empty() {
            return Err(GenesisError::NoValidators);
        }
        let mut keys = BTreeSet::new();
        for v in &self.validators {
            if v.power == 0
                || !self
                    .params
                    .validator_schemes
                    .contains(&v.public_key.scheme())
                || VerifyingKey::new(&v.public_key).is_err()
            {
                return Err(GenesisError::BadValidator);
            }
            if !keys.insert(v.public_key.address()) {
                return Err(GenesisError::DuplicateValidator);
            }
        }
        let mut seen = BTreeSet::new();
        for a in &self.allocations {
            if !seen.insert(a.address) {
                return Err(GenesisError::DuplicateAllocation(a.address));
            }
        }
        self.total_supply().ok_or(GenesisError::SupplyOverflow)?;
        Ok(())
    }
}
