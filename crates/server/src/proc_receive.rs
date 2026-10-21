//! The `proc-receive` hook program.
//!
//! Git runs this program (a symlink named `proc-receive` to the server
//! binary) after it receives and checks the objects. The hook forwards the
//! ref commands to the controller through a private Unix socket, with the
//! access token of this operation, and relays the controller's decision to
//! Git. Git never changes a ref itself (`receive.procReceiveRefs = refs/`),
//! and reports success to the client only for refs the hook reports `ok`.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;

use crate::git::hook::{ENV_SOCKET, ENV_TOKEN, HookCommand, HookRequest, HookResponse, HookResult};
use crate::git::pktline;

/// Entry point when the program runs as `proc-receive`. Returns the exit code.
pub fn run_proc_receive() -> i32 {
    match proc_receive() {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("lager: proc-receive: {e}");
            1
        }
    }
}

fn proc_receive() -> std::io::Result<()> {
    let socket = std::env::var(ENV_SOCKET).map_err(|_| other("no controller socket"))?;
    let token = std::env::var(ENV_TOKEN).map_err(|_| other("no operation token"))?;
    let stdin = std::io::stdin();
    let mut input = stdin.lock();
    let stdout = std::io::stdout();
    let mut out = stdout.lock();

    // Version negotiation.
    let hello = pktline::read_until_flush(&mut input)?;
    let version_ok = hello
        .first()
        .map(|l| l.starts_with(b"version=1"))
        .unwrap_or(false);
    if !version_ok {
        return Err(other("unsupported proc-receive version"));
    }
    pktline::write_data(&mut out, b"version=1\0")?;
    pktline::write_flush(&mut out)?;

    let mut commands = Vec::new();
    for line in pktline::read_until_flush(&mut input)? {
        let line = String::from_utf8(line).map_err(|_| other("non-UTF-8 command"))?;
        let mut parts = line.splitn(3, ' ');
        let (Some(old), Some(new), Some(name)) = (parts.next(), parts.next(), parts.next()) else {
            return Err(other("malformed command"));
        };
        commands.push(HookCommand {
            old: old.into(),
            new: new.into(),
            name: name.into(),
        });
    }

    let response = ask_controller(
        &socket,
        HookRequest {
            token,
            commands: commands.clone(),
        },
    );
    let response = match response {
        Ok(r) => r,
        Err(e) => HookResponse {
            results: commands
                .iter()
                .map(|c| HookResult {
                    name: c.name.clone(),
                    ok: false,
                    message: Some(format!("controller unavailable: {e}")),
                })
                .collect(),
            messages: vec![],
        },
    };
    // stderr reaches the client as "remote:" lines.
    for m in &response.messages {
        eprintln!("{m}");
    }
    for r in &response.results {
        let line = if r.ok {
            format!("ok {}", r.name)
        } else {
            format!(
                "ng {} {}",
                r.name,
                r.message
                    .as_deref()
                    .unwrap_or("rejected")
                    .replace('\n', " ")
            )
        };
        pktline::write_data(&mut out, line.as_bytes())?;
    }
    pktline::write_flush(&mut out)?;
    Ok(())
}

fn ask_controller(socket: &str, req: HookRequest) -> std::io::Result<HookResponse> {
    let mut s = UnixStream::connect(socket)?;
    let mut data = serde_json::to_vec(&req)?;
    data.push(b'\n');
    s.write_all(&data)?;
    s.flush()?;
    let mut line = String::new();
    BufReader::new(s).read_line(&mut line)?;
    serde_json::from_str(&line).map_err(|e| other(&e.to_string()))
}

fn other(m: &str) -> std::io::Error {
    std::io::Error::other(m.to_string())
}
