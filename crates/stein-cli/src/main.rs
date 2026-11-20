//! `stein`: a command-line wallet for the devnet.
//!
//! Keys are 32-byte seeds in files (see `stein_crypto::create_seed_file`).
//! That is fine for a devnet; real funds belong in a hardware wallet (SPEC §9).

mod loadgen;

use std::path::PathBuf;

use clap::{Parser, Subcommand};
use stein_crypto::{Address, Hash, Scheme, create_seed_file, secret_key_from_seed_file};
use stein_node::rpc::{AccountView, PoolView, Status, SubmitRequest, TokenView};
use stein_types::math::mul_div;
use stein_types::{
    AMOUNT_DECIMALS, Action, Amount, Asset, MIN_LIQUIDITY, Transaction, TxBody, deposit,
    first_shares, format_amount, format_units, parse_amount, parse_units, pool_address,
    swap_output, withdrawal,
};

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
        /// Signature scheme: fn-dsa-512-draft (smallest transactions; draft
        /// FIPS 206), ml-dsa-44 or ml-dsa-65 (FIPS 204). A network created
        /// with `init --ml-dsa-only` accepts only the last two.
        #[arg(long, default_value_t = Scheme::FnDsa512Draft)]
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
    /// Tokens: issue one, send one, or look them up.
    Token {
        #[command(subcommand)]
        command: TokenCommand,
    },
    /// Liquidity pools: fund one, trade through one, or look them up.
    Pool {
        #[command(subcommand)]
        command: PoolCommand,
    },
    /// Devnet traffic: fund throwaway accounts, then send random transfers
    /// between them at a steady rate.
    Loadgen {
        /// Key that funds the accounts, e.g. the devnet faucet.
        #[arg(long)]
        key: PathBuf,
        /// Number of accounts. All use FN-DSA-512 where the network accepts
        /// it, otherwise ML-DSA.
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

#[derive(Subcommand)]
enum TokenCommand {
    /// Create a token. The whole supply goes to the issuer and is fixed for
    /// good: nobody can create more.
    Issue {
        /// Issuer's key file.
        #[arg(long)]
        key: PathBuf,
        /// Label: 1 to 12 capital letters and digits, e.g. GOLD. Not unique;
        /// the address printed is what identifies the token.
        #[arg(long)]
        symbol: String,
        /// Total supply in whole tokens, e.g. 1000000.
        #[arg(long)]
        supply: String,
        /// Decimal places (0 to 18).
        #[arg(long, default_value_t = 6)]
        decimals: u8,
    },
    /// Send a token, or shares of a pool.
    Send {
        /// Sender's key file.
        #[arg(long)]
        key: PathBuf,
        /// The token's address, or the pool's for its shares.
        #[arg(long)]
        token: Address,
        /// Recipient address.
        #[arg(long)]
        to: Address,
        /// Amount in whole tokens, e.g. 12.5.
        #[arg(long)]
        amount: String,
    },
    /// Show a token.
    Info {
        /// The token's address.
        address: Address,
    },
    /// List tokens.
    List,
}

#[derive(Subcommand)]
enum PoolCommand {
    /// Deposit both assets of a pair for shares of its pool. The first
    /// deposit creates the pool and sets its price; later ones are taken at
    /// the pool's ratio and the rest is returned.
    Add {
        /// Depositor's key file.
        #[arg(long)]
        key: PathBuf,
        /// One asset: `stein` or a token address.
        #[arg(long)]
        a: Asset,
        /// Most of it to deposit, in whole units.
        #[arg(long)]
        amount_a: String,
        /// The other asset.
        #[arg(long)]
        b: Asset,
        /// Most of it to deposit, in whole units.
        #[arg(long)]
        amount_b: String,
        /// How far the result may fall short of the current estimate before
        /// the pool returns the deposit instead, in hundredths of a percent.
        #[arg(long, default_value_t = 50)]
        slippage_bps: u32,
    },
    /// Redeem shares of a pool for its two assets.
    Remove {
        /// Shareholder's key file.
        #[arg(long)]
        key: PathBuf,
        /// The pool's address.
        #[arg(long)]
        pool: Address,
        /// Shares to redeem, or `all`.
        #[arg(long)]
        shares: String,
        /// As for `add`.
        #[arg(long, default_value_t = 50)]
        slippage_bps: u32,
    },
    /// Trade one asset for another through their pool.
    Swap {
        /// Trader's key file.
        #[arg(long)]
        key: PathBuf,
        /// Asset to pay: `stein` or a token address.
        #[arg(long)]
        sell: Asset,
        /// How much of it, in whole units.
        #[arg(long)]
        amount: String,
        /// Asset to receive.
        #[arg(long)]
        buy: Asset,
        /// How far the amount received may fall short of the current quote
        /// before the pool returns the payment instead, in hundredths of a
        /// percent.
        #[arg(long, default_value_t = 50)]
        slippage_bps: u32,
    },
    /// Show a pool, by its address.
    Info {
        /// The pool's address.
        address: Address,
    },
    /// List pools.
    List,
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

/// `Ok(None)` where the node answers 404.
async fn find<T: serde::de::DeserializeOwned>(rpc: &str, path: &str) -> Result<Option<T>> {
    let response = client()?.get(format!("{rpc}{path}")).send().await?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    if !response.status().is_success() {
        return Err(response.text().await?.into());
    }
    Ok(Some(response.json().await?))
}

/// Decimal places and symbol of an asset. A pool's shares, and anything the
/// node does not know as a token, count in whole units.
async fn describe(rpc: &str, asset: &Asset) -> Result<(u32, String)> {
    match asset {
        Asset::Stein => Ok((AMOUNT_DECIMALS, "STEIN".into())),
        Asset::Token(address) => {
            let token: Option<TokenView> = find(rpc, &format!("/token/{address}")).await?;
            Ok(token.map_or((0, "shares".into()), |t| (t.decimals, t.symbol)))
        }
    }
}

/// `amount` less `bps` hundredths of a percent, rounded down.
fn less_slippage(amount: Amount, bps: u32) -> Amount {
    let keep = 10_000u128.saturating_sub(u128::from(bps));
    mul_div(amount, keep, 10_000).unwrap_or(0)
}

/// Signs `action` with the key in `key`, at the minimum fee, submits it and
/// waits for its block. Reports what it did: a pool may return what it was
/// sent instead of carrying the operation out.
async fn transact(rpc: &str, key: &std::path::Path, action: Action, wait: bool) -> Result<()> {
    let key = secret_key_from_seed_file(key)?;
    let status: Status = get(rpc, "/status").await?;
    let account: AccountView = get(rpc, &format!("/account/{}", key.address())).await?;

    let mut body = TxBody {
        version: TxBody::VERSION,
        chain_id: status.chain_id,
        sender: key.address(),
        nonce: account.nonce,
        fee: 0,
        expires_at: status.height.saturating_add(status.max_tx_ttl_blocks / 2),
        action,
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
    if !wait {
        return Ok(());
    }
    for _ in 0..30 {
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        let s: serde_json::Value = get(rpc, &format!("/tx/{}", tx.id())).await?;
        if s["status"] != "included" {
            continue;
        }
        println!("included in block {}", s["height"]);
        let outcome = &s["tx"]["outcome"];
        let text = |field: &str| outcome[field].as_str().unwrap_or("?").to_string();
        match outcome["result"].as_str() {
            Some("token_issued") => println!("token {}", text("token")),
            Some("liquidity_added") => println!(
                "received {} shares of pool {} for {} and {} base units",
                text("shares"),
                text("pool"),
                text("used_a"),
                text("used_b")
            ),
            Some("liquidity_removed") => println!(
                "received {} and {} base units",
                text("amount_a"),
                text("amount_b")
            ),
            Some("swapped") => println!("received {} base units", text("amount_out")),
            Some("refunded") => {
                return Err(format!(
                    "the pool did not carry it out ({}): what was sent has been returned, \
                     and the fee is spent",
                    text("reason")
                )
                .into());
            }
            _ => {}
        }
        return Ok(());
    }
    Err("not included after 15 s".into())
}

fn print_pool(p: &PoolView) {
    println!(
        "{}  {} {} : {} {}  ({} shares, fee {}/1000)",
        p.address,
        p.reserve_a_formatted,
        p.symbol_a,
        p.reserve_b_formatted,
        p.symbol_b,
        p.shares,
        p.fee_per_mille
    );
}

async fn token_command(rpc: &str, command: TokenCommand) -> Result<()> {
    match command {
        TokenCommand::Issue {
            key,
            symbol,
            supply,
            decimals,
        } => {
            let supply = parse_units(&supply, u32::from(decimals))?;
            let action = Action::IssueToken {
                symbol,
                decimals,
                supply,
            };
            transact(rpc, &key, action, true).await
        }
        TokenCommand::Send {
            key,
            token,
            to,
            amount,
        } => {
            let (decimals, _) = describe(rpc, &Asset::Token(token)).await?;
            let amount = parse_units(&amount, decimals)?;
            transact(rpc, &key, Action::TransferToken { token, to, amount }, true).await
        }
        TokenCommand::Info { address } => {
            let t: TokenView = get(rpc, &format!("/token/{address}")).await?;
            println!(
                "{}  supply {} ({} decimals), issued by {}",
                t.symbol, t.supply_formatted, t.decimals, t.issuer
            );
            Ok(())
        }
        TokenCommand::List => {
            let tokens: Vec<TokenView> = get(rpc, "/tokens?limit=100").await?;
            for t in tokens {
                println!("{}  {}  supply {}", t.address, t.symbol, t.supply_formatted);
            }
            Ok(())
        }
    }
}

async fn pool_command(rpc: &str, command: PoolCommand) -> Result<()> {
    match command {
        PoolCommand::Add {
            key,
            a,
            amount_a,
            b,
            amount_b,
            slippage_bps,
        } => {
            let amount_a = parse_units(&amount_a, describe(rpc, &a).await?.0)?;
            let amount_b = parse_units(&amount_b, describe(rpc, &b).await?.0)?;
            // A pair has one order: STEIN first, then tokens by address.
            let ((asset_a, amount_a), (asset_b, amount_b)) = if a <= b {
                ((a, amount_a), (b, amount_b))
            } else {
                ((b, amount_b), (a, amount_a))
            };
            let address = pool_address(asset_a, asset_b);
            let pool: Option<PoolView> = find(rpc, &format!("/pool/{address}")).await?;
            let expected = match &pool {
                Some(p) => deposit(
                    p.reserve_a.parse()?,
                    p.reserve_b.parse()?,
                    p.shares.parse()?,
                    amount_a,
                    amount_b,
                )
                .map_or(0, |d| d.shares),
                None => first_shares(amount_a, amount_b).saturating_sub(MIN_LIQUIDITY),
            };
            let action = Action::AddLiquidity {
                asset_a,
                amount_a,
                asset_b,
                amount_b,
                min_shares: less_slippage(expected, slippage_bps),
            };
            println!("pool {address}: about {expected} shares");
            transact(rpc, &key, action, true).await
        }
        PoolCommand::Remove {
            key,
            pool,
            shares,
            slippage_bps,
        } => {
            let p: PoolView = get(rpc, &format!("/pool/{pool}")).await?;
            let shares: Amount = if shares == "all" {
                let me = secret_key_from_seed_file(&key)?.address();
                let account: AccountView = get(rpc, &format!("/account/{me}")).await?;
                account
                    .holdings
                    .iter()
                    .find(|h| h.asset == pool)
                    .map_or(Ok(0), |h| h.balance.parse())?
            } else {
                shares.parse()?
            };
            let (out_a, out_b) = withdrawal(
                p.reserve_a.parse()?,
                p.reserve_b.parse()?,
                p.shares.parse()?,
                shares,
            )
            .unwrap_or((0, 0));
            println!(
                "about {} {} and {} {}",
                format_units(out_a, p.decimals_a),
                p.symbol_a,
                format_units(out_b, p.decimals_b),
                p.symbol_b
            );
            let action = Action::RemoveLiquidity {
                pool,
                shares,
                min_a: less_slippage(out_a, slippage_bps),
                min_b: less_slippage(out_b, slippage_bps),
            };
            transact(rpc, &key, action, true).await
        }
        PoolCommand::Swap {
            key,
            sell,
            amount,
            buy,
            slippage_bps,
        } => {
            let amount_in = parse_units(&amount, describe(rpc, &sell).await?.0)?;
            let (decimals_out, symbol_out) = describe(rpc, &buy).await?;
            let address = pool_address(sell, buy);
            let p: PoolView = find(rpc, &format!("/pool/{address}"))
                .await?
                .ok_or("there is no pool for this pair")?;
            let (reserve_a, reserve_b): (Amount, Amount) =
                (p.reserve_a.parse()?, p.reserve_b.parse()?);
            let (reserve_in, reserve_out) = if p.asset_a == sell {
                (reserve_a, reserve_b)
            } else {
                (reserve_b, reserve_a)
            };
            let quote = swap_output(reserve_in, reserve_out, amount_in).unwrap_or(0);
            let min_out = less_slippage(quote, slippage_bps);
            println!(
                "quote {} {symbol_out}, at least {}",
                format_units(quote, decimals_out),
                format_units(min_out, decimals_out)
            );
            let action = Action::Swap {
                asset_in: sell,
                amount_in,
                asset_out: buy,
                min_out,
            };
            transact(rpc, &key, action, true).await
        }
        PoolCommand::Info { address } => {
            print_pool(&get(rpc, &format!("/pool/{address}")).await?);
            Ok(())
        }
        PoolCommand::List => {
            let pools: Vec<PoolView> = get(rpc, "/pools?limit=100").await?;
            pools.iter().for_each(print_pool);
            Ok(())
        }
    }
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
            for h in &a.holdings {
                println!("{} {}  ({})", h.balance_formatted, h.symbol, h.asset);
            }
        }
        Command::Send {
            key,
            to,
            amount,
            wait,
        } => {
            let amount = parse_amount(&amount)?;
            transact(rpc, &key, Action::Transfer { to, amount }, wait).await?;
        }
        Command::Token { command } => token_command(rpc, command).await?,
        Command::Pool { command } => pool_command(rpc, command).await?,
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
