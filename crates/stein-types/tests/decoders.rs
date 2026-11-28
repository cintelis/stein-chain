//! Property tests of everything that decodes input a node does not control
//! (SPEC §9): transactions, blocks and the genesis document in Borsh and
//! JSON, addresses, assets and amounts in text.
//!
//! What must hold for any input at all:
//!
//! * decoding never panics;
//! * what decodes from Borsh re-encodes to the same bytes, so a value has
//!   exactly one encoding and two nodes cannot hash one value two ways;
//! * what a node encodes, it decodes to the same value.
//!
//! `fuzz/` drives the same decoders with coverage-guided input.

#![allow(clippy::arithmetic_side_effects, clippy::unwrap_used)]

use borsh::{BorshDeserialize, BorshSerialize};
use proptest::collection::vec;
use proptest::prelude::*;
use stein_crypto::{Address, PublicKey, Scheme, Signature};
use stein_types::{
    Action, Allocation, Amount, Asset, Block, BlockHeader, ChainParams, Genesis, GenesisValidator,
    Hash, MAX_TOTAL_SUPPLY, Transaction, TxBody, Witness, format_units, parse_units,
};

// --- Generators -------------------------------------------------------------
//
// Boxed: a strategy's working state otherwise lives on the stack, and nested
// as deeply as a block is, it overflows a test thread's.

fn address() -> impl Strategy<Value = Address> {
    any::<[u8; 32]>().prop_map(Address)
}

fn hash() -> impl Strategy<Value = Hash> {
    any::<[u8; 32]>().prop_map(Hash)
}

fn scheme() -> impl Strategy<Value = Scheme> {
    prop::sample::select(Scheme::ALL.to_vec())
}

/// `len` bytes from a seed. Generating thousands of bytes one by one gives
/// the test runner as many values to shrink, and nothing here depends on any
/// one of them.
fn filled(seed: u64, len: usize) -> Vec<u8> {
    let mut x = seed | 1;
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x >> 32) as u8
        })
        .collect()
}

/// Bytes of the right length for the scheme; not a signature that verifies.
fn signature() -> BoxedStrategy<Signature> {
    (scheme(), any::<u64>())
        .prop_map(|(s, seed)| Signature::from_bytes(s, &filled(seed, s.signature_len())).unwrap())
        .boxed()
}

fn public_key() -> BoxedStrategy<PublicKey> {
    (scheme(), any::<u64>())
        .prop_map(|(s, seed)| PublicKey::from_bytes(s, &filled(seed, s.public_key_len())).unwrap())
        .boxed()
}

/// Any amount, with the edges over-represented.
fn amount() -> BoxedStrategy<Amount> {
    prop_oneof![
        any::<u128>(),
        0..1_000u128,
        Just(u128::MAX),
        Just(MAX_TOTAL_SUPPLY),
        Just(MAX_TOTAL_SUPPLY + 1),
        Just(Amount::from(u64::MAX)),
        Just(Amount::from(u64::MAX) + 1),
    ]
    .boxed()
}

fn asset() -> BoxedStrategy<Asset> {
    prop_oneof![Just(Asset::Stein), address().prop_map(Asset::Token)].boxed()
}

fn symbol() -> BoxedStrategy<String> {
    prop_oneof!["[A-Z][A-Z0-9]{0,11}", ".{0,20}"].boxed()
}

fn action() -> BoxedStrategy<Action> {
    prop_oneof![
        (address(), amount()).prop_map(|(to, amount)| Action::Transfer { to, amount }),
        (symbol(), any::<u8>(), amount()).prop_map(|(symbol, decimals, supply)| {
            Action::IssueToken {
                symbol,
                decimals,
                supply,
            }
        }),
        (address(), address(), amount())
            .prop_map(|(token, to, amount)| { Action::TransferToken { token, to, amount } }),
        (asset(), amount(), asset(), amount(), amount()).prop_map(
            |(asset_a, amount_a, asset_b, amount_b, min_shares)| Action::AddLiquidity {
                asset_a,
                amount_a,
                asset_b,
                amount_b,
                min_shares,
            }
        ),
        (address(), amount(), amount(), amount()).prop_map(|(pool, shares, min_a, min_b)| {
            Action::RemoveLiquidity {
                pool,
                shares,
                min_a,
                min_b,
            }
        }),
        (asset(), amount(), asset(), amount()).prop_map(
            |(asset_in, amount_in, asset_out, min_out)| Action::Swap {
                asset_in,
                amount_in,
                asset_out,
                min_out,
            }
        ),
    ]
    .boxed()
}

fn body() -> BoxedStrategy<TxBody> {
    (
        prop_oneof![Just(TxBody::VERSION), any::<u8>()],
        any::<u64>(),
        address(),
        any::<u64>(),
        amount(),
        any::<u64>(),
        action(),
    )
        .prop_map(
            |(version, chain_id, sender, nonce, fee, expires_at, action)| TxBody {
                version,
                chain_id,
                sender,
                nonce,
                fee,
                expires_at,
                action,
            },
        )
        .boxed()
}

fn witness() -> BoxedStrategy<Witness> {
    (prop::option::of(public_key()), signature())
        .prop_map(|(public_key, signature)| Witness {
            public_key,
            signature,
        })
        .boxed()
}

fn transaction() -> BoxedStrategy<Transaction> {
    (body(), witness())
        .prop_map(|(body, witness)| Transaction { body, witness })
        .boxed()
}

fn header() -> BoxedStrategy<BlockHeader> {
    (
        (any::<u8>(), any::<u64>(), any::<u64>(), hash()),
        (any::<u64>(), address(), any::<u32>()),
        (hash(), hash(), hash()),
    )
        .prop_map(
            |(
                (version, chain_id, height, parent),
                (timestamp_ms, proposer, tx_count),
                (tx_root, witness_root, state_root),
            )| BlockHeader {
                version,
                chain_id,
                height,
                parent,
                timestamp_ms,
                proposer,
                tx_count,
                tx_root,
                witness_root,
                state_root,
            },
        )
        .boxed()
}

fn block() -> BoxedStrategy<Block> {
    (
        header(),
        signature(),
        vec(body(), 0..3),
        prop::option::of(vec(witness(), 0..3)),
    )
        .prop_map(|(header, proposer_signature, bodies, witnesses)| Block {
            header,
            proposer_signature,
            bodies,
            witnesses,
        })
        .boxed()
}

fn params() -> BoxedStrategy<ChainParams> {
    (
        (any::<u64>(), any::<u32>(), any::<u64>(), amount()),
        (any::<u64>(), any::<u64>(), any::<u64>()),
        (vec(scheme(), 0..4), vec(scheme(), 0..4)),
    )
        .prop_map(
            |(
                (block_time_ms, max_block_txs, max_block_bytes, min_fee_per_byte),
                (max_tx_ttl_blocks, witness_retention_blocks, epoch_blocks),
                (tx_schemes, validator_schemes),
            )| ChainParams {
                block_time_ms,
                max_block_txs,
                max_block_bytes,
                min_fee_per_byte,
                max_tx_ttl_blocks,
                witness_retention_blocks,
                tx_schemes,
                validator_schemes,
                epoch_blocks,
            },
        )
        .boxed()
}

fn genesis() -> BoxedStrategy<Genesis> {
    (
        any::<u64>(),
        any::<u64>(),
        params(),
        vec(
            (address(), amount()).prop_map(|(address, amount)| Allocation { address, amount }),
            0..4,
        ),
        vec(
            (public_key(), any::<u64>())
                .prop_map(|(public_key, power)| GenesisValidator { public_key, power }),
            0..3,
        ),
    )
        .prop_map(
            |(chain_id, genesis_time_ms, params, allocations, validators)| Genesis {
                chain_id,
                genesis_time_ms,
                params,
                allocations,
                validators,
            },
        )
        .boxed()
}

/// A way to damage an encoding.
#[derive(Debug, Clone)]
enum Damage {
    Flip(prop::sample::Index, u8),
    Truncate(prop::sample::Index),
    Append(Vec<u8>),
    Insert(prop::sample::Index, u8),
    Remove(prop::sample::Index),
}

fn damage() -> BoxedStrategy<Damage> {
    prop_oneof![
        (any::<prop::sample::Index>(), 1..=255u8).prop_map(|(i, x)| Damage::Flip(i, x)),
        any::<prop::sample::Index>().prop_map(Damage::Truncate),
        vec(any::<u8>(), 1..8).prop_map(Damage::Append),
        (any::<prop::sample::Index>(), any::<u8>()).prop_map(|(i, x)| Damage::Insert(i, x)),
        any::<prop::sample::Index>().prop_map(Damage::Remove),
    ]
    .boxed()
}

fn damaged(mut bytes: Vec<u8>, damage: &[Damage]) -> Vec<u8> {
    for d in damage {
        match d {
            Damage::Flip(i, x) if !bytes.is_empty() => {
                let i = i.index(bytes.len());
                bytes[i] ^= x;
            }
            Damage::Truncate(i) if !bytes.is_empty() => bytes.truncate(i.index(bytes.len())),
            Damage::Append(more) => bytes.extend_from_slice(more),
            Damage::Insert(i, x) => bytes.insert(i.index(bytes.len() + 1), *x),
            Damage::Remove(i) if !bytes.is_empty() => {
                bytes.remove(i.index(bytes.len()));
            }
            _ => {}
        }
    }
    bytes
}

// --- What must hold ---------------------------------------------------------

/// `bytes` either do not decode as a `T`, or are the one encoding of what
/// they decode to.
fn decodes_canonically<T: BorshDeserialize + BorshSerialize>(
    bytes: &[u8],
) -> Result<(), TestCaseError> {
    if let Ok(value) = borsh::from_slice::<T>(bytes) {
        prop_assert_eq!(borsh::to_vec(&value).unwrap(), bytes);
    }
    Ok(())
}

fn round_trips_borsh<T>(value: &T) -> Result<Vec<u8>, TestCaseError>
where
    T: BorshDeserialize + BorshSerialize + PartialEq + std::fmt::Debug,
{
    let bytes = borsh::to_vec(value).unwrap();
    prop_assert_eq!(&borsh::from_slice::<T>(&bytes).unwrap(), value);
    Ok(bytes)
}

fn round_trips_json<T>(value: &T) -> Result<(), TestCaseError>
where
    T: serde::Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug,
{
    let text = serde_json::to_string(value).unwrap();
    prop_assert_eq!(&serde_json::from_str::<T>(&text).unwrap(), value);
    Ok(())
}

proptest! {
    #[test]
    fn a_transaction_decodes_to_what_was_encoded(tx in transaction()) {
        let bytes = round_trips_borsh(&tx)?;
        // Fees and block limits are measured in this length.
        prop_assert_eq!(tx.encoded_len(), bytes.len() as u64);
        round_trips_borsh(&tx.body)?;
        round_trips_json(&tx)?;
    }

    #[test]
    fn a_block_decodes_to_what_was_encoded(block in block()) {
        round_trips_borsh(&block)?;
        round_trips_json(&block)?;
    }

    #[test]
    fn a_genesis_document_decodes_to_what_was_encoded(genesis in genesis()) {
        round_trips_borsh(&genesis)?;
        round_trips_json(&genesis)?;
        // Whatever it says, checking it never panics.
        let _ = genesis.validate();
        let _ = genesis.hash();
    }

    #[test]
    fn arbitrary_bytes_decode_to_nothing_or_canonically(bytes in vec(any::<u8>(), 0..400)) {
        decodes_canonically::<Transaction>(&bytes)?;
        decodes_canonically::<TxBody>(&bytes)?;
        decodes_canonically::<Witness>(&bytes)?;
        decodes_canonically::<BlockHeader>(&bytes)?;
        decodes_canonically::<Block>(&bytes)?;
        decodes_canonically::<Vec<Block>>(&bytes)?;
        decodes_canonically::<Genesis>(&bytes)?;
        decodes_canonically::<Action>(&bytes)?;
        decodes_canonically::<Asset>(&bytes)?;
    }

    #[test]
    fn a_damaged_transaction_decodes_to_nothing_or_canonically(
        tx in transaction(),
        damage in vec(damage(), 1..4),
    ) {
        let bytes = damaged(borsh::to_vec(&tx).unwrap(), &damage);
        decodes_canonically::<Transaction>(&bytes)?;
        // Whatever still decodes can be checked without a panic.
        if let Ok(tx) = borsh::from_slice::<Transaction>(&bytes) {
            let _ = tx.check_stateless(tx.body.chain_id, &ChainParams::default(), tx.body.expires_at);
            let _ = tx.id();
        }
    }

    #[test]
    fn a_damaged_block_decodes_to_nothing_or_canonically(
        blocks in vec(block(), 1..3),
        damage in vec(damage(), 1..4),
    ) {
        let bytes = damaged(borsh::to_vec(&blocks).unwrap(), &damage);
        decodes_canonically::<Vec<Block>>(&bytes)?;
        if let Ok(blocks) = borsh::from_slice::<Vec<Block>>(&bytes) {
            for block in &blocks {
                let _ = block.roots_match();
                let _ = block.transactions();
                let _ = block.hash();
            }
        }
    }

    #[test]
    fn a_damaged_genesis_decodes_to_nothing_or_canonically(
        genesis in genesis(),
        damage in vec(damage(), 1..4),
    ) {
        let bytes = damaged(borsh::to_vec(&genesis).unwrap(), &damage);
        decodes_canonically::<Genesis>(&bytes)?;
        // The JSON form is what a node reads from disk and from its leader.
        let text = damaged(serde_json::to_vec(&genesis).unwrap(), &damage);
        if let Ok(genesis) = serde_json::from_slice::<Genesis>(&text) {
            let _ = genesis.validate();
            let _ = genesis.hash();
        }
    }

    #[test]
    fn arbitrary_text_is_never_a_panic(text in ".{0,200}", decimals in 0..=255u32) {
        let _ = serde_json::from_str::<Genesis>(&text);
        let _ = serde_json::from_str::<Transaction>(&text);
        let _ = serde_json::from_str::<Block>(&text);
        let _ = text.parse::<Address>();
        let _ = text.parse::<Asset>();
        let _ = text.parse::<Scheme>();
        let _ = parse_units(&text, decimals);
    }

    #[test]
    fn checking_any_transaction_never_panics(
        tx in transaction(),
        params in params(),
        chain_id in any::<u64>(),
        height in any::<u64>(),
    ) {
        let _ = tx.check_stateless(chain_id, &params, height);
        let _ = tx.min_fee(&params);
        let _ = tx.body.action.debits();
        let _ = tx.body.action.stein_debit();
        let _ = tx.body.action.counterparty(&tx.body.sender, tx.body.nonce);
    }

    #[test]
    fn an_address_reads_back_and_a_mistyped_one_is_not_another_address(
        address in address(),
        at in any::<prop::sample::Index>(),
        to in "[0-9a-zA-Z]",
    ) {
        let text = address.to_string();
        prop_assert_eq!(text.parse::<Address>().unwrap(), address);
        prop_assert_eq!(text.parse::<Asset>().unwrap(), Asset::Token(address));

        // One character changed: refused, or still the same address (a hex
        // digit in the other case).
        let at = at.index(text.len());
        let mut typo = text.clone();
        typo.replace_range(at..=at, &to);
        if let Ok(read) = typo.parse::<Address>() {
            prop_assert_eq!(read, address);
        }
    }

    #[test]
    fn an_amount_reads_back_at_any_precision_a_token_may_have(
        amount in amount(),
        decimals in 0..=u32::from(stein_types::MAX_TOKEN_DECIMALS),
    ) {
        let text = format_units(amount, decimals);
        prop_assert_eq!(parse_units(&text, decimals), Ok(amount));
    }

    #[test]
    fn formatting_any_amount_never_panics(amount in amount(), decimals in 0..=255u32) {
        let _ = format_units(amount, decimals);
    }
}
