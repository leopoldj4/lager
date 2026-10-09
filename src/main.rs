use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use clap::{Parser, Subcommand};
use rustgit::catalog::Catalog;
use rustgit::client::{self, Client, Login};
use rustgit::config::{CatalogKind, Config, validate_repo_name};
use rustgit::model::{RepoId, branch_ref};
use rustgit::repository::admin;
use rustgit::storage::Storage;

#[derive(Parser)]
#[command(name = "rustgit", about = "Git server with S3-compatible storage")]
struct Cli {
    /// Server configuration [default: rustgit.toml]. Repository commands
    /// without it go through the server of `rustgit login`, if any.
    #[arg(long, short, env = "RUSTGIT_CONFIG", global = true)]
    config: Option<PathBuf>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run the server.
    Serve,
    /// Keep a server URL and credentials for repository commands. Asks for
    /// what the URL does not contain; needs no configuration file.
    Login {
        /// For example https://git.example.com or https://user@git.example.com
        url: String,
        #[arg(long, short)]
        user: Option<String>,
    },
    /// Remove the credentials that `rustgit login` kept.
    Logout,
    /// Repository management: through the server after `rustgit login`,
    /// or directly in storage with a server configuration.
    Repo {
        #[command(subcommand)]
        cmd: RepoCmd,
    },
    /// Create a snapshot (combined pack) of the current generation.
    Snapshot { name: String },
    /// Find obsolete stored data. Removes it only with --delete.
    Cleanup {
        name: String,
        /// Keep obsolete packs (recoverable contents) younger than this.
        #[arg(long, default_value_t = 168)]
        pack_retention_hours: u64,
        /// Keep receipts and other obsolete records younger than this.
        #[arg(long, default_value_t = 720)]
        record_retention_hours: u64,
        /// Remove the reported data. Without it, only report.
        #[arg(long)]
        delete: bool,
        /// Permit retention periods shorter than active operations need.
        #[arg(long, hide = true)]
        unsafe_short_retention: bool,
    },
    /// Read a password from standard input and print a salted hash for
    /// `password_hash` in the configuration file.
    HashPassword,
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
    /// List the repositories.
    List,
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
    let cmd = match cli.cmd {
        Cmd::Login { url, user } => return login(&url, user).await,
        Cmd::Logout => return logout(),
        Cmd::Repo { cmd } if cli.config.is_none() => {
            if let Some(login) = saved_login()? {
                return remote_repo_cmd(login, cmd).await;
            }
            anyhow::ensure!(
                Path::new(DEFAULT_CONFIG).exists(),
                "no {DEFAULT_CONFIG} here; run rustgit login <url> to manage \
                 repositories through a server, or pass -c <config>"
            );
            Cmd::Repo { cmd }
        }
        cmd => cmd,
    };
    let config_path = cli.config.unwrap_or_else(|| DEFAULT_CONFIG.into());
    if let Cmd::HashPassword = cmd {
        let mut password = String::new();
        std::io::stdin().read_line(&mut password)?;
        let password = password.trim_end_matches(['\r', '\n']);
        anyhow::ensure!(!password.is_empty(), "empty password");
        let hash = rustgit::http::password::PasswordHash::new(
            password,
            rustgit::http::password::DEFAULT_ITERATIONS,
        );
        println!("{hash}");
        return Ok(());
    }
    let cfg = Config::load(&config_path)?;
    let storage = rustgit::server::storage_from_config(&cfg)?;
    match cmd {
        Cmd::Serve => {
            let server = rustgit::server::start(cfg, Some(config_path.clone()), storage).await?;
            eprintln!("rustgit serving on {}", server.url());
            use tokio::signal::unix::{SignalKind, signal};
            let mut term = signal(SignalKind::terminate())?;
            let mut int = signal(SignalKind::interrupt())?;
            tokio::select! {
                _ = term.recv() => {}
                _ = int.recv() => {}
            }
            eprintln!("rustgit stopping; a second signal stops at once");
            // Stopping at any point is safe: storage decides every push.
            tokio::select! {
                _ = server.shutdown() => {}
                _ = term.recv() => {}
                _ = int.recv() => {}
            }
        }
        Cmd::Repo { cmd } => repo_cmd(&config_path, &cfg, &storage, cmd).await?,
        Cmd::Snapshot { name } => {
            let id = repo_id(&cfg, &storage, &name).await?;
            let svc = cli_services(&cfg, storage).await?;
            let registry = rustgit::repository::Registry::new(svc)?;
            let report = rustgit::maintenance::snapshot(&registry.controller(id), "cli").await?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        Cmd::Cleanup {
            name,
            pack_retention_hours,
            record_retention_hours,
            delete,
            unsafe_short_retention,
        } => {
            let id = repo_id(&cfg, &storage, &name).await?;
            let opts = rustgit::maintenance::CleanupOptions {
                pack_retention: Duration::from_secs(pack_retention_hours * 3600),
                record_retention: Duration::from_secs(record_retention_hours * 3600),
                removal_window: Duration::from_secs(600),
                clock_margin: rustgit::maintenance::CLOCK_MARGIN,
                report_only: !delete,
            };
            let min = rustgit::maintenance::CleanupOptions::minimum_retention(&cfg.limits);
            anyhow::ensure!(
                unsafe_short_retention || opts.pack_retention.min(opts.record_retention) >= min,
                "retention periods must be at least {} seconds",
                min.as_secs()
            );
            let report = rustgit::maintenance::cleanup(&storage, id, &opts).await?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        Cmd::HashPassword | Cmd::Login { .. } | Cmd::Logout => {
            unreachable!("handled before the configuration loads")
        }
        Cmd::Backup {
            cmd: BackupCmd::Export { name, out },
        } => {
            let m = rustgit::backup::export(&storage, repo_id(&cfg, &storage, &name).await?, &out)
                .await?;
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
                anyhow::ensure!(
                    cfg.catalog.kind == CatalogKind::Config,
                    "--name adds the repository to the configuration file; \
                     the storage catalog assigns new UUIDs to new names"
                );
                add_repo_to_config(&config_path, &name, head.repository, "main")?;
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
    std::fs::create_dir_all(&cfg.server.cache_dir)?;
    Ok(Arc::new(rustgit::repository::Services {
        storage,
        git,
        cache_root: dir,
        limits: cfg.limits.clone(),
        metrics: Default::default(),
        notifier: Default::default(),
        disk: rustgit::http::DiskReservations::new(
            &cfg.server.cache_dir,
            cfg.limits.min_free_disk_bytes,
        ),
    }))
}

fn catalog(cfg: &Config, storage: &Storage) -> Option<Catalog> {
    (cfg.catalog.kind == CatalogKind::Storage)
        .then(|| Catalog::new(storage.clone(), Duration::ZERO))
}

async fn repo_id(cfg: &Config, storage: &Storage, name: &str) -> anyhow::Result<RepoId> {
    let found = match catalog(cfg, storage) {
        Some(c) => c.lookup(name).await?.map(|r| r.id),
        None => cfg.repos.iter().find(|r| r.name == name).map(|r| r.id),
    };
    found.ok_or_else(|| anyhow::anyhow!("unknown repository {name}"))
}

async fn repo_cmd(
    path: &Path,
    cfg: &Config,
    storage: &Storage,
    cmd: RepoCmd,
) -> anyhow::Result<()> {
    if let Some(catalog) = catalog(cfg, storage) {
        return catalog_cmd(&catalog, cmd).await;
    }
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
            let head =
                admin::delete_repository(storage, repo_id(cfg, storage, &name).await?, "cli")
                    .await?;
            println!("deleted {name} at generation {}", head.generation);
        }
        RepoCmd::SetDefaultBranch { name, branch } => {
            let head = admin::set_default_branch(
                storage,
                repo_id(cfg, storage, &name).await?,
                branch_ref(&branch)?,
                "cli",
            )
            .await?;
            println!(
                "default branch of {name} is {branch} (generation {})",
                head.generation
            );
        }
        RepoCmd::List => {
            for r in &cfg.repos {
                println!("{}\t{}", r.name, r.id);
            }
        }
    }
    Ok(())
}

async fn catalog_cmd(catalog: &Catalog, cmd: RepoCmd) -> anyhow::Result<()> {
    match cmd {
        RepoCmd::Create {
            name,
            default_branch,
        } => {
            let mut settings = rustgit::config::RepoConfig::new(&name, RepoId::new());
            settings.default_branch = default_branch;
            let r = catalog.create(&settings, "cli").await?;
            println!("created {name} ({})", r.id);
        }
        RepoCmd::Delete { name } => {
            let id = catalog.delete(&name, "cli").await?;
            println!("deleted {name} ({id})");
        }
        RepoCmd::SetDefaultBranch { name, branch } => {
            let (_, generation) = catalog.set_default_branch(&name, &branch, "cli").await?;
            println!("default branch of {name} is {branch} (generation {generation})");
        }
        RepoCmd::List => {
            for r in catalog.list().await? {
                println!("{}\t{}\t{:?}", r.name, r.repository, r.state);
            }
        }
    }
    Ok(())
}

const DEFAULT_CONFIG: &str = "rustgit.toml";

fn login_path() -> anyhow::Result<PathBuf> {
    Login::default_path().ok_or_else(|| anyhow::anyhow!("HOME is not set"))
}

fn saved_login() -> anyhow::Result<Option<Login>> {
    Login::load(&login_path()?)
}

/// Checks the credentials with the server before keeping them.
async fn login(url: &str, user: Option<String>) -> anyhow::Result<()> {
    let (url, url_user, url_password) = client::parse_server_url(url)?;
    let user = match user.or(url_user) {
        Some(u) => u,
        None => client::prompt("Username: ", false)?,
    };
    let password = match url_password {
        Some(p) => p,
        None => client::prompt("Password: ", true)?,
    };
    let client = Client::new(Login {
        url,
        user,
        password,
    })?;
    let me = client.me().await?;
    let path = login_path()?;
    client.login().save(&path)?;
    println!("logged in to {} as {}", client.login().url, me.name);
    if !me.admin {
        println!("repository commands need admin access to every repository (\"*\" = \"admin\")");
    }
    Ok(())
}

fn logout() -> anyhow::Result<()> {
    let path = login_path()?;
    match Login::load(&path)? {
        Some(l) => {
            std::fs::remove_file(&path)?;
            println!("logged out of {}", l.url);
        }
        None => println!("not logged in"),
    }
    Ok(())
}

async fn remote_repo_cmd(login: Login, cmd: RepoCmd) -> anyhow::Result<()> {
    let client = Client::new(login)?;
    match cmd {
        RepoCmd::Create {
            name,
            default_branch,
        } => {
            let r = client.create_repo(&name, &default_branch).await?;
            println!("created {name} ({})", r.id);
            println!("clone with: git clone {}/{name}.git", client.login().url);
        }
        RepoCmd::Delete { name } => {
            client.delete_repo(&name).await?;
            println!("deleted {name}");
        }
        RepoCmd::SetDefaultBranch { name, branch } => {
            let r = client.set_default_branch(&name, &branch).await?;
            println!(
                "default branch of {name} is {branch} (generation {})",
                r.generation.unwrap_or(0)
            );
        }
        RepoCmd::List => {
            for r in client.repos().await? {
                println!("{}\t{}\t{}", r.name, r.id, r.default_branch);
            }
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
