//! `stein-node init` creates a single-validator devnet; `stein-node run` runs it.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use clap::{Parser, Subcommand};
use stein_crypto::{Hash, Scheme, create_seed_file};
use stein_node::{Node, Role, default_data_dir, files, rpc};
use stein_types::{Allocation, ChainParams, Genesis, GenesisValidator, ONE_STEIN};

#[derive(Parser)]
#[command(name = "stein-node", version, about = "Stein Chain devnet node")]
struct Cli {
    /// Data directory.
    #[arg(long, global = true, env = "STEIN_DATA_DIR", default_value_os_t = default_data_dir())]
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
    /// Run the node as the validator.
    Run {
        /// RPC listen address. Keep it on localhost unless it sits behind a
        /// proxy or tunnel: the RPC has no auth or rate limiting yet.
        #[arg(long, env = "STEIN_RPC", default_value = "127.0.0.1:8645")]
        rpc: SocketAddr,
    },
    /// Run the node as a follower of a validator: forward transactions to it,
    /// and download and fully re-verify its blocks.
    Follow {
        /// The validator's RPC base URL, e.g. https://xyz.trycloudflare.com.
        #[arg(long, env = "STEIN_LEADER")]
        leader: String,
        /// The genesis hash this node will accept. Get it from the validator
        /// operator out of band; never from the leader itself.
        #[arg(long, env = "STEIN_GENESIS_HASH")]
        genesis_hash: Hash,
        /// RPC listen address.
        #[arg(long, env = "STEIN_RPC", default_value = "127.0.0.1:8646")]
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
        Command::Run { rpc } => serve(Node::open(&cli.data_dir)?, rpc).await,
        Command::Follow {
            leader,
            genesis_hash,
            rpc,
        } => {
            serve(
                Node::open_follower(&cli.data_dir, &leader, genesis_hash).await?,
                rpc,
            )
            .await
        }
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

async fn serve(node: Arc<Node>, addr: SocketAddr) -> Result<(), Box<dyn std::error::Error>> {
    match &node.role {
        Role::Validator(_) => tokio::spawn(Arc::clone(&node).run_producer()),
        Role::Follower(leader) => {
            tracing::info!(leader = %leader.url, "following");
            tokio::spawn(Arc::clone(&node).run_follower())
        }
    };
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!(%addr, genesis = %node.genesis_hash, "RPC listening");
    axum::serve(listener, rpc::router(node))
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}
