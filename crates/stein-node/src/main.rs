//! `stein-node init` creates a single-validator devnet; `stein-node run` runs it.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use clap::{Parser, Subcommand};
use stein_crypto::{Scheme, create_seed_file};
use stein_node::{Node, default_data_dir, files, rpc};
use stein_types::{Allocation, ChainParams, Genesis, GenesisValidator, ONE_STEIN};

#[derive(Parser)]
#[command(name = "stein-node", version, about = "Stein Chain devnet node")]
struct Cli {
    /// Data directory.
    #[arg(long, global = true, default_value_os_t = default_data_dir())]
    data_dir: PathBuf,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Create a validator key, a faucet key and a genesis document.
    Init {
        /// Network id. Pick a value no other network uses.
        #[arg(long, default_value_t = 31_337)]
        chain_id: u64,
        /// STEIN allocated to the faucet account.
        #[arg(long, default_value_t = 1_000_000_000)]
        faucet_stein: u64,
        /// Signature scheme for the validator and faucet keys.
        #[arg(long, default_value_t = Scheme::MlDsa44)]
        scheme: Scheme,
    },
    /// Run the node.
    Run {
        /// RPC listen address. Keep it on localhost: the RPC has no auth yet.
        #[arg(long, default_value = "127.0.0.1:8645")]
        rpc: SocketAddr,
    },
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Colour only on a terminal: escape codes in a log file break log tooling.
    tracing_subscriber::fmt()
        .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stdout()))
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    let cli = Cli::parse();
    match cli.command {
        Command::Init {
            chain_id,
            faucet_stein,
            scheme,
        } => init(&cli.data_dir, chain_id, faucet_stein, scheme),
        Command::Run { rpc } => run(&cli.data_dir, rpc).await,
    }
}

fn init(
    dir: &std::path::Path,
    chain_id: u64,
    faucet_stein: u64,
    scheme: Scheme,
) -> Result<(), Box<dyn std::error::Error>> {
    std::fs::create_dir_all(dir)?;
    let genesis_path = dir.join(files::GENESIS);
    if genesis_path.exists() {
        return Err(format!(
            "{} already exists; refusing to overwrite a network",
            genesis_path.display()
        )
        .into());
    }
    let validator = create_seed_file(&dir.join(files::VALIDATOR_SEED), scheme)?;
    let faucet = create_seed_file(&dir.join("faucet.seed"), scheme)?;
    let genesis = Genesis {
        chain_id,
        genesis_time_ms: SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis() as u64,
        params: ChainParams::default(),
        allocations: vec![Allocation {
            address: faucet.address(),
            amount: u128::from(faucet_stein)
                .checked_mul(ONE_STEIN)
                .ok_or("faucet amount overflows")?,
        }],
        validators: vec![GenesisValidator {
            public_key: validator.public_key().clone(),
            power: 1,
        }],
    };
    genesis.validate()?;
    std::fs::write(&genesis_path, serde_json::to_string_pretty(&genesis)?)?;
    println!("devnet initialised in {}", dir.display());
    println!("  chain id     : {chain_id}");
    println!("  key scheme   : {scheme}");
    println!("  genesis hash : {}", genesis.hash());
    println!("  validator    : {}", validator.address());
    println!(
        "  faucet       : {} ({faucet_stein} STEIN)",
        faucet.address()
    );
    println!("  faucet key   : {}", dir.join("faucet.seed").display());
    Ok(())
}

async fn run(dir: &std::path::Path, addr: SocketAddr) -> Result<(), Box<dyn std::error::Error>> {
    let node = Node::open(dir)?;
    tokio::spawn(std::sync::Arc::clone(&node).run_producer());
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!(%addr, genesis = %node.genesis_hash, "RPC listening");
    axum::serve(listener, rpc::router(node))
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}
