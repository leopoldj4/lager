use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use clap::{Parser, Subcommand};
use worktree_core::catalog::Catalog;
use worktree_core::config::Config;
use worktree_core::identity::{self, NewToken, NewUser, UserChange};
use worktree_core::model::RepoId;
use worktree_core::repo::{Access, RefRule, RepoSettings};
use worktree_core::storage::Storage;

#[derive(Parser)]
#[command(
    name = "worktree",
    about = "Administration of a worktree Git server and its storage"
)]
struct Cli {
    /// Server configuration [default: worktree.toml].
    #[arg(long, short, env = "WORKTREE_CONFIG", global = true)]
    config: Option<PathBuf>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Users in the database.
    User {
        #[command(subcommand)]
        cmd: UserCmd,
    },
    /// Personal access tokens of users, for Git in place of the password.
    Token {
        #[command(subcommand)]
        cmd: TokenCmd,
    },
    /// Repositories in the catalog in storage.
    Repo {
        #[command(subcommand)]
        cmd: RepoCmd,
    },
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
    /// Backup and recovery.
    Backup {
        #[command(subcommand)]
        cmd: BackupCmd,
    },
}

#[derive(Subcommand)]
enum RepoCmd {
    /// Create a repository.
    Create {
        name: String,
        #[arg(long, default_value = "main")]
        default_branch: String,
    },
    /// Mark a repository deleted (tombstone).
    Delete { name: String },
    /// Change the default branch (Git HEAD).
    SetDefaultBranch { name: String, branch: String },
    /// Let anyone read a repository without signing in (`true`), or only
    /// users with access (`false`).
    SetPublic {
        name: String,
        #[arg(action = clap::ArgAction::Set)]
        public: bool,
    },
    /// Add or replace the ref rule for a pattern: a ref name, or a prefix
    /// ending in `*` such as refs/heads/release/*. Levels not given keep
    /// their defaults (write, force, write).
    SetRefRule {
        name: String,
        pattern: String,
        /// Creation and fast-forward updates.
        #[arg(long)]
        update: Option<Access>,
        /// Non-fast-forward updates and tag moves.
        #[arg(long)]
        force: Option<Access>,
        #[arg(long)]
        delete: Option<Access>,
    },
    /// Remove the ref rule for a pattern.
    DeleteRefRule { name: String, pattern: String },
    /// Show a repository's settings.
    Show { name: String },
    /// List the repositories.
    List,
}

#[derive(Subcommand)]
enum UserCmd {
    /// Create a user. Asks for the password.
    Create {
        name: String,
        /// An access level for a repository (or "*"), such as org/app=write.
        /// Repeatable.
        #[arg(long, value_name = "REPO=LEVEL")]
        access: Vec<String>,
    },
    /// List the users.
    List,
    /// Change a user's password. Asks for it.
    SetPassword { name: String },
    /// Replace all of a user's access levels; none removes all access.
    SetAccess {
        name: String,
        #[arg(value_name = "REPO=LEVEL")]
        access: Vec<String>,
    },
    /// Delete a user and its tokens.
    Delete { name: String },
}

#[derive(Subcommand)]
enum TokenCmd {
    /// Create a token for a user and print it once.
    Create {
        user: String,
        /// What the token is for, such as "CI for org/app".
        name: String,
        /// Limit the token to a level for a repository (or "*"), such as
        /// org/app=read. Repeatable. Without it, the token has the user's
        /// access.
        #[arg(long, value_name = "REPO=LEVEL")]
        access: Vec<String>,
        #[arg(long)]
        expires_days: Option<u32>,
    },
    /// List a user's tokens.
    List { user: String },
    /// Delete a token. Git clients that use it stop working.
    Delete { user: String, id: i64 },
}

#[derive(Subcommand)]
enum BackupCmd {
    Export {
        name: String,
        out: PathBuf,
    },
    /// Import into the configured storage. With --name, give it that name.
    Import {
        dir: PathBuf,
        #[arg(long)]
        name: Option<String>,
    },
}

fn main() {
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
    let cmd = match cli.cmd {
        Cmd::User { cmd } => return user_cmd(&cfg, cmd).await,
        Cmd::Token { cmd } => return token_cmd(&cfg, cmd).await,
        cmd => cmd,
    };
    let storage = Storage::from_config(&cfg)?;
    // Every command reads the current record, never a cached one.
    let catalog = Catalog::new(storage.clone(), Duration::ZERO);
    match cmd {
        Cmd::Repo { cmd } => repo_cmd(&catalog, cmd).await?,
        Cmd::Cleanup {
            name,
            pack_retention_hours,
            record_retention_hours,
            delete,
            unsafe_short_retention,
        } => {
            let id = repo_id(&catalog, &name).await?;
            let opts = worktree_core::cleanup::CleanupOptions {
                pack_retention: Duration::from_secs(pack_retention_hours * 3600),
                record_retention: Duration::from_secs(record_retention_hours * 3600),
                removal_window: Duration::from_secs(600),
                clock_margin: worktree_core::cleanup::CLOCK_MARGIN,
                report_only: !delete,
            };
            let min = worktree_core::cleanup::CleanupOptions::minimum_retention();
            anyhow::ensure!(
                unsafe_short_retention || opts.pack_retention.min(opts.record_retention) >= min,
                "retention periods must be at least {} seconds",
                min.as_secs()
            );
            let report = worktree_core::cleanup::cleanup(&storage, id, &opts).await?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        Cmd::User { .. } | Cmd::Token { .. } => {
            unreachable!("handled before storage is opened")
        }
        Cmd::Backup {
            cmd: BackupCmd::Export { name, out },
        } => {
            let m = worktree_core::backup::export(&storage, repo_id(&catalog, &name).await?, &out)
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
            let head = worktree_core::backup::import(&storage, &dir).await?;
            if let Some(name) = name {
                catalog
                    .attach(&RepoSettings::new(&name, head.repository))
                    .await?;
                println!("named it {name}");
            }
            println!(
                "imported repository {} at generation {}",
                head.repository, head.generation
            );
        }
    }
    Ok(())
}

async fn repo_id(catalog: &Catalog, name: &str) -> anyhow::Result<RepoId> {
    catalog
        .lookup(name)
        .await?
        .map(|r| r.id)
        .ok_or_else(|| anyhow::anyhow!("unknown repository {name}"))
}

async fn repo_cmd(catalog: &Catalog, cmd: RepoCmd) -> anyhow::Result<()> {
    match cmd {
        RepoCmd::Create {
            name,
            default_branch,
        } => {
            let mut settings = RepoSettings::new(&name, RepoId::new());
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
        RepoCmd::SetPublic { name, public } => {
            catalog.update(&name, |r| r.anonymous_read = public).await?;
            println!("{name} is {}", if public { "public" } else { "private" });
        }
        RepoCmd::SetRefRule {
            name,
            pattern,
            update,
            force,
            delete,
        } => {
            let mut rule = RefRule::new(&pattern);
            rule.update = update.unwrap_or(rule.update);
            rule.force = force.unwrap_or(rule.force);
            rule.delete = delete.unwrap_or(rule.delete);
            worktree_core::repo::validate_ref_rule(&rule)?;
            let r = catalog
                .update(&name, |r| {
                    r.ref_rules.retain(|x| x.pattern != rule.pattern);
                    r.ref_rules.push(rule.clone());
                })
                .await?;
            print_ref_rules(&r);
        }
        RepoCmd::DeleteRefRule { name, pattern } => {
            let before = catalog
                .lookup(&name)
                .await?
                .ok_or_else(|| anyhow::anyhow!("unknown repository {name}"))?;
            anyhow::ensure!(
                before.ref_rules.iter().any(|x| x.pattern == pattern),
                "{name} has no ref rule for {pattern}"
            );
            let r = catalog
                .update(&name, |r| r.ref_rules.retain(|x| x.pattern != pattern))
                .await?;
            print_ref_rules(&r);
        }
        RepoCmd::Show { name } => {
            let r = catalog
                .lookup(&name)
                .await?
                .ok_or_else(|| anyhow::anyhow!("unknown repository {name}"))?;
            println!("name\t{}", r.name);
            println!("id\t{}", r.id);
            println!("default branch\t{}", r.default_branch);
            println!("public\t{}", r.anonymous_read);
            print_ref_rules(&r);
        }
        RepoCmd::List => {
            for r in catalog.list().await? {
                println!("{}\t{}\t{:?}", r.name, r.repository, r.state);
            }
        }
    }
    Ok(())
}

fn print_ref_rules(r: &RepoSettings) {
    if r.ref_rules.is_empty() {
        println!("ref rules\tnone");
    }
    for rule in &r.ref_rules {
        println!(
            "ref rule\t{}\tupdate={} force={} delete={}",
            rule.pattern,
            rule.update.as_str(),
            rule.force.as_str(),
            rule.delete.as_str()
        );
    }
}

const DEFAULT_CONFIG: &str = "worktree.toml";

/// Who changed a user, as the database records it.
fn operator() -> String {
    format!(
        "cli:{}",
        std::env::var("USER").unwrap_or_else(|_| "unknown".into())
    )
}

/// `REPO=LEVEL` arguments.
fn parse_access(args: &[String]) -> anyhow::Result<BTreeMap<String, Access>> {
    let mut out = BTreeMap::new();
    for a in args {
        let (repo, level) = a
            .rsplit_once('=')
            .ok_or_else(|| anyhow::anyhow!("{a:?}: use REPO=LEVEL"))?;
        out.insert(
            repo.to_string(),
            level.parse().map_err(|e: String| anyhow::anyhow!(e))?,
        );
    }
    identity::validate_access(&out)?;
    Ok(out)
}

fn format_access(access: &BTreeMap<String, Access>) -> String {
    if access.is_empty() {
        return "-".into();
    }
    access
        .iter()
        .map(|(r, l)| format!("{r}={}", l.as_str()))
        .collect::<Vec<_>>()
        .join(",")
}

/// Asks for a new password (twice on a terminal) and hashes it.
fn new_password_hash() -> anyhow::Result<String> {
    use std::io::IsTerminal;
    let password = prompt("Password: ", true)?;
    if std::io::stdin().is_terminal() {
        anyhow::ensure!(
            prompt("Repeat: ", true)? == password,
            "the passwords differ"
        );
    }
    identity::validate_password(&password)?;
    Ok(worktree_core::password::PasswordHash::new(
        &password,
        worktree_core::password::DEFAULT_ITERATIONS,
    )
    .to_string())
}

async fn user_cmd(cfg: &Config, cmd: UserCmd) -> anyhow::Result<()> {
    let ids = identity::connect(cfg).await?;
    match cmd {
        UserCmd::Create { name, access } => {
            identity::validate_user_name(&name)?;
            let access = parse_access(&access)?;
            let password_hash = new_password_hash()?;
            let new = NewUser {
                name,
                password_hash,
                access,
            };
            let u = ids.create(new, &operator()).await?;
            println!("created user {} ({})", u.name, format_access(&u.access));
        }
        UserCmd::List => {
            for u in ids.list().await? {
                println!("{}\t{}", u.name, format_access(&u.access));
            }
        }
        UserCmd::SetPassword { name } => {
            let change = UserChange {
                password_hash: Some(new_password_hash()?),
                access: None,
            };
            ids.update(&name, change, &operator()).await?;
            println!("changed the password of {name}; its tokens still work");
        }
        UserCmd::SetAccess { name, access } => {
            let change = UserChange {
                password_hash: None,
                access: Some(parse_access(&access)?),
            };
            let u = ids.update(&name, change, &operator()).await?;
            println!("{}: {}", u.name, format_access(&u.access));
        }
        UserCmd::Delete { name } => {
            ids.delete(&name, &operator()).await?;
            println!("deleted user {name} and its tokens");
        }
    }
    Ok(())
}

async fn token_cmd(cfg: &Config, cmd: TokenCmd) -> anyhow::Result<()> {
    let ids = identity::connect(cfg).await?;
    match cmd {
        TokenCmd::Create {
            user,
            name,
            access,
            expires_days,
        } => {
            let new = NewToken {
                name,
                access: (!access.is_empty())
                    .then(|| parse_access(&access))
                    .transpose()?,
                expires_days,
            };
            identity::validate_token(&new)?;
            let (t, secret) = ids.create_token(&user, new).await?;
            eprintln!("created token {} for {user}; it is shown only now:", t.id);
            println!("{secret}");
        }
        TokenCmd::List { user } => {
            for t in ids.tokens(&user).await? {
                let access = t
                    .access
                    .as_ref()
                    .map_or("user's access".into(), format_access);
                let expires = t
                    .expires_ms
                    .map_or("no expiry".into(), |ms| format!("expires {}", relative(ms)));
                let used = t
                    .last_used_ms
                    .map_or("never used".into(), |ms| format!("used {}", relative(ms)));
                println!(
                    "{}\t{}\t{access}\tcreated {}\t{expires}\t{used}",
                    t.id,
                    t.name,
                    relative(t.created_ms)
                );
            }
        }
        TokenCmd::Delete { user, id } => {
            ids.delete_token(&user, id).await?;
            println!("deleted token {id} of {user}");
        }
    }
    Ok(())
}

/// "3 days ago" or "in 3 days" for a time in milliseconds since 1970.
fn relative(ms: u64) -> String {
    let now = worktree_core::model::now_ms();
    let (d, future) = if ms > now {
        (ms - now, true)
    } else {
        (now - ms, false)
    };
    let s = d / 1000;
    let amount = match s {
        0..60 => format!("{s} s"),
        60..3600 => format!("{} min", s / 60),
        3600..86400 => format!("{} h", s / 3600),
        _ => format!("{} days", (s + 43200) / 86400),
    };
    if future {
        format!("in {amount}")
    } else {
        format!("{amount} ago")
    }
}

/// Reads a line from the terminal, or from standard input when it is not a
/// terminal. With `hidden`, the terminal does not show what is typed.
pub fn prompt(label: &str, hidden: bool) -> anyhow::Result<String> {
    use std::io::{BufRead, IsTerminal, Write};
    let stdin = std::io::stdin();
    let terminal = stdin.is_terminal();
    if terminal {
        eprint!("{label}");
        std::io::stderr().flush()?;
    }
    let restore = if terminal && hidden { echo_off() } else { None };
    let mut line = String::new();
    let read = stdin.lock().read_line(&mut line);
    if let Some(saved) = restore {
        // SAFETY: restores the attributes read from the same descriptor.
        unsafe { libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &saved) };
    }
    anyhow::ensure!(
        read? > 0,
        "no input for {}",
        label.trim_end_matches([':', ' '])
    );
    Ok(line.trim_end_matches(['\r', '\n']).to_string())
}

/// Turns off terminal echo; returns the previous attributes.
fn echo_off() -> Option<libc::termios> {
    // SAFETY: `t` is a valid out pointer for the attributes of stdin.
    unsafe {
        let mut t: libc::termios = std::mem::zeroed();
        if libc::tcgetattr(libc::STDIN_FILENO, &mut t) != 0 {
            return None;
        }
        let saved = t;
        t.c_lflag &= !libc::ECHO;
        t.c_lflag |= libc::ECHONL;
        (libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &t) == 0).then_some(saved)
    }
}
