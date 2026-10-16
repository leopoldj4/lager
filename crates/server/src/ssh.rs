//! Git over SSH (docs/decisions/0022-git-over-ssh.md).
//!
//! Clients sign in with a public key of a user; the SSH user name is
//! ignored. A session channel runs one `git-upload-pack` or
//! `git-receive-pack` for a repository name: no shell, no terminal, no
//! forwarding. Permissions, limits, and pushes are those of smart HTTP;
//! the Git process runs without `--stateless-rpc`, connected to the
//! channel.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use axum::response::Response;
use russh::keys::{PrivateKey, PublicKey};
use russh::server::{Auth, Config, Msg, Session};
use russh::{Channel, ChannelId, MethodKind, MethodSet};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::{Notify, watch};
use tracing::Instrument;

use crate::git::hook;
use crate::http::process::Guards;
use crate::http::{
    AppState, Service, authorize_identity, default_branch_changed, filter_git_protocol,
    process_permit, push_access, repo_error,
};
use crate::repository::{Origin, PendingPush};
use crate::server::stopped;
use worktree_core::identity::{AuthError, Identity, ssh_fingerprint};
use worktree_core::limits;
use worktree_core::model::RequestId;

/// Session channels a connection can have open at the same time. Git uses
/// one.
const MAX_CHANNELS: usize = 4;
/// What a client sends to `git-upload-pack`: negotiation only.
const MAX_UPLOAD_PACK_INPUT: u64 = 64 * 1024 * 1024;
/// Diagnostics from Git passed on to the client.
const MAX_STDERR: u64 = 64 * 1024;

/// Reads the host key: an OpenSSH private key without a passphrase.
pub fn load_host_key(path: &Path) -> anyhow::Result<PrivateKey> {
    russh::keys::load_secret_key(path, None)
        .map_err(|e| anyhow::anyhow!("ssh.host_key {}: {e}", path.display()))
}

pub fn config(host_key: PrivateKey) -> Arc<Config> {
    let mut methods = MethodSet::empty();
    methods.push(MethodKind::PublicKey);
    Arc::new(Config {
        server_id: russh::SshId::Standard("SSH-2.0-worktree".into()),
        methods,
        keys: vec![host_key],
        // OpenSSH clients start with "none" to learn the methods.
        auth_rejection_time_initial: Some(Duration::ZERO),
        inactivity_timeout: Some(limits::CLIENT_IDLE_TIMEOUT),
        keepalive_interval: Some(Duration::from_secs(60)),
        nodelay: true,
        ..Default::default()
    })
}

/// Accepts connections until the stop signal. Then connections without a
/// running Git process close, and the others finish their command.
pub async fn serve(
    listener: TcpListener,
    config: Arc<Config>,
    state: AppState,
    stop: watch::Receiver<bool>,
) {
    let mut accept_stop = stop.clone();
    let mut connections = tokio::task::JoinSet::new();
    loop {
        let accepted = tokio::select! {
            a = listener.accept() => a,
            Some(_) = connections.join_next(), if !connections.is_empty() => continue,
            _ = stopped(&mut accept_stop) => break,
        };
        let Ok((tcp, peer)) = accepted else {
            continue;
        };
        let (config, state, stop) = (config.clone(), state.clone(), stop.clone());
        let span = tracing::info_span!("ssh", %peer);
        connections.spawn(
            async move {
                if let Err(e) = connection(tcp, peer, config, state, stop).await {
                    tracing::debug!(error = %e, "SSH connection ended with error");
                }
            }
            .instrument(span),
        );
    }
    drop(listener);
    while connections.join_next().await.is_some() {}
}

async fn connection(
    tcp: tokio::net::TcpStream,
    peer: SocketAddr,
    config: Arc<Config>,
    state: AppState,
    mut stop: watch::Receiver<bool>,
) -> Result<(), russh::Error> {
    state.registry.svc.metrics.inc("ssh_connections");
    let running = Arc::new(AtomicUsize::new(0));
    let finished = Arc::new(Notify::new());
    let handler = Connection {
        state,
        peer,
        identity: None,
        channels: HashMap::new(),
        running: running.clone(),
        finished: finished.clone(),
        stop: stop.clone(),
    };
    let session = russh::server::run_stream(config, tcp, handler).await?;
    let handle = session.handle();
    tokio::pin!(session);
    tokio::select! {
        r = &mut session => return r,
        _ = stopped(&mut stop) => {}
    }
    while running.load(Ordering::SeqCst) != 0 {
        tokio::select! {
            r = &mut session => return r,
            _ = finished.notified() => {}
        }
    }
    let _ = handle
        .disconnect(
            russh::Disconnect::ByApplication,
            "server stopping".into(),
            String::new(),
        )
        .await;
    session.await
}

struct Connection {
    state: AppState,
    peer: SocketAddr,
    /// The user of the accepted key.
    identity: Option<Identity>,
    /// Open channels without a command yet.
    channels: HashMap<ChannelId, PendingChannel>,
    /// Running Git processes of this connection.
    running: Arc<AtomicUsize>,
    finished: Arc<Notify>,
    stop: watch::Receiver<bool>,
}

struct PendingChannel {
    channel: Channel<Msg>,
    /// `GIT_PROTOCOL`, without version 2.
    protocol: Option<String>,
}

impl Connection {
    async fn key_user(&self, key: &PublicKey) -> Option<Identity> {
        let blob = key.to_bytes().ok()?;
        match self
            .state
            .directory
            .users()
            .authenticate_ssh_key(&ssh_fingerprint(&blob))
            .await
        {
            Ok(user) => user,
            Err(AuthError::Unavailable(m)) => {
                tracing::warn!(error = %m, "user lookup unavailable");
                None
            }
            Err(AuthError::BadCredentials) => None,
        }
    }
}

impl russh::server::Handler for Connection {
    type Error = russh::Error;

    async fn auth_publickey_offered(
        &mut self,
        _user: &str,
        key: &PublicKey,
    ) -> Result<Auth, Self::Error> {
        // Spares the client a signature for a key that no user has.
        Ok(match self.key_user(key).await {
            Some(_) => Auth::Accept,
            None => Auth::reject(),
        })
    }

    async fn auth_publickey(&mut self, _user: &str, key: &PublicKey) -> Result<Auth, Self::Error> {
        let Some(identity) = self.key_user(key).await else {
            self.state.registry.svc.metrics.inc("ssh_auth_failures");
            return Ok(Auth::reject());
        };
        tracing::debug!(user = %identity.name(), "SSH key accepted");
        self.identity = Some(identity);
        Ok(Auth::Accept)
    }

    async fn channel_open_session(
        &mut self,
        channel: Channel<Msg>,
        reply: russh::server::ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        if self.identity.is_none()
            || *self.stop.borrow()
            || self.channels.len() + self.running.load(Ordering::SeqCst) >= MAX_CHANNELS
        {
            reply
                .reject(russh::ChannelOpenFailure::ResourceShortage)
                .await;
            return Ok(());
        }
        self.channels.insert(
            channel.id(),
            PendingChannel {
                channel,
                protocol: None,
            },
        );
        reply.accept().await;
        Ok(())
    }

    async fn channel_close(
        &mut self,
        channel: ChannelId,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        self.channels.remove(&channel);
        Ok(())
    }

    async fn env_request(
        &mut self,
        channel: ChannelId,
        name: &str,
        value: &str,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        match self.channels.get_mut(&channel) {
            Some(c) if name == "GIT_PROTOCOL" => {
                c.protocol = filter_git_protocol(value);
                session.channel_success(channel)
            }
            _ => session.channel_failure(channel),
        }
    }

    async fn exec_request(
        &mut self,
        channel: ChannelId,
        data: &[u8],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        let (Some(pending), Some(identity)) =
            (self.channels.remove(&channel), self.identity.clone())
        else {
            return session.channel_failure(channel);
        };
        let Some((service, name)) = parse_command(data) else {
            tracing::debug!(command = %String::from_utf8_lossy(data), "SSH command refused");
            session.channel_success(channel)?;
            fail(
                pending.channel,
                "only git clone, fetch, and push are supported over SSH".into(),
            );
            return Ok(());
        };
        // The channel answers for itself from now on: Git reports success
        // or failure with the exit status.
        session.channel_success(channel)?;
        if *self.stop.borrow() {
            fail(pending.channel, "server stopping".into());
            return Ok(());
        }
        let request = uuid::Uuid::new_v4();
        let span = tracing::info_span!("request", id = %request);
        let (state, peer, running) = (self.state.clone(), self.peer, self.running.clone());
        let finished = self.finished.clone();
        running.fetch_add(1, Ordering::SeqCst);
        tokio::spawn(
            async move {
                let user = identity.name().to_string();
                let started = std::time::Instant::now();
                let status = run_service(&state, request, identity, &name, service, pending).await;
                running.fetch_sub(1, Ordering::SeqCst);
                finished.notify_one();
                tracing::info!(
                    %peer,
                    user,
                    repo = name,
                    service = service.name(),
                    status,
                    ms = started.elapsed().as_millis() as u64,
                    "ssh request"
                );
            }
            .instrument(span),
        );
        Ok(())
    }

    async fn shell_request(
        &mut self,
        channel: ChannelId,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        let Some(pending) = self.channels.remove(&channel) else {
            return session.channel_failure(channel);
        };
        session.channel_success(channel)?;
        let user = self.identity.as_ref().map_or("", |i| i.name());
        fail(
            pending.channel,
            format!("Hi {user}! You signed in, but worktree serves only Git over SSH."),
        );
        Ok(())
    }
}

/// Ends a channel with a message on stderr and exit status 1.
fn fail(channel: Channel<Msg>, message: String) {
    tokio::spawn(async move {
        let _ = channel
            .extended_data_bytes(1, format!("{message}\n").into_bytes())
            .await;
        let _ = channel.exit_status(1).await;
        let _ = channel.eof().await;
        let _ = channel.close().await;
    });
}

/// Maps `git-upload-pack '/org/app.git'` to the service and the repository
/// name. Git quotes the path in single quotes; a repository name has no
/// quotes, so any other quoting is refused. The name is only a key into the
/// repository table, never a disk path.
fn parse_command(data: &[u8]) -> Option<(Service, String)> {
    let cmd = std::str::from_utf8(data).ok()?.trim();
    let rest = cmd
        .strip_prefix("git-")
        .or_else(|| cmd.strip_prefix("git "))?;
    let (sub, arg) = rest.split_once(' ')?;
    let service = Service::parse(&format!("git-{sub}"))?;
    let arg = arg.trim();
    let path = arg
        .strip_prefix('\'')
        .and_then(|a| a.strip_suffix('\''))
        .unwrap_or(arg);
    if path.contains(['\'', '"', '\\', ' ']) {
        return None;
    }
    let path = path.trim_start_matches('/').trim_end_matches('/');
    let name = path.strip_suffix(".git").unwrap_or(path);
    worktree_core::repo::validate_repo_name(name).ok()?;
    Some((service, name.to_string()))
}

/// Runs the Git service on the channel. Answers the exit status that the
/// client got.
async fn run_service(
    s: &AppState,
    request: uuid::Uuid,
    identity: Identity,
    name: &str,
    service: Service,
    pending: PendingChannel,
) -> u32 {
    let channel = pending.channel;
    let prepared = prepare(s, request, identity, name, service, pending.protocol).await;
    let (cmd, guards, max_input) = match prepared {
        Ok(p) => p,
        Err(r) => {
            fail(channel, response_text(r).await);
            return 1;
        }
    };
    let status = match pipe(cmd, channel, max_input, s).await {
        Ok(status) => status,
        Err(e) => {
            tracing::debug!(error = %e, "SSH Git service failed");
            1
        }
    };
    drop(guards);
    status
}

/// Checks access and builds the Git command, as smart HTTP does.
async fn prepare(
    s: &AppState,
    request: uuid::Uuid,
    identity: Identity,
    name: &str,
    service: Service,
    protocol: Option<String>,
) -> Result<(tokio::process::Command, Guards, u64), Response> {
    let create = service == Service::ReceivePack;
    let auth = authorize_identity(s, identity, name, service.needs(), create).await?;
    let mut guards = Guards::default();
    let controller = s.registry.controller(auth.repo.id);
    let mut cmd = s.registry.svc.git.service_command();
    s.registry.svc.metrics.inc(match service {
        Service::UploadPack => "ssh_upload_pack_requests",
        Service::ReceivePack => "ssh_receive_pack_requests",
    });
    let max_input = match service {
        Service::UploadPack => {
            let view = controller.prepare_read().await.map_err(repo_error)?;
            cmd.arg("upload-pack").arg(&view.dir);
            guards.hold(view);
            MAX_UPLOAD_PACK_INPUT
        }
        Service::ReceivePack => {
            // SSH gives no size in advance.
            let reservation = s
                .registry
                .svc
                .disk
                .reserve(limits::DEFAULT_PUSH_RESERVATION_BYTES.saturating_mul(2))
                .map_err(|m| {
                    s.registry.svc.metrics.inc("disk_limit_rejections");
                    crate::http::plain(axum::http::StatusCode::INSUFFICIENT_STORAGE, &m)
                })?;
            guards.hold(reservation);
            let (cache, view) = controller.prepare_write().await.map_err(repo_error)?;
            let op = PendingPush {
                repo: auth.repo.id,
                user: auth.identity.name().to_string(),
                request: RequestId(request),
                access: push_access(
                    s.directory.clone(),
                    auth.identity.clone(),
                    auth.repo.name.clone(),
                ),
                cache: cache.clone(),
                default_branch_changed: default_branch_changed(
                    s.directory.clone(),
                    auth.repo.name.clone(),
                ),
                origin: Origin::Client,
                ref_guards: vec![],
            };
            let token = s.registry.register_push(op);
            cmd.arg("-c")
                .arg(format!(
                    "receive.maxInputSize={}",
                    limits::MAX_RECEIVE_BYTES
                ))
                .arg("receive-pack")
                .arg(&cache.dir)
                .env(hook::ENV_SOCKET, &s.socket_path)
                .env(hook::ENV_TOKEN, &token.token);
            guards.hold(token);
            guards.hold(cache);
            guards.hold(view);
            limits::MAX_RECEIVE_BYTES
        }
    };
    if let Some(p) = protocol {
        cmd.env("GIT_PROTOCOL", p);
    }
    guards.hold(process_permit(s).await?);
    Ok((cmd, guards, max_input))
}

/// Connects the process to the channel: channel data to stdin (at most
/// `max_input` bytes), stdout to channel data, stderr to the client's
/// stderr. Ends the channel with the exit status.
async fn pipe(
    mut cmd: tokio::process::Command,
    channel: Channel<Msg>,
    max_input: u64,
    s: &AppState,
) -> std::io::Result<u32> {
    use std::process::Stdio;
    let metrics = s.registry.svc.metrics.clone();
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn()?;
    metrics.inc("git_processes_started");
    let mut stdin = child.stdin.take().expect("piped");
    let mut stdout = child.stdout.take().expect("piped");
    let stderr = child.stderr.take().expect("piped");
    let (mut read, write) = channel.split();
    let mut out = write.make_writer();
    let mut err = write.make_writer_ext(Some(1));

    // Channel -> stdin, limited. A client that sends more loses the
    // connection to the process.
    let mut input = tokio::spawn(async move {
        let mut reader = read.make_reader().take(max_input + 1);
        let n = tokio::io::copy(&mut reader, &mut stdin).await;
        let _ = stdin.shutdown().await;
        matches!(n, Ok(n) if n > max_input)
    });
    let output = async {
        copy_flushing(&mut stdout, &mut out).await?;
        out.shutdown().await
    };
    let diagnostics = async {
        let _ = copy_flushing(&mut stderr.take(MAX_STDERR), &mut err).await;
        Ok::<_, std::io::Error>(())
    };
    let result = tokio::select! {
        r = async { tokio::try_join!(output, diagnostics) } => r.map(|_| ()),
        Ok(true) = &mut input => Err(std::io::Error::other("input over the limit")),
    };
    if result.is_err() {
        // The client went away or sent too much.
        let _ = child.start_kill();
    }
    let status = child.wait().await;
    input.abort();
    metrics.inc("git_processes_exited");
    let code = match &status {
        Ok(s) => s.code().map_or(128, |c| c as u32),
        Err(_) => 128,
    };
    if code != 0 {
        tracing::debug!(status = ?status, "git service exited with failure");
    }
    let _ = write.exit_status(code).await;
    let _ = write.eof().await;
    let _ = write.close().await;
    result.map(|_| code)
}

/// Copies and flushes after every read: Git waits for each answer of the
/// negotiation, so nothing may stay buffered.
async fn copy_flushing<R, W>(r: &mut R, w: &mut W) -> std::io::Result<u64>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut buf = vec![0u8; 32 * 1024];
    let mut total = 0;
    loop {
        let n = r.read(&mut buf).await?;
        if n == 0 {
            return Ok(total);
        }
        w.write_all(&buf[..n]).await?;
        w.flush().await?;
        total += n as u64;
    }
}

/// The message of a plain-text error response, for the client's stderr.
async fn response_text(r: Response) -> String {
    let status = r.status();
    let body = axum::body::to_bytes(r.into_body(), 4096)
        .await
        .unwrap_or_default();
    let text = String::from_utf8_lossy(&body).trim().to_string();
    if text.is_empty() {
        status.to_string()
    } else {
        text
    }
}
