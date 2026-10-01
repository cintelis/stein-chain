//! `stein`: a command-line wallet for the devnet.
//!
//! Keys are FIPS 204 seeds in files (see `stein_crypto::create_seed_file`).
//! That is fine for a devnet; real funds belong in a hardware wallet (SPEC §9).

mod loadgen;

use std::path::PathBuf;

use clap::{Parser, Subcommand};
use stein_crypto::{Address, Hash, Scheme, create_seed_file, secret_key_from_seed_file};
use stein_node::rpc::{AccountView, Status, SubmitRequest};
use stein_types::{Action, Transaction, TxBody, format_amount, parse_amount};

#[derive(Parser)]
#[command(name = "stein", version, about = "Stein Chain wallet")]
struct Cli {
    /// Node RPC endpoint.
    #[arg(long, global = true, default_value = "http://127.0.0.1:8645")]
    rpc: String,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Create a new key file.
    Keygen {
        /// Where to write the seed. Never overwritten.
        #[arg(long)]
        out: PathBuf,
        /// Signature scheme: ml-dsa-44 (smaller transactions) or ml-dsa-65.
        #[arg(long, default_value_t = Scheme::MlDsa44)]
        scheme: Scheme,
    },
    /// Print the address of a key file.
    Address {
        /// Key file.
        #[arg(long)]
        key: PathBuf,
    },
    /// Show chain status.
    Status,
    /// Show an account.
    Balance {
        /// Address (st…).
        address: Address,
    },
    /// Send STEIN.
    Send {
        /// Sender's key file.
        #[arg(long)]
        key: PathBuf,
        /// Recipient address.
        #[arg(long)]
        to: Address,
        /// Amount in STEIN, e.g. 12.5.
        #[arg(long)]
        amount: String,
        /// Wait until the transaction is in a block.
        #[arg(long, default_value_t = true)]
        wait: bool,
    },
    /// Look up a transaction by id.
    Tx {
        /// Transaction id.
        id: Hash,
    },
    /// Devnet traffic: fund throwaway accounts, then send random transfers
    /// between them at a steady rate.
    Loadgen {
        /// Key that funds the accounts, e.g. the devnet faucet.
        #[arg(long)]
        key: PathBuf,
        /// Number of accounts (one in three uses ML-DSA-65).
        #[arg(long, default_value_t = 20)]
        accounts: usize,
        /// Transfers per second.
        #[arg(long, default_value_t = 2.0)]
        tps: f64,
        /// Stop after this many seconds (0 = until Ctrl-C).
        #[arg(long, default_value_t = 0)]
        duration: u64,
        /// STEIN given to each account.
        #[arg(long, default_value = "1000")]
        fund: String,
        /// Largest single transfer, in STEIN.
        #[arg(long, default_value = "25")]
        max_amount: String,
    },
}

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// HTTP client under Stein's TLS policy: TLS 1.3, post-quantum key exchange.
fn client() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .use_preconfigured_tls(stein_tls::client_config()?)
        .timeout(std::time::Duration::from_secs(30))
        .build()?)
}

async fn get<T: serde::de::DeserializeOwned>(rpc: &str, path: &str) -> Result<T> {
    let response = client()?.get(format!("{rpc}{path}")).send().await?;
    if !response.status().is_success() {
        return Err(response.text().await?.into());
    }
    Ok(response.json().await?)
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let rpc = cli.rpc.trim_end_matches('/');
    match cli.command {
        Command::Keygen { out, scheme } => {
            let key = create_seed_file(&out, scheme)?;
            println!("{}", key.address());
            eprintln!(
                "seed written to {} — anyone with this file controls the account",
                out.display()
            );
        }
        Command::Address { key } => println!("{}", secret_key_from_seed_file(&key)?.address()),
        Command::Status => {
            let status: serde_json::Value = get(rpc, "/status").await?;
            println!("{}", serde_json::to_string_pretty(&status)?);
        }
        Command::Balance { address } => {
            let a: AccountView = get(rpc, &format!("/account/{address}")).await?;
            println!(
                "{} STEIN  (nonce {}, key registered: {})",
                a.balance_stein, a.nonce, a.key_registered
            );
        }
        Command::Send {
            key,
            to,
            amount,
            wait,
        } => {
            let key = secret_key_from_seed_file(&key)?;
            let amount = parse_amount(&amount)?;
            let status: Status = get(rpc, "/status").await?;
            let account: AccountView = get(rpc, &format!("/account/{}", key.address())).await?;

            let mut body = TxBody {
                chain_id: status.chain_id,
                sender: key.address(),
                nonce: account.nonce,
                fee: 0,
                expires_at: status.height.saturating_add(status.max_tx_ttl_blocks / 2),
                action: Action::Transfer { to, amount },
            };
            // The fee field is fixed-width, so the size is known before the fee is.
            let register = !account.key_registered;
            let size = Transaction::sign(body.clone(), &key, register)?.encoded_len();
            body.fee = status.min_fee_per_byte.saturating_mul(u128::from(size));
            let tx = Transaction::sign(body, &key, register)?;

            let raw = hex::encode(borsh::to_vec(&tx)?);
            let response = client()?
                .post(format!("{rpc}/tx"))
                .json(&SubmitRequest { raw })
                .send()
                .await?;
            let ok = response.status().is_success();
            let reply: serde_json::Value = response.json().await?;
            if !ok {
                return Err(format!("rejected: {}", reply["error"]).into());
            }
            println!(
                "submitted {}  ({} bytes, fee {} STEIN)",
                tx.id(),
                size,
                format_amount(tx.body.fee)
            );
            if wait {
                for _ in 0..30 {
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                    let s: serde_json::Value = get(rpc, &format!("/tx/{}", tx.id())).await?;
                    if s["status"] == "included" {
                        println!("included in block {}", s["height"]);
                        return Ok(());
                    }
                }
                return Err("not included after 15 s".into());
            }
        }
        Command::Loadgen {
            key,
            accounts,
            tps,
            duration,
            fund,
            max_amount,
        } => {
            let opts = loadgen::Options {
                funder: key,
                accounts,
                tps,
                duration_secs: duration,
                fund: parse_amount(&fund)?,
                max_amount: parse_amount(&max_amount)?,
            };
            loadgen::run(rpc, client()?, opts).await?;
        }
        Command::Tx { id } => {
            let s: serde_json::Value = get(rpc, &format!("/tx/{id}")).await?;
            println!("{}", serde_json::to_string_pretty(&s)?);
        }
    }
    Ok(())
}
