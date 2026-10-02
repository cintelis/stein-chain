//! `stein loadgen`: devnet traffic. Creates throwaway accounts in memory
//! (mixed ML-DSA-44 and ML-DSA-65), funds them from a key file, then sends
//! random transfers between them at a steady rate until stopped.
//!
//! Keys exist only in this process: when it exits, whatever the accounts hold
//! is unrecoverable. Devnet only.

// A devnet tool, not consensus code: counters and amounts here are bounded by
// the funding it hands out, and an overflow could only garble its own output.
#![allow(clippy::arithmetic_side_effects)]

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use stein_crypto::{Address, Scheme, SecretKey, secret_key_from_seed_file};
use stein_node::rpc::{AccountView, Status, SubmitRequest};
use stein_types::{Action, Amount, ONE_STEIN, Transaction, TxBody, format_amount};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// What to generate.
pub struct Options {
    /// Key that funds the accounts (e.g. the devnet faucet).
    pub funder: std::path::PathBuf,
    /// Number of accounts.
    pub accounts: usize,
    /// Transfers per second.
    pub tps: f64,
    /// Stop after this many seconds (0 = until Ctrl-C).
    pub duration_secs: u64,
    /// STEIN given to each account at the start.
    pub fund: Amount,
    /// Largest single transfer.
    pub max_amount: Amount,
}

struct Account {
    key: SecretKey,
    nonce: u64,
    registered: bool,
    /// Our running estimate; resynced from the node after any failure.
    balance: Amount,
}

/// A small non-cryptographic generator for picking senders and amounts.
struct Picker(u64);

impl Picker {
    fn new() -> Self {
        let t = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(1, |d| d.as_nanos() as u64);
        Self(t ^ u64::from(std::process::id()).rotate_left(32) | 1)
    }
    fn next(&mut self) -> u64 {
        // xorshift64*
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
}

struct Rpc {
    base: String,
    client: reqwest::Client,
}

impl Rpc {
    async fn get<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T> {
        let response = self
            .client
            .get(format!("{}{path}", self.base))
            .send()
            .await?;
        if !response.status().is_success() {
            return Err(response.text().await?.into());
        }
        Ok(response.json().await?)
    }

    /// Signs and submits a transfer; returns the encoded size.
    async fn transfer(
        &self,
        status: &Status,
        key: &SecretKey,
        nonce: u64,
        register: bool,
        to: Address,
        amount: Amount,
    ) -> Result<(u64, Amount)> {
        let mut body = TxBody {
            version: TxBody::VERSION,
            chain_id: status.chain_id,
            sender: key.address(),
            nonce,
            fee: 0,
            expires_at: status.height.saturating_add(status.max_tx_ttl_blocks / 2),
            action: Action::Transfer { to, amount },
        };
        // The fee field is fixed-width, so the size is known before the fee is.
        let size = Transaction::sign(body.clone(), key, register)?.encoded_len();
        body.fee = status.min_fee_per_byte.saturating_mul(u128::from(size));
        let fee = body.fee;
        let tx = Transaction::sign(body, key, register)?;
        let raw = hex::encode(borsh::to_vec(&tx)?);
        let response = self
            .client
            .post(format!("{}/tx", self.base))
            .json(&SubmitRequest { raw })
            .send()
            .await?;
        if !response.status().is_success() {
            let reply: serde_json::Value = response.json().await.unwrap_or_default();
            return Err(format!("rejected: {}", reply["error"]).into());
        }
        Ok((size, fee))
    }
}

/// Runs the generator against the node at `rpc`.
pub async fn run(rpc: &str, client: reqwest::Client, opts: Options) -> Result<()> {
    if opts.accounts < 2 {
        return Err("need at least 2 accounts".into());
    }
    if !(opts.tps > 0.0 && opts.tps <= 200.0) {
        return Err("--tps must be between 0 and 200".into());
    }
    let rpc = Rpc {
        base: rpc.to_string(),
        client,
    };
    let funder = secret_key_from_seed_file(&opts.funder)?;
    let mut status: Status = rpc.get("/status").await?;
    println!(
        "loadgen: chain {} at height {}, {} accounts, {} tx/s, funding {} STEIN each",
        status.chain_id,
        status.height,
        opts.accounts,
        opts.tps,
        format_amount(opts.fund)
    );

    // One in three accounts on ML-DSA-65, the rest on the default ML-DSA-44.
    let mut accounts: Vec<Account> = (0..opts.accounts)
        .map(|i| {
            let scheme = if i % 3 == 2 {
                Scheme::MlDsa65
            } else {
                Scheme::MlDsa44
            };
            SecretKey::generate(scheme).map(|key| Account {
                key,
                nonce: 0,
                registered: false,
                balance: 0,
            })
        })
        .collect::<std::result::Result<_, _>>()?;

    // Fund every account from the funder, nonces in sequence.
    let f: AccountView = rpc.get(&format!("/account/{}", funder.address())).await?;
    let needed = opts.fund.saturating_mul(opts.accounts as u128);
    let have: Amount = f.balance.parse()?;
    if have < needed {
        return Err(format!(
            "funder holds {} STEIN, needs {}",
            format_amount(have),
            format_amount(needed)
        )
        .into());
    }
    let (mut nonce, mut registered) = (f.nonce, f.key_registered);
    for a in &mut accounts {
        rpc.transfer(
            &status,
            &funder,
            nonce,
            !registered,
            a.key.address(),
            opts.fund,
        )
        .await?;
        nonce = nonce.saturating_add(1);
        registered = true;
        a.balance = opts.fund;
    }
    // Wait until the funding is in blocks before spending it.
    let target = nonce;
    let started = Instant::now();
    loop {
        let f: AccountView = rpc.get(&format!("/account/{}", funder.address())).await?;
        if f.nonce >= target {
            break;
        }
        if started.elapsed() > Duration::from_secs(60) {
            return Err("funding transfers were not included within 60 s".into());
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    println!(
        "loadgen: {} accounts funded; sending transfers (Ctrl-C to stop)",
        accounts.len()
    );

    let mut pick = Picker::new();
    let interval = Duration::from_secs_f64(1.0 / opts.tps);
    let mut ticker = tokio::time::interval(interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let deadline =
        (opts.duration_secs > 0).then(|| Instant::now() + Duration::from_secs(opts.duration_secs));
    let (mut sent, mut failed, mut bytes) = (0u64, 0u64, 0u64);
    let mut last_report = Instant::now();
    let mut last_status = Instant::now();
    let reserve = ONE_STEIN / 100; // keep enough for fees

    loop {
        tokio::select! {
            _ = ticker.tick() => {}
            _ = tokio::signal::ctrl_c() => break,
        }
        if deadline.is_some_and(|d| Instant::now() >= d) {
            break;
        }
        if last_status.elapsed() > Duration::from_secs(5) {
            if let Ok(s) = rpc.get::<Status>("/status").await {
                status = s;
            }
            last_status = Instant::now();
        }

        let n = accounts.len() as u64;
        let from = pick.below(n) as usize;
        let to = (from + 1 + pick.below(n - 1) as usize) % accounts.len();
        let spendable = accounts[from].balance.saturating_sub(reserve);
        if spendable < ONE_STEIN / 100 {
            continue;
        }
        // Between 0.01 STEIN and the smaller of --max-amount and a tenth of
        // the balance, in whole hundredths.
        let cap = (opts.max_amount.min(spendable / 10) / (ONE_STEIN / 100)).max(1) as u64;
        let amount = Amount::from(1 + pick.below(cap)) * (ONE_STEIN / 100);
        let (key_to, a) = (accounts[to].key.address(), &accounts[from]);
        match rpc
            .transfer(&status, &a.key, a.nonce, !a.registered, key_to, amount)
            .await
        {
            Ok((size, fee)) => {
                sent += 1;
                bytes += size;
                let a = &mut accounts[from];
                a.nonce += 1;
                a.registered = true;
                a.balance = a.balance.saturating_sub(amount + fee);
                accounts[to].balance = accounts[to].balance.saturating_add(amount);
            }
            Err(e) => {
                failed += 1;
                eprintln!("loadgen: transfer from account {from} failed: {e}; resyncing it");
                if let Ok(v) = rpc
                    .get::<AccountView>(&format!("/account/{}", accounts[from].key.address()))
                    .await
                {
                    let a = &mut accounts[from];
                    a.nonce = v.nonce;
                    a.registered = v.key_registered;
                    a.balance = v.balance.parse().unwrap_or(0);
                }
            }
        }
        if last_report.elapsed() > Duration::from_secs(10) {
            println!(
                "loadgen: {sent} sent, {failed} failed, avg {} B/tx, chain height {}",
                bytes.checked_div(sent).unwrap_or(0),
                status.height
            );
            last_report = Instant::now();
        }
    }
    println!("loadgen: done: {sent} transfers sent, {failed} failed");
    Ok(())
}
