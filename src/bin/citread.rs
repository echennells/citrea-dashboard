use citreaanalysis::citrea_rpc::{DEFAULT_BATCH_EXPLORER_URL, DEFAULT_CITREA_RPC_URL};
use citreaanalysis::daemon::{self, DaemonConfig};
use clap::Parser;

#[derive(Parser)]
#[command(name = "citread", about = "Citrea economics monitoring daemon")]
struct Args {
    /// Path to the SQLite database
    #[arg(long, default_value = "citrea.db")]
    db_path: String,

    /// Poll interval in seconds (steady-state mode)
    #[arg(long, default_value_t = 120)]
    poll_interval: u64,

    /// Run all backfill pipelines aggressively then exit
    #[arg(long)]
    backfill_only: bool,

    /// Path to all_commitments.json for bootstrapping
    #[arg(long, default_value = "all_commitments.json")]
    json_path: String,

    /// Number of batches to count L2 txs for per cycle
    #[arg(long, default_value_t = 10)]
    batches_per_cycle: usize,

    /// Bitcoin Core RPC host (host:port)
    #[arg(long, env = "BTC_RPC_HOST")]
    btc_rpc_host: String,

    /// Bitcoin Core RPC username
    #[arg(long, env = "BTC_RPC_USER")]
    btc_rpc_user: String,

    /// Bitcoin Core RPC password
    #[arg(long, env = "BTC_RPC_PASS")]
    btc_rpc_pass: String,

    /// Citrea L2 RPC URL
    #[arg(long, env = "CITREA_RPC_URL", default_value = DEFAULT_CITREA_RPC_URL)]
    citrea_rpc_url: String,

    /// Batch explorer API URL
    #[arg(long, env = "BATCH_EXPLORER_URL", default_value = DEFAULT_BATCH_EXPLORER_URL)]
    batch_explorer_url: String,
}

fn main() {
    env_logger::init();
    let args = Args::parse();

    let config = DaemonConfig {
        db_path: args.db_path,
        poll_interval_secs: args.poll_interval,
        backfill_only: args.backfill_only,
        json_path: Some(args.json_path),
        batches_per_cycle: args.batches_per_cycle,
        btc_rpc_host: args.btc_rpc_host,
        btc_rpc_user: args.btc_rpc_user,
        btc_rpc_pass: args.btc_rpc_pass,
        citrea_rpc_url: args.citrea_rpc_url,
        batch_explorer_url: args.batch_explorer_url,
    };

    if let Err(e) = daemon::run(config) {
        log::error!("Daemon error: {}", e);
        std::process::exit(1);
    }
}
