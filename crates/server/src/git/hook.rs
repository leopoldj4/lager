//! Messages between the `proc-receive` hook program and the controller.
//!
//! Git runs the hook (a symlink named `proc-receive` to the server binary)
//! after it receives and checks the objects. The hook forwards the ref
//! commands to the controller through a private Unix socket, with the access
//! token of this operation, and relays the controller's decision to Git.
//! Git never changes a ref itself (`receive.procReceiveRefs = refs/`), and
//! reports success to the client only for refs the hook reports `ok`.

use serde::{Deserialize, Serialize};

pub const ENV_SOCKET: &str = "LAGER_HOOK_SOCKET";
pub const ENV_TOKEN: &str = "LAGER_HOOK_TOKEN";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HookRequest {
    pub token: String,
    pub commands: Vec<HookCommand>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct HookCommand {
    pub old: String,
    pub new: String,
    pub name: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HookResponse {
    pub results: Vec<HookResult>,
    /// Progress and diagnostic lines shown to the client.
    #[serde(default)]
    pub messages: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HookResult {
    pub name: String,
    pub ok: bool,
    pub message: Option<String>,
}
