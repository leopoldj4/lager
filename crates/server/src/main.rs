use std::path::{Path, PathBuf};

use clap::Parser;
use worktree_core::config::Config;
use worktree_core::storage::Storage;

#[derive(Parser)]
#[command(
    name = "worktree-server",
    about = "Git server with S3-compatible storage"
)]
struct Cli {
    /// Server configuration [default: worktree.toml].
    #[arg(long, short, env = "WORKTREE_CONFIG")]
    config: Option<PathBuf>,
}

const DEFAULT_CONFIG: &str = "worktree.toml";

fn main() {
    // Git runs this program as the proc-receive hook through a symlink.
    let argv0 = std::env::args_os().next().unwrap_or_default();
    if Path::new(&argv0).file_name().and_then(|f| f.to_str()) == Some("proc-receive") {
        std::process::exit(worktree_server::proc_receive::run_proc_receive());
    }
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with_writer(std::io::stderr)
        .init();
    let cli = Cli::parse();
    let rt = tokio::runtime::Runtime::new().expect("runtime");
    if let Err(e) = rt.block_on(run(cli)) {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

async fn run(cli: Cli) -> anyhow::Result<()> {
    let config_path = cli.config.unwrap_or_else(|| DEFAULT_CONFIG.into());
    let cfg = Config::load(&config_path)?;
    let storage = Storage::from_config(&cfg)?;
    let server = worktree_server::server::start(cfg, Some(config_path), storage).await?;
    eprintln!("worktree serving on {}", server.url());
    use tokio::signal::unix::{SignalKind, signal};
    let mut term = signal(SignalKind::terminate())?;
    let mut int = signal(SignalKind::interrupt())?;
    tokio::select! {
        _ = term.recv() => {}
        _ = int.recv() => {}
    }
    eprintln!("worktree stopping; a second signal stops at once");
    // Stopping at any point is safe: storage decides every push.
    tokio::select! {
        _ = server.shutdown() => {}
        _ = term.recv() => {}
        _ = int.recv() => {}
    }
    Ok(())
}
