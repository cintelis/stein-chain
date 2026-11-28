//! Bytes as a transaction: what `POST /tx` and block import decode.
#![no_main]

use libfuzzer_sys::fuzz_target;
use stein_crypto::{PublicKey, Scheme, Signature};
use stein_fuzz::{canonical, json_round_trip};
use stein_types::{Action, ChainParams, Transaction, TxBody, Witness};

/// Everything a node does with a transaction before it looks at account
/// state or verifies the signature.
fn check(tx: &Transaction) {
    let params = ChainParams::default();
    let _ = tx.check_stateless(tx.body.chain_id, &params, tx.body.expires_at);
    let _ = tx.min_fee(&params);
    let _ = tx.id();
    let _ = tx.body.action.debits();
    let _ = tx.body.action.stein_debit();
    let _ = tx.body.action.counterparty(&tx.body.sender, tx.body.nonce);
    json_round_trip(tx);
    // Fees and block limits are measured in this length.
    let encoded = borsh::to_vec(tx).expect("encode");
    assert_eq!(tx.encoded_len(), encoded.len() as u64);
    assert_eq!(canonical::<Transaction>(&encoded).as_ref(), Some(tx));
}

/// A witness of the right shape, chosen by `pick`. A fuzzer does not come by
/// thousands of bytes of exactly a signature's length, so bytes that are a
/// body alone are completed with one.
fn filler(pick: u8) -> Witness {
    let scheme = Scheme::ALL[usize::from(pick) % Scheme::ALL.len()];
    let bytes = |len: usize| vec![pick; len];
    Witness {
        public_key: (pick & 0x80 != 0).then(|| {
            PublicKey::from_bytes(scheme, &bytes(scheme.public_key_len())).expect("length")
        }),
        signature: Signature::from_bytes(scheme, &bytes(scheme.signature_len())).expect("length"),
    }
}

fuzz_target!(|data: &[u8]| {
    if let Some(tx) = canonical::<Transaction>(data) {
        check(&tx);
    }
    if let Some((&pick, rest)) = data.split_first()
        && let Some(body) = canonical::<TxBody>(rest)
    {
        check(&Transaction {
            body,
            witness: filler(pick),
        });
    }
    let _ = canonical::<Witness>(data);
    let _ = canonical::<Action>(data);
});
