//! End-to-end state-machine throughput on this machine: a leader builds full
//! blocks of signed transfers, and a follower re-verifies and re-executes them
//! the way every validator must. Excludes networking and disk.
//!
//! cargo run --release -p stein-state --example throughput

#![allow(clippy::arithmetic_side_effects)]

use std::time::{Duration, Instant};

use rayon::prelude::*;
use stein_crypto::{Scheme, SecretKey};
use stein_state::State;
use stein_types::{
    Action, Allocation, ChainParams, Genesis, GenesisValidator, ONE_STEIN, Transaction, TxBody,
};

const SENDERS: usize = 4_000;
const BLOCKS: usize = 5;

fn main() {
    for scheme in Scheme::ALL {
        run(scheme);
    }
}

fn run(scheme: Scheme) {
    let validator = SecretKey::from_seed(Scheme::MlDsa44, &[0xee; 32]).expect("key");
    let senders: Vec<SecretKey> = (0..SENDERS)
        .into_par_iter()
        .map(|i| {
            let mut seed = [0u8; 32];
            seed[..8].copy_from_slice(&(i as u64).to_le_bytes());
            SecretKey::from_seed(scheme, &seed).expect("key")
        })
        .collect();
    let params = ChainParams {
        max_block_txs: SENDERS as u32,
        max_block_bytes: 64 << 20,
        tx_schemes: Scheme::ALL.to_vec(),
        ..ChainParams::default()
    };
    let genesis = Genesis {
        chain_id: 1,
        genesis_time_ms: 0,
        params,
        allocations: senders
            .iter()
            .map(|k| Allocation {
                address: k.address(),
                amount: 1_000 * ONE_STEIN,
            })
            .collect(),
        validators: vec![GenesisValidator {
            public_key: validator.public_key().clone(),
            power: 1,
        }],
    };
    let mut leader = State::from_genesis(&genesis).expect("genesis");
    let mut cold = State::from_genesis(&genesis).expect("genesis");
    let mut warm = State::from_genesis(&genesis).expect("genesis");

    let (mut build, mut import_cold, mut admit, mut import_warm) = (
        Duration::ZERO,
        Duration::ZERO,
        Duration::ZERO,
        Duration::ZERO,
    );
    let (mut txs_total, mut bytes_total, mut steady_bytes, mut steady_txs) =
        (0usize, 0u64, 0u64, 0usize);
    for round in 0..BLOCKS {
        // Signing happens on users' machines; it is not part of node throughput.
        let txs: Vec<Transaction> = senders
            .par_iter()
            .enumerate()
            .map(|(i, k)| {
                let body = TxBody {
                    chain_id: 1,
                    sender: k.address(),
                    nonce: round as u64,
                    fee: ONE_STEIN / 1000,
                    expires_at: 100,
                    action: Action::Transfer {
                        to: senders[(i + 1) % SENDERS].address(),
                        amount: 1,
                    },
                };
                Transaction::sign(body, k, round == 0).expect("sign")
            })
            .collect();
        let bytes: u64 = txs.iter().map(Transaction::encoded_len).sum();
        bytes_total += bytes;
        if round > 0 {
            steady_bytes += bytes;
            steady_txs += txs.len();
        }

        // The warm follower admits every transaction to its mempool first, as
        // a real validator does while the leader collects them.
        let t = Instant::now();
        let ok = txs
            .par_iter()
            .filter(|tx| warm.check_transaction(tx, tx.public_key()).is_ok())
            .count();
        admit += t.elapsed();
        assert_eq!(ok, txs.len());

        let t = Instant::now();
        let (block, rejected) = leader
            .build_block(txs, &validator, (round as u64 + 1) * 1000)
            .expect("build");
        build += t.elapsed();
        assert!(
            rejected.is_empty(),
            "{:?}",
            &rejected[..rejected.len().min(3)]
        );
        txs_total += block.bodies.len();

        let t = Instant::now();
        cold.import_block(&block).expect("import");
        import_cold += t.elapsed();

        let t = Instant::now();
        warm.import_block(&block).expect("import");
        import_warm += t.elapsed();
    }

    let tps = |d: Duration| txs_total as f64 / d.as_secs_f64();
    let steady = steady_bytes / steady_txs as u64;
    println!(
        "{scheme}: {BLOCKS} blocks x {SENDERS} transfers, {} threads",
        rayon::current_num_threads()
    );
    println!(
        "  transfer size                 : {} bytes first, {steady} bytes thereafter",
        (bytes_total - steady_bytes) / SENDERS as u64
    );
    println!("  leader build (cold cache)     : {:>8.0} tx/s", tps(build));
    println!(
        "  follower import, cold cache   : {:>8.0} tx/s",
        tps(import_cold)
    );
    println!("  mempool admission (verify)    : {:>8.0} tx/s", tps(admit));
    println!(
        "  follower import, warm cache   : {:>8.0} tx/s  (block critical path)",
        tps(import_warm)
    );
    println!(
        "  bandwidth at 2,000 TPS        : {:.0} Mbit/s raw transaction data",
        2000.0 * steady as f64 * 8.0 / 1e6
    );
}
