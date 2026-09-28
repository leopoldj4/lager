//! Git processes, hooks, packs, and ref transactions.
//!
//! All Git processes run with a cleared environment, no system or global
//! configuration, and arguments passed without a shell.

pub mod hook;
pub mod pktline;

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::process::Stdio;

use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

use crate::model::{ObjectFormat, Oid, RefName, RefUpdate};

/// Minimum Git version: `proc-receive` and `update-ref --stdin` transactions
/// with `--ref-format`.
pub const MIN_VERSION: (u32, u32) = (2, 45);

#[derive(Debug, thiserror::Error)]
pub enum GitError {
    #[error("git {cmd} failed: {stderr}")]
    Failed { cmd: String, stderr: String },
    #[error("git io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("unexpected git output: {0}")]
    Protocol(String),
}

#[derive(Clone, Debug)]
pub struct Git {
    binary: PathBuf,
    home: PathBuf,
    path_env: String,
    /// Directory containing the `proc-receive` hook.
    hooks_dir: PathBuf,
    /// CPU seconds and address-space bytes for client request processes.
    cpu_secs: u64,
    memory_bytes: u64,
}

impl Git {
    /// `work_dir` holds an empty home directory and the hooks directory.
    pub fn new(binary: PathBuf, work_dir: &Path, hook_binary: &Path) -> std::io::Result<Git> {
        let home = work_dir.join("home");
        let hooks_dir = work_dir.join("hooks");
        std::fs::create_dir_all(&home)?;
        std::fs::create_dir_all(&hooks_dir)?;
        let link = hooks_dir.join("proc-receive");
        let _ = std::fs::remove_file(&link);
        std::os::unix::fs::symlink(hook_binary, &link)?;
        let path_env = std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin".into());
        Ok(Git {
            binary,
            home,
            path_env,
            hooks_dir,
            cpu_secs: 0,
            memory_bytes: 0,
        })
    }

    /// Resource limits for the Git processes of client requests. 0: none.
    pub fn with_process_limits(mut self, cpu_secs: u64, memory_bytes: u64) -> Self {
        self.cpu_secs = cpu_secs;
        self.memory_bytes = memory_bytes;
        self
    }

    /// A Git command for a client request, with CPU and memory limits.
    pub fn service_command(&self) -> Command {
        let mut c = self.command();
        let (cpu, mem) = (self.cpu_secs, self.memory_bytes);
        if cpu > 0 || mem > 0 {
            // SAFETY: the closure only calls setrlimit, which is
            // async-signal-safe, and allocates nothing.
            unsafe {
                c.pre_exec(move || {
                    set_limit(libc::RLIMIT_CPU, cpu)?;
                    #[cfg(target_os = "linux")]
                    set_limit(libc::RLIMIT_AS, mem)?;
                    let _ = mem;
                    Ok(())
                });
            }
        }
        c
    }

    pub fn hooks_dir(&self) -> &Path {
        &self.hooks_dir
    }

    /// A Git command with a checked environment.
    pub fn command(&self) -> Command {
        let mut c = Command::new(&self.binary);
        c.env_clear()
            .env("PATH", &self.path_env)
            .env("HOME", &self.home)
            .env("XDG_CONFIG_HOME", &self.home)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_ATTR_NOSYSTEM", "1")
            .env("LC_ALL", "C")
            .stdin(Stdio::null())
            .kill_on_drop(true);
        c
    }

    /// A Git command for the repository at `dir`.
    pub fn in_repo(&self, dir: &Path) -> Command {
        let mut c = self.command();
        c.env("GIT_DIR", dir);
        c
    }

    pub async fn version(&self) -> Result<(u32, u32), GitError> {
        let out = run(self.command().arg("version"), None).await?;
        let s = String::from_utf8_lossy(&out);
        let v = s
            .split_whitespace()
            .nth(2)
            .ok_or_else(|| GitError::Protocol(s.to_string()))?;
        let mut it = v.split('.').map(|p| p.parse::<u32>().unwrap_or(0));
        Ok((it.next().unwrap_or(0), it.next().unwrap_or(0)))
    }

    /// Creates a bare cache repository with the server's fixed configuration.
    pub async fn init_cache_repo(&self, dir: &Path, format: ObjectFormat) -> Result<(), GitError> {
        run(
            self.command()
                .arg("init")
                .arg("--bare")
                .arg("--quiet")
                .arg("--ref-format=files")
                .arg(format!("--object-format={}", format.git_name()))
                .arg(dir),
            None,
        )
        .await?;
        let mut config = tokio::fs::OpenOptions::new()
            .append(true)
            .open(dir.join("config"))
            .await?;
        config.write_all(self.cache_config().as_bytes()).await?;
        config.flush().await?;
        // Git's sample hooks and description are not needed.
        let _ = tokio::fs::remove_dir_all(dir.join("hooks")).await;
        Ok(())
    }

    fn cache_config(&self) -> String {
        format!(
            "[core]\n\thooksPath = {hooks}\n\tlogAllRefUpdates = false\n\
             [gc]\n\tauto = 0\n\tautoDetach = false\n\
             [maintenance]\n\tauto = false\n\
             [receive]\n\tprocReceiveRefs = refs/\n\tfsckObjects = true\n\tautogc = false\n\
             \tadvertisePushOptions = false\n\tshallowUpdate = false\n\
             [transfer]\n\tfsckObjects = true\n\
             {read}\
             [pack]\n\twindowMemory = 256m\n\tthreads = 2\n\
             [fetch]\n\twriteCommitGraph = false\n",
            hooks = config_quote(&self.hooks_dir.to_string_lossy()),
            read = read_config(),
        )
    }

    /// Runs `git index-pack --stdin` so the pack lands in `objects/pack`.
    pub async fn index_pack(&self, dir: &Path, pack: &Path) -> Result<(), GitError> {
        let file = std::fs::File::open(pack)?;
        let mut c = self.in_repo(dir);
        c.arg("index-pack")
            .arg("--stdin")
            .stdin(Stdio::from(file))
            .stdout(Stdio::null());
        run_redirected(&mut c, None).await?;
        Ok(())
    }

    /// Writes a complete (not thin) pack of objects reachable from `include`
    /// and not from `exclude`. Returns the number of objects.
    pub async fn pack_objects(
        &self,
        dir: &Path,
        include: &[Oid],
        exclude: &[Oid],
        out: &Path,
    ) -> Result<u32, GitError> {
        let file = std::fs::File::create(out)?;
        let mut input = String::new();
        for o in include {
            input.push_str(o.as_str());
            input.push('\n');
        }
        for o in exclude {
            input.push('^');
            input.push_str(o.as_str());
            input.push('\n');
        }
        let mut c = self.in_repo(dir);
        c.args([
            "pack-objects",
            "--revs",
            "--stdout",
            "--delta-base-offset",
            "-q",
        ])
        .stdout(Stdio::from(file));
        run_redirected(&mut c, Some(input.into_bytes())).await?;
        let mut f = tokio::fs::File::open(out).await?;
        let mut header = [0u8; 12];
        f.read_exact(&mut header).await?;
        if &header[..4] != b"PACK" {
            return Err(GitError::Protocol(
                "pack-objects wrote no pack header".into(),
            ));
        }
        Ok(u32::from_be_bytes([
            header[8], header[9], header[10], header[11],
        ]))
    }

    /// Total and largest size, after decompression, of the objects reachable
    /// from `include` and not from `exclude`. Streams the object list.
    pub async fn object_sizes(
        &self,
        dir: &Path,
        include: &[Oid],
        exclude: &[Oid],
    ) -> Result<(u64, u64), GitError> {
        let mut input = String::new();
        for o in include {
            input.push_str(o.as_str());
            input.push('\n');
        }
        for o in exclude {
            input.push('^');
            input.push_str(o.as_str());
            input.push('\n');
        }
        let mut list = self
            .in_repo(dir)
            .args(["rev-list", "--objects", "--no-object-names", "--stdin"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let mut stdin = list.stdin.take().expect("piped");
        let writer = tokio::spawn(async move {
            let _ = stdin.write_all(input.as_bytes()).await;
        });
        let oids: Stdio = list.stdout.take().expect("piped").try_into()?;
        let mut sizes = self
            .in_repo(dir)
            .args(["cat-file", "--batch-check=%(objectsize)"])
            .stdin(oids)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let mut lines = BufReader::new(sizes.stdout.take().expect("piped")).lines();
        let (mut total, mut largest) = (0u64, 0u64);
        while let Some(line) = lines.next_line().await? {
            let n: u64 = line
                .trim()
                .parse()
                .map_err(|_| GitError::Protocol(format!("object size {line:?}")))?;
            total = total.saturating_add(n);
            largest = largest.max(n);
        }
        let _ = writer.await;
        let (a, b) = (list.wait().await?, sizes.wait().await?);
        if !a.success() || !b.success() {
            return Err(GitError::Failed {
                cmd: "rev-list".into(),
                stderr: format!("{a}, {b}"),
            });
        }
        Ok((total, largest))
    }

    pub async fn is_ancestor(&self, dir: &Path, old: &Oid, new: &Oid) -> Result<bool, GitError> {
        let status = self
            .in_repo(dir)
            .args(["merge-base", "--is-ancestor", old.as_str(), new.as_str()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await?;
        match status.code() {
            Some(0) => Ok(true),
            Some(1) => Ok(false),
            _ => Err(GitError::Failed {
                cmd: "merge-base".into(),
                stderr: format!("{status}"),
            }),
        }
    }

    /// Object types; `None` for missing objects.
    pub async fn object_types(
        &self,
        dir: &Path,
        oids: &[Oid],
    ) -> Result<HashMap<Oid, Option<String>>, GitError> {
        let mut input = String::new();
        for o in oids {
            input.push_str(o.as_str());
            input.push('\n');
        }
        let out = run(
            self.in_repo(dir)
                .args(["cat-file", "--batch-check=%(objectname) %(objecttype)"]),
            Some(input.into_bytes()),
        )
        .await?;
        let mut map = HashMap::new();
        for (line, oid) in String::from_utf8_lossy(&out).lines().zip(oids) {
            let ty = match line.split_once(' ') {
                Some((_, "missing")) | None => None,
                Some((_, t)) => Some(t.to_string()),
            };
            map.insert(oid.clone(), ty);
        }
        Ok(map)
    }

    /// Checks that every object reachable from the refs exists.
    pub async fn check_connectivity(&self, dir: &Path) -> Result<(), GitError> {
        run(
            self.in_repo(dir).args([
                "fsck",
                "--connectivity-only",
                "--no-dangling",
                "--no-progress",
            ]),
            None,
        )
        .await?;
        Ok(())
    }

    pub async fn refs(
        &self,
        dir: &Path,
        format: ObjectFormat,
    ) -> Result<BTreeMap<RefName, Oid>, GitError> {
        let out = run(
            self.in_repo(dir)
                .args(["for-each-ref", "--format=%(objectname) %(refname)"]),
            None,
        )
        .await?;
        let mut map = BTreeMap::new();
        for line in String::from_utf8_lossy(&out).lines() {
            let (oid, name) = line
                .split_once(' ')
                .ok_or_else(|| GitError::Protocol(line.into()))?;
            let name = RefName::parse(name).map_err(|e| GitError::Protocol(e.to_string()))?;
            let oid = Oid::parse(format, oid).map_err(|e| GitError::Protocol(e.to_string()))?;
            map.insert(name, oid);
        }
        Ok(map)
    }

    pub async fn set_head(&self, dir: &Path, target: &RefName) -> Result<(), GitError> {
        run(
            self.in_repo(dir)
                .args(["symbolic-ref", "HEAD", target.as_str()]),
            None,
        )
        .await?;
        Ok(())
    }

    pub async fn head_target(&self, dir: &Path) -> Result<String, GitError> {
        let out = run(self.in_repo(dir).args(["symbolic-ref", "HEAD"]), None).await?;
        Ok(String::from_utf8_lossy(&out).trim().to_string())
    }

    /// Starts a ref transaction and prepares it: the ref locks are held until
    /// `commit` or `abort`.
    pub async fn prepare_ref_transaction(
        &self,
        dir: &Path,
        format: ObjectFormat,
        updates: &[RefUpdate],
    ) -> Result<RefTransaction, GitError> {
        let mut child = self
            .in_repo(dir)
            .args(["update-ref", "--stdin", "-z"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(false)
            .spawn()?;
        let stdin = child.stdin.take().expect("piped");
        let stdout = BufReader::new(child.stdout.take().expect("piped"));
        let mut t = RefTransaction {
            child: Some(child),
            stdin: Some(stdin),
            stdout,
        };
        let mut cmds = Vec::new();
        for u in updates {
            cmds.extend_from_slice(b"update ");
            cmds.extend_from_slice(u.name.as_str().as_bytes());
            cmds.push(0);
            cmds.extend_from_slice(Oid::to_git(u.new.as_ref(), format).as_bytes());
            cmds.push(0);
            cmds.extend_from_slice(Oid::to_git(u.old.as_ref(), format).as_bytes());
            cmds.push(0);
        }
        t.send(b"start\0", "start").await?;
        t.write(&cmds).await?;
        t.send(b"prepare\0", "prepare").await?;
        Ok(t)
    }
}

/// A prepared `git update-ref --stdin` transaction.
pub struct RefTransaction {
    child: Option<Child>,
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
}

impl RefTransaction {
    async fn write(&mut self, data: &[u8]) -> Result<(), GitError> {
        let stdin = self.stdin.as_mut().expect("open");
        if let Err(e) = stdin.write_all(data).await {
            return Err(self.failure(e.to_string()).await);
        }
        Ok(())
    }

    async fn send(&mut self, cmd: &[u8], name: &str) -> Result<(), GitError> {
        self.write(cmd).await?;
        let stdin = self.stdin.as_mut().expect("open");
        if let Err(e) = stdin.flush().await {
            return Err(self.failure(e.to_string()).await);
        }
        let mut line = String::new();
        self.stdout.read_line(&mut line).await?;
        if line.trim_end() != format!("{name}: ok") {
            return Err(self.failure(line).await);
        }
        Ok(())
    }

    async fn failure(&mut self, context: String) -> GitError {
        self.stdin.take();
        let mut stderr = String::new();
        if let Some(mut child) = self.child.take() {
            if let Some(mut e) = child.stderr.take() {
                let _ = e.read_to_string(&mut stderr).await;
            }
            let _ = child.wait().await;
        }
        GitError::Failed {
            cmd: "update-ref".into(),
            stderr: format!("{} {}", stderr.trim(), context.trim()),
        }
    }

    async fn finish(mut self, cmd: &[u8], name: &str) -> Result<(), GitError> {
        self.send(cmd, name).await?;
        self.stdin.take();
        if let Some(mut child) = self.child.take() {
            let status = child.wait().await?;
            if !status.success() {
                return Err(GitError::Failed {
                    cmd: "update-ref".into(),
                    stderr: format!("{status}"),
                });
            }
        }
        Ok(())
    }

    pub async fn commit(self) -> Result<(), GitError> {
        self.finish(b"commit\0", "commit").await
    }

    pub async fn abort(self) -> Result<(), GitError> {
        self.finish(b"abort\0", "abort").await
    }
}

impl Drop for RefTransaction {
    fn drop(&mut self) {
        // Closing stdin makes Git roll back the open transaction and remove
        // its lock files. Wait for the process so it does not linger.
        self.stdin.take();
        if let Some(mut child) = self.child.take()
            && let Ok(handle) = tokio::runtime::Handle::try_current()
        {
            handle.spawn(async move {
                let _ = child.wait().await;
            });
        }
    }
}

/// Runs a command to completion with `input` on stdin. Returns stdout, or
/// stderr in the error.
pub async fn run(cmd: &mut Command, input: Option<Vec<u8>>) -> Result<Vec<u8>, GitError> {
    cmd.stdin(if input.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    });
    cmd.stdout(Stdio::piped());
    exec(cmd, input).await
}

/// Like [`run`], but keeps the stdin and stdout the caller configured.
pub async fn run_redirected(
    cmd: &mut Command,
    input: Option<Vec<u8>>,
) -> Result<Vec<u8>, GitError> {
    if input.is_some() {
        cmd.stdin(Stdio::piped());
    }
    exec(cmd, input).await
}

async fn exec(cmd: &mut Command, input: Option<Vec<u8>>) -> Result<Vec<u8>, GitError> {
    let name = cmd
        .as_std()
        .get_args()
        .next()
        .map(|a| a.to_string_lossy().into_owned())
        .unwrap_or_default();
    cmd.stderr(Stdio::piped());
    let mut child = cmd.spawn()?;
    let writer = match (input, child.stdin.take()) {
        (Some(data), Some(mut stdin)) => Some(tokio::spawn(async move {
            let _ = stdin.write_all(&data).await;
        })),
        _ => None,
    };
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let out_pipe = child.stdout.take();
    let err_pipe = child.stderr.take();
    let (r1, r2) = tokio::join!(
        async {
            if let Some(mut p) = out_pipe {
                p.read_to_end(&mut stdout).await?;
            }
            Ok::<_, std::io::Error>(())
        },
        async {
            if let Some(p) = err_pipe {
                // Keep at most 64 KiB of diagnostics, but drain the pipe.
                let mut limited = p.take(64 * 1024);
                limited.read_to_end(&mut stderr).await?;
                tokio::io::copy(&mut limited.into_inner(), &mut tokio::io::sink()).await?;
            }
            Ok::<_, std::io::Error>(())
        }
    );
    r1?;
    r2?;
    let status = child.wait().await?;
    if let Some(w) = writer {
        let _ = w.await;
    }
    if !status.success() {
        return Err(GitError::Failed {
            cmd: name,
            stderr: String::from_utf8_lossy(&stderr).trim().to_string(),
        });
    }
    Ok(stdout)
}

/// Upload-pack configuration: serve only objects reachable from advertised
/// refs, and no partial clone (a later extension).
pub fn read_config() -> &'static str {
    "[uploadpack]\n\tallowAnySHA1InWant = false\n\tallowReachableSHA1InWant = false\n\
     \tallowTipSHA1InWant = false\n\tallowFilter = false\n\tallowRefInWant = false\n"
}

/// The resource argument of `setrlimit`: an unsigned enum in glibc.
#[cfg(all(target_os = "linux", target_env = "gnu"))]
type Resource = libc::__rlimit_resource_t;
#[cfg(not(all(target_os = "linux", target_env = "gnu")))]
type Resource = libc::c_int;

fn set_limit(resource: Resource, value: u64) -> std::io::Result<()> {
    if value == 0 {
        return Ok(());
    }
    let l = libc::rlimit {
        rlim_cur: value as libc::rlim_t,
        rlim_max: value as libc::rlim_t,
    };
    // SAFETY: `l` is a valid rlimit value.
    if unsafe { libc::setrlimit(resource, &l) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

fn config_quote(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The limits apply to client request processes only. A shell stands in
    /// for Git so that the limit can be read back.
    #[tokio::test]
    async fn client_request_processes_get_the_cpu_limit() {
        let dir = tempfile::tempdir().unwrap();
        let sh = Git::new("/bin/sh".into(), dir.path(), Path::new("/bin/true"))
            .unwrap()
            .with_process_limits(7, 0);
        let limited = run(sh.service_command().args(["-c", "ulimit -t"]), None)
            .await
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&limited).trim(), "7");
        let internal = run(sh.command().args(["-c", "ulimit -t"]), None)
            .await
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&internal).trim(), "unlimited");
    }
}
