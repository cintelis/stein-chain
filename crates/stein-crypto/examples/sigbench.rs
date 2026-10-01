//! Measures ML-DSA-44 and ML-DSA-65 signing and verification on this machine,
//! single-core and across all cores. Used to size validator hardware (SPEC §10).
//!
//! cargo run --release -p stein-crypto --example sigbench

#![allow(clippy::arithmetic_side_effects)]

use std::time::Instant;

use rayon::prelude::*;
use stein_crypto::{Domain, Scheme, SecretKey, Signature, verify_batch};

fn main() {
    for scheme in Scheme::ALL {
        bench(scheme);
    }
}

fn bench(scheme: Scheme) {
    const N: usize = 4000;
    let keys: Vec<SecretKey> = (0..64u8)
        .map(|i| SecretKey::from_seed(scheme, &[i; 32]).expect("key"))
        .collect();
    let msgs: Vec<Vec<u8>> = (0..N)
        .map(|i| (i as u64).to_le_bytes().repeat(16))
        .collect();

    let start = Instant::now();
    let sigs: Vec<Signature> = msgs
        .iter()
        .enumerate()
        .map(|(i, m)| keys[i % 64].sign(Domain::Transaction, m).expect("sign"))
        .collect();
    let sign = start.elapsed();

    let jobs: Vec<_> = sigs
        .iter()
        .zip(&msgs)
        .enumerate()
        .map(|(i, (s, m))| {
            (
                keys[i % 64].verifying_key(),
                Domain::Transaction,
                m.as_slice(),
                s,
            )
        })
        .collect();

    let start = Instant::now();
    for (k, d, m, s) in &jobs {
        k.verify(*d, m, s).expect("verify");
    }
    let serial = start.elapsed();

    let start = Instant::now();
    verify_batch(&jobs).expect("batch");
    let parallel = start.elapsed();

    let start = Instant::now();
    let _: Vec<_> = (0..N)
        .into_par_iter()
        .map(|i| keys[i % 64].public_key().address())
        .collect();
    let hashing = start.elapsed();

    let per_sec = |d: std::time::Duration| N as f64 / d.as_secs_f64();
    println!(
        "{scheme}, {N} operations, {} threads",
        rayon::current_num_threads()
    );
    println!("  sign      (1 core)  : {:>9.0} /s", per_sec(sign));
    println!("  verify    (1 core)  : {:>9.0} /s", per_sec(serial));
    println!("  verify    (all)     : {:>9.0} /s", per_sec(parallel));
    println!("  address   (all)     : {:>9.0} /s", per_sec(hashing));
}
