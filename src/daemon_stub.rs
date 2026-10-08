//! No background server on this platform: sessions live in the interface process and end with it.

use crate::agent::AgentInfo;
use crate::session::SpawnOpts;
use anyhow::Result;
use serde_json::Value;
use std::convert::Infallible;

#[allow(dead_code)]
pub struct Info {
    pub sid: u64,
    pub meta: Value,
}

pub struct Daemon {
    pub server_version: String,
    never: Infallible,
}

#[allow(dead_code)]
pub enum ConnectError {
    None,
    Incompatible(String),
    Other(anyhow::Error),
}

pub struct RemoteSession(Infallible);

impl Daemon {
    pub fn spawn(&self, _opts: SpawnOpts) -> Result<RemoteSession> {
        match self.never {}
    }
    pub fn attach(&self, _sid: u64, _rows: u16, _cols: u16) -> RemoteSession {
        match self.never {}
    }
    pub fn list(&self) -> Result<Vec<Info>> {
        match self.never {}
    }
    pub fn alive(&self) -> bool {
        match self.never {}
    }
}

impl RemoteSession {
    pub fn size(&self) -> (u16, u16) {
        match self.0 {}
    }
    pub fn resize(&mut self, _rows: u16, _cols: u16) {
        match self.0 {}
    }
    pub fn write(&self, _bytes: &[u8]) {
        match self.0 {}
    }
    pub fn sudo_prompt(&self) -> bool {
        match self.0 {}
    }
    pub fn exit_code(&self) -> Option<u32> {
        match self.0 {}
    }
    pub fn agent(&self) -> Option<AgentInfo> {
        match self.0 {}
    }
    pub fn take_done(&mut self) -> bool {
        match self.0 {}
    }
    pub fn with_screen<R>(&self, _f: impl FnOnce(&vt100::Screen) -> R) -> R {
        match self.0 {}
    }
    pub fn scroll(&self, _delta: i32) {
        match self.0 {}
    }
    pub fn reset_scroll(&self) {
        match self.0 {}
    }
    pub fn kill(&mut self) {
        match self.0 {}
    }
    pub fn set_meta(&self, _meta: &Value) {
        match self.0 {}
    }
}

pub fn connect_or_start() -> Result<Daemon, ConnectError> {
    Err(ConnectError::None)
}

pub fn run_server() -> Result<()> {
    anyhow::bail!("the background server is not available on this platform")
}

pub fn kill_server() -> Result<String> {
    Ok("No background server on this platform.".into())
}
