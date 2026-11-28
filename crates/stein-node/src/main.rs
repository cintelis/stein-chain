//! `stein-node init` creates a single-validator devnet; `stein-node run` runs it.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use clap::{Parser, Subcommand};
use stein_crypto::{Hash, Scheme, create_seed_file};
use stein_node::limit::{Limiter, Limits};
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
        /// Signature scheme for the faucet key. Default: fn-dsa-512-draft, or
        /// ml-dsa-44 with --ml-dsa-only.
        #[arg(long)]
        scheme: Option<Scheme>,
        /// Signature scheme for the validator key: ml-dsa-65 or ml-dsa-44.
        /// Validators never use a draft-standard scheme.
        #[arg(long, default_value_t = Scheme::MlDsa65)]
        validator_scheme: Scheme,
        /// Accept only ML-DSA account keys. By default accounts may also opt in
        /// to FN-DSA-512 per the draft FIPS 206 (SPEC §3.5).
        #[arg(long)]
        ml_dsa_only: bool,
    },
    /// Run the node as the validator.
    Run {
        /// RPC listen address. Keep it on localhost: the public reach a
        /// follower, never the validator (SPEC §9).
        #[arg(long, env = "STEIN_RPC", default_value = "127.0.0.1:8645")]
        rpc: SocketAddr,
        #[command(flatten)]
        limits: RpcLimits,
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
        #[command(flatten)]
        limits: RpcLimits,
    },
}

/// What one client (an IPv4 address or an IPv6 /64) may ask of the RPC.
/// Requests from this machine itself are never limited.
#[derive(clap::Args)]
struct RpcLimits {
    /// Transactions a client may submit per minute; 0 for no limit.
    #[arg(long, env = "STEIN_RPC_SUBMITS_PER_MINUTE", default_value_t = Limits::default().submits_per_minute)]
    rpc_submits_per_minute: u32,
    /// Block-log reads (what a follower syncs from) a client may make per
    /// minute; 0 for no limit.
    #[arg(long, env = "STEIN_RPC_SYNCS_PER_MINUTE", default_value_t = Limits::default().syncs_per_minute)]
    rpc_syncs_per_minute: u32,
    /// Other requests a client may make per minute; 0 for no limit.
    #[arg(long, env = "STEIN_RPC_READS_PER_MINUTE", default_value_t = Limits::default().reads_per_minute)]
    rpc_reads_per_minute: u32,
    /// Event streams a client may hold open at once; 0 for no limit.
    #[arg(long, env = "STEIN_RPC_STREAMS", default_value_t = Limits::default().streams)]
    rpc_streams: u32,
    /// The header a proxy in front of this node puts the client's address
    /// in, e.g. cf-connecting-ip. Set it only when the proxy is the only way
    /// to reach the node: a client that reaches the node directly can write
    /// the header itself.
    #[arg(long, env = "STEIN_RPC_CLIENT_IP_HEADER")]
    rpc_client_ip_header: Option<axum::http::HeaderName>,
}

impl From<RpcLimits> for Limits {
    fn from(l: RpcLimits) -> Self {
        Self {
            submits_per_minute: l.rpc_submits_per_minute,
            syncs_per_minute: l.rpc_syncs_per_minute,
            reads_per_minute: l.rpc_reads_per_minute,
            streams: l.rpc_streams,
            client_ip_header: l.rpc_client_ip_header,
        }
    }
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
            validator_scheme,
            ml_dsa_only,
        } => init(
            &cli.data_dir,
            chain_id,
            faucet_stein,
            scheme,
            validator_scheme,
            ml_dsa_only,
        ),
        Command::Run { rpc, limits } => serve(Node::open(&cli.data_dir)?, rpc, limits.into()).await,
        Command::Follow {
            leader,
            genesis_hash,
            rpc,
            limits,
        } => {
            serve(
                Node::open_follower(&cli.data_dir, &leader, genesis_hash).await?,
                rpc,
                limits.into(),
            )
            .await
        }
    }
}

fn init(
    dir: &std::path::Path,
    chain_id: u64,
    faucet_stein: u64,
    scheme: Option<Scheme>,
    validator_scheme: Scheme,
    ml_dsa_only: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let scheme = scheme.unwrap_or(if ml_dsa_only {
        Scheme::MlDsa44
    } else {
        Scheme::FnDsa512Draft
    });
    let mut params = ChainParams::default();
    if ml_dsa_only {
        params.tx_schemes.retain(|s| !s.is_draft());
    }
    if !params.tx_schemes.contains(&scheme) {
        return Err(format!("{scheme} keys are not accepted with --ml-dsa-only").into());
    }
    if !params.validator_schemes.contains(&validator_scheme) {
        return Err(format!("validators cannot use {validator_scheme}").into());
    }

    std::fs::create_dir_all(dir)?;
    let genesis_path = dir.join(files::GENESIS);
    if genesis_path.exists() {
        return Err(format!(
            "{} already exists; refusing to overwrite a network",
            genesis_path.display()
        )
        .into());
    }
    let validator = create_seed_file(&dir.join(files::VALIDATOR_SEED), validator_scheme)?;
    let faucet = create_seed_file(&dir.join("faucet.seed"), scheme)?;
    let genesis = Genesis {
        chain_id,
        genesis_time_ms: SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis() as u64,
        params,
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
    println!(
        "  accounts     : {}",
        genesis
            .params
            .tx_schemes
            .iter()
            .map(|s| s.name())
            .collect::<Vec<_>>()
            .join(", ")
    );
    println!("  key schemes  : validator {validator_scheme}, faucet {scheme}");
    println!("  genesis hash : {}", genesis.hash());
    println!("  validator    : {}", validator.address());
    println!(
        "  faucet       : {} ({faucet_stein} STEIN)",
        faucet.address()
    );
    println!("  faucet key   : {}", dir.join("faucet.seed").display());
    Ok(())
}

async fn serve(
    node: Arc<Node>,
    addr: SocketAddr,
    limits: Limits,
) -> Result<(), Box<dyn std::error::Error>> {
    match &node.role {
        Role::Validator(_) => tokio::spawn(Arc::clone(&node).run_producer()),
        Role::Follower(leader) => {
            tracing::info!(leader = %leader.url, "following");
            tokio::spawn(Arc::clone(&node).run_follower())
        }
    };
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!(
        %addr,
        genesis = %node.genesis_hash,
        submits_per_minute = limits.submits_per_minute,
        syncs_per_minute = limits.syncs_per_minute,
        reads_per_minute = limits.reads_per_minute,
        streams = limits.streams,
        client_ip_header = limits.client_ip_header.as_ref().map(|h| h.as_str()),
        "RPC listening; limits are per client, 0 is none"
    );
    let app = rpc::router_with_limits(node, Limiter::new(limits));
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(async {
        let _ = tokio::signal::ctrl_c().await;
    })
    .await?;
    Ok(())
}
