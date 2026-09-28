use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use clap::{Parser, Subcommand};
use rustgit::config::{Config, validate_repo_name};
use rustgit::model::{RepoId, branch_ref};
use rustgit::repository::admin;

#[derive(Parser)]
#[command(name = "rustgit", about = "Git server with S3-compatible storage")]
struct Cli {
    #[arg(
        long,
        short,
        env = "RUSTGIT_CONFIG",
        global = true,
        default_value = "rustgit.toml"
    )]
    config: PathBuf,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run the server.
    Serve,
    /// Repository management.
    Repo {
        #[command(subcommand)]
        cmd: RepoCmd,
    },
    /// Create a snapshot (combined pack) of the current generation.
    Snapshot { name: String },
    /// Find obsolete stored data. Removes it only with --delete.
    Cleanup {
        name: String,
        #[arg(long, default_value_t = 168)]
        retention_hours: u64,
        #[arg(long)]
        delete: bool,
    },
    /// Backup and recovery.
    Backup {
        #[command(subcommand)]
        cmd: BackupCmd,
    },
    /// Check the storage requirements against the configured provider.
    CheckStorage,
}

#[derive(Subcommand)]
enum RepoCmd {
    /// Create a repository and add it to the configuration file.
    Create {
        name: String,
        #[arg(long, default_value = "main")]
        default_branch: String,
    },
    /// Mark a repository deleted (tombstone).
    Delete { name: String },
    /// Change the default branch (Git HEAD).
    SetDefaultBranch { name: String, branch: String },
}

#[derive(Subcommand)]
enum BackupCmd {
    Export {
        name: String,
        out: PathBuf,
    },
    /// Import into the configured storage. With --name, add it to the configuration.
    Import {
        dir: PathBuf,
        #[arg(long)]
        name: Option<String>,
    },
}

fn main() {
    // Git runs this program as the proc-receive hook through a symlink.
    let argv0 = std::env::args_os().next().unwrap_or_default();
    if Path::new(&argv0).file_name().and_then(|f| f.to_str()) == Some("proc-receive") {
        std::process::exit(rustgit::git::hook::run_proc_receive());
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
    let cfg = Config::load(&cli.config)?;
    let storage = rustgit::server::storage_from_config(&cfg)?;
    match cli.cmd {
        Cmd::Serve => {
            let server = rustgit::server::start(cfg, Some(cli.config.clone()), storage).await?;
            eprintln!("rustgit serving on {}", server.url());
            tokio::signal::ctrl_c().await?;
            server.stop();
        }
        Cmd::Repo { cmd } => repo_cmd(&cli.config, &cfg, &storage, cmd).await?,
        Cmd::Snapshot { name } => {
            let id = repo_id(&cfg, &name)?;
            let svc = cli_services(&cfg, storage).await?;
            let registry = rustgit::repository::Registry::new(svc)?;
            let report = rustgit::maintenance::snapshot(&registry.controller(id), "cli").await?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        Cmd::Cleanup {
            name,
            retention_hours,
            delete,
        } => {
            let id = repo_id(&cfg, &name)?;
            let report = rustgit::maintenance::cleanup(
                &storage,
                id,
                Duration::from_secs(retention_hours * 3600),
                !delete,
            )
            .await?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        Cmd::Backup {
            cmd: BackupCmd::Export { name, out },
        } => {
            let m = rustgit::backup::export(&storage, repo_id(&cfg, &name)?, &out).await?;
            println!(
                "exported generation {} ({} records, {} packs)",
                m.generation,
                m.records.len(),
                m.packs.len()
            );
        }
        Cmd::Backup {
            cmd: BackupCmd::Import { dir, name },
        } => {
            let head = rustgit::backup::import(&storage, &dir).await?;
            if let Some(name) = name {
                validate_repo_name(&name)?;
                add_repo_to_config(&cli.config, &name, head.repository, "main")?;
            }
            println!(
                "imported repository {} at generation {}",
                head.repository, head.generation
            );
        }
        Cmd::CheckStorage => {
            let results = rustgit::storage::provider_check::run(&storage).await;
            let mut failed = false;
            for (name, r) in results {
                match r {
                    Ok(()) => println!("ok    {name}"),
                    Err(e) => {
                        failed = true;
                        println!("FAIL  {name}: {e}");
                    }
                }
            }
            anyhow::ensure!(
                !failed,
                "the storage service does not meet the requirements"
            );
        }
    }
    Ok(())
}

async fn cli_services(
    cfg: &Config,
    storage: rustgit::storage::Storage,
) -> anyhow::Result<Arc<rustgit::repository::Services>> {
    let dir = cfg
        .server
        .cache_dir
        .join(format!("cli-{}", std::process::id()));
    let git = rustgit::git::Git::new(
        cfg.server.git_binary.clone(),
        &dir.join("work"),
        &std::env::current_exe()?,
    )?;
    Ok(Arc::new(rustgit::repository::Services {
        storage,
        git,
        cache_root: dir,
        limits: cfg.limits.clone(),
        metrics: Default::default(),
        notifier: Default::default(),
    }))
}

fn repo_id(cfg: &Config, name: &str) -> anyhow::Result<RepoId> {
    cfg.repos
        .iter()
        .find(|r| r.name == name)
        .map(|r| r.id)
        .ok_or_else(|| anyhow::anyhow!("unknown repository {name}"))
}

async fn repo_cmd(
    path: &Path,
    cfg: &Config,
    storage: &rustgit::storage::Storage,
    cmd: RepoCmd,
) -> anyhow::Result<()> {
    match cmd {
        RepoCmd::Create {
            name,
            default_branch,
        } => {
            validate_repo_name(&name)?;
            let branch = branch_ref(&default_branch)?;
            let id = RepoId::new();
            if let Some(existing) = cfg.repos.iter().find(|r| r.name == name) {
                // A reused name gets a new UUID. The old repository must be deleted.
                match storage.load_head(existing.id).await? {
                    Some((h, _)) if h.state == rustgit::model::RepoState::Deleted => {}
                    None => {}
                    Some(_) => anyhow::bail!("repository {name} exists"),
                }
                admin::create_repository(storage, id, &branch, "cli").await?;
                let text = std::fs::read_to_string(path)?;
                std::fs::write(
                    path,
                    text.replace(&existing.id.to_string(), &id.to_string()),
                )?;
            } else {
                admin::create_repository(storage, id, &branch, "cli").await?;
                add_repo_to_config(path, &name, id, &default_branch)?;
            }
            println!("created {name} ({id})");
        }
        RepoCmd::Delete { name } => {
            let head = admin::delete_repository(storage, repo_id(cfg, &name)?, "cli").await?;
            println!("deleted {name} at generation {}", head.generation);
        }
        RepoCmd::SetDefaultBranch { name, branch } => {
            let head = admin::set_default_branch(
                storage,
                repo_id(cfg, &name)?,
                branch_ref(&branch)?,
                "cli",
            )
            .await?;
            println!(
                "default branch of {name} is {branch} (generation {})",
                head.generation
            );
        }
    }
    Ok(())
}

fn add_repo_to_config(
    path: &Path,
    name: &str,
    id: RepoId,
    default_branch: &str,
) -> anyhow::Result<()> {
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new().append(true).open(path)?;
    write!(
        f,
        "\n[[repos]]\nname = {name:?}\nid = \"{id}\"\ndefault_branch = {default_branch:?}\n"
    )?;
    Ok(())
}
