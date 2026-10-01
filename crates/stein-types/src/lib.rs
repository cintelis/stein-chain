//! Stein Chain wire types. Everything that is hashed or signed is encoded with
//! Borsh, which has exactly one encoding per value; JSON exists only at the RPC
//! edge.

mod amount;
mod block;
mod genesis;
mod params;
mod tx;

pub use amount::{AMOUNT_DECIMALS, Amount, AmountError, ONE_STEIN, format_amount, parse_amount};
pub use block::{Block, BlockHeader, Witness};
pub use genesis::{Allocation, Genesis, GenesisError, GenesisValidator};
pub use params::ChainParams;
pub use tx::{Action, Transaction, TxBody, TxError};

pub use stein_crypto::{Address, Hash, PublicKey, Scheme, Signature};

/// Identifies one network (mainnet, a testnet, a devnet). Signed into every
/// transaction and block so nothing can be replayed across networks.
pub type ChainId = u64;

/// Block height. Genesis state is height 0; the first block is height 1.
pub type Height = u64;
