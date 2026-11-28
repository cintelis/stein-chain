//! Bytes as the blocks a leader serves: what a follower decodes and imports.
#![no_main]

use std::sync::LazyLock;

use libfuzzer_sys::fuzz_target;
use stein_crypto::{Scheme, SecretKey};
use stein_fuzz::{canonical, json_round_trip};
use stein_state::State;
use stein_types::{Block, BlockHeader, ChainParams, Genesis, GenesisValidator};

static GENESIS: LazyLock<Genesis> = LazyLock::new(|| {
    let validator = SecretKey::from_seed(Scheme::MlDsa44, &[1; 32]).expect("key");
    Genesis {
        chain_id: 31_337,
        genesis_time_ms: 1_000,
        params: ChainParams::default(),
        allocations: Vec::new(),
        validators: vec![GenesisValidator {
            public_key: validator.public_key().clone(),
            power: 1,
        }],
    }
});

fuzz_target!(|data: &[u8]| {
    let _ = canonical::<BlockHeader>(data);
    let _ = canonical::<Block>(data);
    let Some(blocks) = canonical::<Vec<Block>>(data) else {
        return;
    };
    let mut state = State::from_genesis(&GENESIS).expect("genesis");
    for block in &blocks {
        let _ = block.roots_match();
        let _ = block.transactions();
        let _ = block.hash();
        json_round_trip(block);
        // Nothing here is signed by the validator, so nothing may be applied.
        let before = (state.state_root(), state.height(), state.head());
        assert!(state.import_block(block).is_err());
        assert_eq!((state.state_root(), state.height(), state.head()), before);
    }
});
