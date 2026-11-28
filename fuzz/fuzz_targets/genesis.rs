//! Bytes as a genesis document: JSON, as a node reads it from disk and a
//! follower from its leader, and Borsh, as it is hashed.
#![no_main]

use libfuzzer_sys::fuzz_target;
use stein_fuzz::{canonical, json_round_trip};
use stein_state::State;
use stein_types::Genesis;

fuzz_target!(|data: &[u8]| {
    let _ = canonical::<Genesis>(data);
    let Ok(genesis) = serde_json::from_slice::<Genesis>(data) else {
        return;
    };
    json_round_trip(&genesis);
    let _ = genesis.hash();
    let _ = genesis.total_supply();
    let valid = genesis.validate().is_ok();
    // A node starts from exactly the documents that pass validation.
    match State::from_genesis(&genesis) {
        Ok(state) => {
            assert!(valid);
            let _ = state.state_root();
            let _ = state.proposer_for(1);
        }
        Err(_) => assert!(!valid),
    }
});
