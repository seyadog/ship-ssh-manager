//! The background server: it owns the PTYs, so sessions survive closing the interface.
//!
//! `ship daemon` listens on a private Unix socket. The interface connects, spawns sessions in it and
//! attaches to them: on attach it gets a snapshot of the screen, then the live output. Closing the
//! interface only disconnects; the next one finds every session where it was left.
//!
//! The protocol is one JSON object per line. Terminal bytes travel as base64.

use crate::agent::AgentInfo;
use crate::session::{Autofill, PtySession, SCROLLBACK, SpawnOpts};
use anyhow::{Context, Result, anyhow, bail};
use base64::{Engine, engine::general_purpose::STANDARD as B64};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::Shutdown;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Sender, SyncSender, channel, sync_channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Bumped when the messages change in a way old and new sides cannot understand.
pub const PROTO: u32 = 1;
/// The server exits when it has no sessions and nobody connected for this long.
const IDLE_EXIT: Duration = Duration::from_secs(10);
/// Events queued per connection. A client that falls this far behind is disconnected.
const QUEUE: usize = 8192;

// ---------------------------------------------------------------- protocol

#[derive(Serialize, Deserialize)]
struct Fill {
    when: String,
    secret: String,
    fallback: bool,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
enum Req {
    Hello { proto: u32 },
    Spawn {
        req: u64,
        argv: Vec<String>,
        rows: u16,
        cols: u16,
        cwd: Option<String>,
        env: Vec<(String, String)>,
        fills: Vec<Fill>,
        login_expected: bool,
    },
    Attach { sid: u64, rows: u16, cols: u16 },
    Input { sid: u64, data: String },
    Resize { sid: u64, rows: u16, cols: u16 },
    Kill { sid: u64 },
    SetMeta { sid: u64, meta: Value },
    List { req: u64 },
    Shutdown,
}

#[derive(Serialize, Deserialize, Clone)]
struct AgentMsg {
    name: String,
    working: bool,
}

impl From<AgentInfo> for AgentMsg {
    fn from(a: AgentInfo) -> Self {
        AgentMsg { name: a.name, working: a.working }
    }
}

impl From<AgentMsg> for AgentInfo {
    fn from(a: AgentMsg) -> Self {
        AgentInfo { name: a.name, working: a.working }
    }
}

/// What the client needs to rebuild a tab for a session that is already running.
#[derive(Serialize, Deserialize, Clone)]
pub struct Info {
    pub sid: u64,
    pub meta: Value,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(tag = "t", rename_all = "snake_case")]
enum Ev {
    Hello { proto: u32, version: String },
    Spawned { req: u64, sid: u64 },
    Failed { req: u64, error: String },
    Sessions { req: u64, sessions: Vec<Info> },
    /// The screen as it is now; live output follows.
    Snapshot {
        sid: u64,
        rows: u16,
        cols: u16,
        data: String,
        exit: Option<u32>,
        sudo: bool,
        agent: Option<AgentMsg>,
        /// An agent finished while nobody was attached.
        done: bool,
        /// The raw output so far, replayed before `data` to rebuild the scroll-back.
        #[serde(default)]
        replay: String,
    },
    Output { sid: u64, data: String },
    Exit { sid: u64, code: u32 },
    Sudo { sid: u64, on: bool },
    Agent { sid: u64, agent: Option<AgentMsg>, done: bool },
}

fn write_line<T: Serialize>(w: &mut impl Write, msg: &T) -> std::io::Result<()> {
    let mut line = serde_json::to_vec(msg).map_err(std::io::Error::other)?;
    line.push(b'\n');
    w.write_all(&line)?;
    w.flush()
}

// ---------------------------------------------------------------- paths

/// Where the server listens: inside the config dir if `SHIP_CONFIG_DIR` is set (separate profiles and
/// tests do not share a server), otherwise in the user's private runtime directory.
pub fn socket_path() -> PathBuf {
    if let Some(d) = std::env::var_os("SHIP_CONFIG_DIR") {
        return PathBuf::from(d).join("daemon.sock");
    }
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|p| p.is_dir())
        .unwrap_or_else(std::env::temp_dir);
    // SAFETY: getuid has no preconditions and cannot fail.
    let uid = unsafe { libc::getuid() };
    base.join(format!("ship-{uid}")).join("daemon.sock")
}

fn prepare_dir(path: &Path) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
    }
    Ok(())
}

// ---------------------------------------------------------------- server

/// A connection receiving a session's events.
struct Sub {
    client: u64,
    tx: SyncSender<Ev>,
    /// Closed if the client cannot keep up, so it notices and reconnects instead of missing output.
    stream: Arc<UnixStream>,
}

type Subs = Arc<Mutex<Vec<Sub>>>;

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn broadcast(subs: &Subs, ev: Ev) {
    let mut subs = lock(subs);
    subs.retain(|s| match s.tx.try_send(ev.clone()) {
        Ok(()) => true,
        Err(_) => {
            let _ = s.stream.shutdown(Shutdown::Both);
            false
        }
    });
}

/// What was last announced to the clients, to send only changes.
#[derive(Default)]
struct Sent {
    exit: bool,
    sudo: bool,
    agent: Option<AgentInfo>,
}

struct Hosted {
    pty: Arc<Mutex<PtySession>>,
    meta: Value,
    subs: Subs,
    sent: Sent,
    /// An agent finished while nobody was attached: reported on the next attach.
    pending_done: bool,
}

#[derive(Default)]
struct State {
    next_sid: u64,
    next_client: u64,
    clients: usize,
    sessions: HashMap<u64, Hosted>,
}

struct Server {
    state: Mutex<State>,
    sound: bool,
    /// Leave once there has been nothing to keep for a while (always, except in tests).
    idle_exit: bool,
    socket: PathBuf,
}

/// Runs the server until it is told to stop or has been idle for a while.
pub fn run_server() -> Result<()> {
    let path = socket_path();
    serve(path, crate::settings::Settings::load().sound, true)
}

fn serve(path: PathBuf, sound: bool, idle_exit: bool) -> Result<()> {
    prepare_dir(&path)?;
    if path.exists() {
        if UnixStream::connect(&path).is_ok() {
            return Ok(()); // another server is already running
        }
        let _ = std::fs::remove_file(&path); // stale socket of a dead server
    }
    let listener = UnixListener::bind(&path).with_context(|| format!("could not listen on {}", path.display()))?;
    let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    let server = Arc::new(Server { state: Mutex::new(State::default()), sound, idle_exit, socket: path });

    {
        let server = Arc::clone(&server);
        std::thread::spawn(move || tick_loop(&server));
    }
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let server = Arc::clone(&server);
        std::thread::spawn(move || {
            let _ = serve_client(&server, stream);
        });
    }
    Ok(())
}

fn exit_server(server: &Server) -> ! {
    let _ = std::fs::remove_file(&server.socket);
    std::process::exit(0)
}

/// Twice a second: announce exits, agent changes and sudo prompts; leave when there is nothing to keep.
fn tick_loop(server: &Server) {
    let mut idle_since: Option<Instant> = None;
    loop {
        std::thread::sleep(Duration::from_millis(250));
        let mut st = lock(&server.state);
        for (&sid, h) in st.sessions.iter_mut() {
            let (exit, agent, done, sudo) = {
                let mut p = lock(&h.pty);
                p.poll_exit();
                p.poll_agent();
                (p.exit_code, p.agent().cloned(), p.take_done(), p.sudo_prompt())
            };
            let attached = !lock(&h.subs).is_empty();
            if done {
                if attached {
                    // reported below with the agent change
                } else {
                    h.pending_done = true;
                    if server.sound {
                        crate::notify::ring();
                    }
                }
            }
            if let Some(code) = exit {
                if !h.sent.exit {
                    h.sent.exit = true;
                    broadcast(&h.subs, Ev::Exit { sid, code });
                }
            }
            if agent != h.sent.agent || (done && attached) {
                h.sent.agent = agent.clone();
                broadcast(&h.subs, Ev::Agent { sid, agent: agent.map(Into::into), done: done && attached });
            }
            if sudo != h.sent.sudo {
                h.sent.sudo = sudo;
                broadcast(&h.subs, Ev::Sudo { sid, on: sudo });
            }
        }
        if server.idle_exit && st.sessions.is_empty() && st.clients == 0 {
            let since = *idle_since.get_or_insert_with(Instant::now);
            if since.elapsed() > IDLE_EXIT {
                drop(st);
                exit_server(server);
            }
        } else {
            idle_since = None;
        }
    }
}

fn serve_client(server: &Arc<Server>, stream: UnixStream) -> Result<()> {
    let cid = {
        let mut st = lock(&server.state);
        st.next_client += 1;
        st.clients += 1;
        st.next_client
    };
    let result = client_loop(server, stream, cid);
    let mut st = lock(&server.state);
    st.clients -= 1;
    for h in st.sessions.values() {
        lock(&h.subs).retain(|s| s.client != cid);
    }
    result
}

fn client_loop(server: &Arc<Server>, stream: UnixStream, cid: u64) -> Result<()> {
    let ctl = Arc::new(stream.try_clone()?);
    let (tx, rx) = sync_channel::<Ev>(QUEUE);
    {
        let mut w = stream.try_clone()?;
        std::thread::spawn(move || {
            for ev in rx {
                if write_line(&mut w, &ev).is_err() {
                    break;
                }
            }
            let _ = w.shutdown(Shutdown::Both);
        });
    }
    for line in BufReader::new(stream).lines() {
        let Ok(req) = serde_json::from_str::<Req>(&line?) else { continue };
        handle(server, req, cid, &tx, &ctl);
    }
    Ok(())
}

fn handle(server: &Arc<Server>, req: Req, cid: u64, tx: &SyncSender<Ev>, ctl: &Arc<UnixStream>) {
    let reply = |ev: Ev| {
        let _ = tx.try_send(ev);
    };
    match req {
        Req::Hello { .. } => reply(Ev::Hello { proto: PROTO, version: env!("CARGO_PKG_VERSION").into() }),
        Req::Spawn { req, argv, rows, cols, cwd, env, fills, login_expected } => {
            if argv.is_empty() {
                return reply(Ev::Failed { req, error: "empty command".into() });
            }
            let sid = {
                let mut st = lock(&server.state);
                st.next_sid += 1;
                st.next_sid
            };
            let subs: Subs = Arc::new(Mutex::new(vec![]));
            let tap = {
                let subs = Arc::clone(&subs);
                Arc::new(move |bytes: &[u8]| {
                    if !lock(&subs).is_empty() {
                        broadcast(&subs, Ev::Output { sid, data: B64.encode(bytes) });
                    }
                })
            };
            let opts = SpawnOpts {
                argv,
                rows,
                cols,
                fills: fills.into_iter().map(|f| Autofill { when: f.when, secret: f.secret, fallback: f.fallback }).collect(),
                login_expected,
                cwd: cwd.map(PathBuf::from),
                env: Some(env),
                tap: Some(tap),
            };
            match PtySession::spawn_with(opts) {
                Ok(pty) => {
                    let hosted = Hosted {
                        pty: Arc::new(Mutex::new(pty)),
                        meta: Value::Null,
                        subs,
                        sent: Sent::default(),
                        pending_done: false,
                    };
                    lock(&server.state).sessions.insert(sid, hosted);
                    reply(Ev::Spawned { req, sid });
                }
                Err(e) => reply(Ev::Failed { req, error: format!("{e:#}") }),
            }
        }
        Req::Attach { sid, rows, cols } => {
            let (pty, subs, done) = {
                let mut st = lock(&server.state);
                let Some(h) = st.sessions.get_mut(&sid) else { return };
                (Arc::clone(&h.pty), Arc::clone(&h.subs), std::mem::take(&mut h.pending_done))
            };
            let mut p = lock(&pty);
            p.resize(rows, cols);
            let (exit, sudo, agent) = (p.exit_code, p.sudo_prompt(), p.agent().cloned());
            // Snapshot and subscription happen under the screen lock: the output that follows the snapshot
            // is exactly the output that came after it.
            p.with_screen(|screen| {
                let snapshot = Ev::Snapshot {
                    sid,
                    rows,
                    cols,
                    data: B64.encode(screen.state_formatted()),
                    exit,
                    sudo,
                    agent: agent.map(Into::into),
                    done,
                    replay: B64.encode(p.history()),
                };
                let _ = tx.try_send(snapshot);
                lock(&subs).push(Sub { client: cid, tx: tx.clone(), stream: Arc::clone(ctl) });
            });
        }
        Req::Input { sid, data } => {
            let Ok(bytes) = B64.decode(data) else { return };
            let pty = lock(&server.state).sessions.get(&sid).map(|h| Arc::clone(&h.pty));
            if let Some(p) = pty {
                lock(&p).write(&bytes);
            }
        }
        Req::Resize { sid, rows, cols } => {
            let pty = lock(&server.state).sessions.get(&sid).map(|h| Arc::clone(&h.pty));
            if let Some(p) = pty {
                lock(&p).resize(rows, cols);
            }
        }
        Req::Kill { sid } => {
            let hosted = lock(&server.state).sessions.remove(&sid);
            if let Some(h) = hosted {
                lock(&h.pty).kill();
            }
        }
        Req::SetMeta { sid, meta } => {
            if let Some(h) = lock(&server.state).sessions.get_mut(&sid) {
                h.meta = meta;
            }
        }
        Req::List { req } => {
            let sessions = {
                let st = lock(&server.state);
                let mut v: Vec<Info> = st.sessions.iter().map(|(&sid, h)| Info { sid, meta: h.meta.clone() }).collect();
                v.sort_by_key(|i| i.sid);
                v
            };
            reply(Ev::Sessions { req, sessions });
        }
        Req::Shutdown => {
            let hosted: Vec<Hosted> = lock(&server.state).sessions.drain().map(|(_, h)| h).collect();
            for h in hosted {
                lock(&h.pty).kill();
            }
            exit_server(server);
        }
    }
}

// ---------------------------------------------------------------- client

/// What the interface knows about a session in the server: its screen and flags, kept up to date by events.
pub struct Remote {
    parser: Mutex<vt100::Parser>,
    exit: Mutex<Option<u32>>,
    sudo: AtomicBool,
    agent: Mutex<Option<AgentInfo>>,
    done: AtomicBool,
}

struct ClientShared {
    writer: Mutex<UnixStream>,
    sessions: Mutex<HashMap<u64, Arc<Remote>>>,
    waiting: Mutex<HashMap<u64, Sender<Ev>>>,
    next_req: AtomicU64,
    alive: AtomicBool,
}

impl ClientShared {
    fn send(&self, req: &Req) {
        if write_line(&mut *lock(&self.writer), req).is_err() {
            self.alive.store(false, Ordering::Relaxed);
        }
    }

    /// Sends a request that is answered with an event carrying the same `req` id.
    fn ask(&self, make: impl FnOnce(u64) -> Req) -> Result<Ev> {
        let id = self.next_req.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = channel();
        lock(&self.waiting).insert(id, tx);
        self.send(&make(id));
        let ev = rx.recv_timeout(Duration::from_secs(10));
        lock(&self.waiting).remove(&id);
        ev.map_err(|_| anyhow!("the background server did not answer"))
    }

    fn dispatch(&self, ev: Ev) {
        let waiter = |req: u64| lock(&self.waiting).get(&req).cloned();
        match ev {
            Ev::Spawned { req, .. } | Ev::Failed { req, .. } | Ev::Sessions { req, .. } => {
                if let Some(w) = waiter(req) {
                    let _ = w.send(ev);
                }
            }
            Ev::Snapshot { sid, rows, cols, data, exit, sudo, agent, done, replay } => {
                if let Some(r) = lock(&self.sessions).get(&sid) {
                    let mut parser = vt100::Parser::new(rows, cols, SCROLLBACK);
                    // Replaying the output rebuilds the scroll-back; the snapshot then sets the exact screen and modes.
                    parser.process(&B64.decode(replay).unwrap_or_default());
                    parser.process(&B64.decode(data).unwrap_or_default());
                    *lock(&r.parser) = parser;
                    *lock(&r.exit) = exit;
                    r.sudo.store(sudo, Ordering::Relaxed);
                    *lock(&r.agent) = agent.map(Into::into);
                    if done {
                        r.done.store(true, Ordering::Relaxed);
                    }
                }
            }
            Ev::Output { sid, data } => {
                if let Some(r) = lock(&self.sessions).get(&sid) {
                    lock(&r.parser).process(&B64.decode(data).unwrap_or_default());
                }
            }
            Ev::Exit { sid, code } => {
                if let Some(r) = lock(&self.sessions).get(&sid) {
                    *lock(&r.exit) = Some(code);
                }
            }
            Ev::Sudo { sid, on } => {
                if let Some(r) = lock(&self.sessions).get(&sid) {
                    r.sudo.store(on, Ordering::Relaxed);
                }
            }
            Ev::Agent { sid, agent, done } => {
                if let Some(r) = lock(&self.sessions).get(&sid) {
                    *lock(&r.agent) = agent.map(Into::into);
                    if done {
                        r.done.store(true, Ordering::Relaxed);
                    }
                }
            }
            Ev::Hello { .. } => {}
        }
    }

    /// The server went away: every session it held is gone with it.
    fn lost(&self) {
        self.alive.store(false, Ordering::Relaxed);
        for r in lock(&self.sessions).values() {
            lock(&r.exit).get_or_insert(255);
        }
    }
}

/// Connection to the background server.
pub struct Daemon {
    shared: Arc<ClientShared>,
    /// Version of the server we talk to.
    pub server_version: String,
}

pub enum ConnectError {
    /// No server is listening.
    None,
    /// A server of an incompatible protocol is running (its version).
    Incompatible(String),
    Other(anyhow::Error),
}

impl Daemon {
    pub fn connect_at(path: &Path) -> std::result::Result<Daemon, ConnectError> {
        let stream = UnixStream::connect(path).map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused => ConnectError::None,
            _ => ConnectError::Other(e.into()),
        })?;
        Self::handshake(stream)
    }

    fn handshake(stream: UnixStream) -> std::result::Result<Daemon, ConnectError> {
        let other = |e: anyhow::Error| ConnectError::Other(e);
        stream.set_read_timeout(Some(Duration::from_secs(3))).map_err(|e| other(e.into()))?;
        let mut writer = stream.try_clone().map_err(|e| other(e.into()))?;
        write_line(&mut writer, &Req::Hello { proto: PROTO }).map_err(|e| other(e.into()))?;
        let mut reader = BufReader::new(stream.try_clone().map_err(|e| other(e.into()))?);
        let mut line = String::new();
        reader.read_line(&mut line).map_err(|e| other(anyhow!("no answer from the background server: {e}")))?;
        let Ok(Ev::Hello { proto, version }) = serde_json::from_str(&line) else {
            return Err(other(anyhow!("unexpected answer from the background server")));
        };
        if proto != PROTO {
            return Err(ConnectError::Incompatible(version));
        }
        stream.set_read_timeout(None).map_err(|e| other(e.into()))?;
        let shared = Arc::new(ClientShared {
            writer: Mutex::new(writer),
            sessions: Mutex::new(HashMap::new()),
            waiting: Mutex::new(HashMap::new()),
            next_req: AtomicU64::new(1),
            alive: AtomicBool::new(true),
        });
        let inbox = Arc::clone(&shared);
        std::thread::spawn(move || {
            for line in reader.lines() {
                let Ok(line) = line else { break };
                if let Ok(ev) = serde_json::from_str::<Ev>(&line) {
                    inbox.dispatch(ev);
                }
            }
            inbox.lost();
        });
        Ok(Daemon { shared, server_version: version })
    }

    /// Starts a session in the server and attaches to it.
    pub fn spawn(&self, mut opts: SpawnOpts) -> Result<RemoteSession> {
        let (rows, cols) = (opts.rows.max(1), opts.cols.max(1));
        let env = opts.env.take().unwrap_or_else(|| std::env::vars().collect());
        let mut fills: Vec<Fill> = std::mem::take(&mut opts.fills)
            .into_iter()
            .map(|f| Fill { when: f.when, secret: f.secret, fallback: f.fallback })
            .collect();
        let argv = opts.argv.clone();
        let cwd = opts.cwd.as_ref().map(|p| p.to_string_lossy().into_owned());
        let login_expected = opts.login_expected;
        let ev = self.shared.ask(|req| Req::Spawn {
            req,
            argv,
            rows,
            cols,
            cwd,
            env,
            fills: std::mem::take(&mut fills),
            login_expected,
        })?;
        match ev {
            Ev::Spawned { sid, .. } => Ok(self.attach(sid, rows, cols)),
            Ev::Failed { error, .. } => bail!("{error}"),
            _ => bail!("unexpected answer from the background server"),
        }
    }

    /// Attaches to a running session: its screen arrives as a snapshot, then live output.
    pub fn attach(&self, sid: u64, rows: u16, cols: u16) -> RemoteSession {
        let (rows, cols) = (rows.max(1), cols.max(1));
        let remote = Arc::new(Remote {
            parser: Mutex::new(vt100::Parser::new(rows, cols, SCROLLBACK)),
            exit: Mutex::new(None),
            sudo: AtomicBool::new(false),
            agent: Mutex::new(None),
            done: AtomicBool::new(false),
        });
        lock(&self.shared.sessions).insert(sid, Arc::clone(&remote));
        self.shared.send(&Req::Attach { sid, rows, cols });
        RemoteSession { sid, remote, client: Arc::clone(&self.shared), size: (rows, cols) }
    }

    /// The sessions the server is holding.
    pub fn list(&self) -> Result<Vec<Info>> {
        match self.shared.ask(|req| Req::List { req })? {
            Ev::Sessions { sessions, .. } => Ok(sessions),
            _ => bail!("unexpected answer from the background server"),
        }
    }

    pub fn alive(&self) -> bool {
        self.shared.alive.load(Ordering::Relaxed)
    }
}

/// A session living in the server, seen from the interface.
pub struct RemoteSession {
    sid: u64,
    remote: Arc<Remote>,
    client: Arc<ClientShared>,
    size: (u16, u16),
}

impl RemoteSession {
    pub fn size(&self) -> (u16, u16) {
        self.size
    }

    pub fn resize(&mut self, rows: u16, cols: u16) {
        let (rows, cols) = (rows.max(1), cols.max(1));
        if (rows, cols) == self.size {
            return;
        }
        self.size = (rows, cols);
        lock(&self.remote.parser).screen_mut().set_size(rows, cols);
        self.client.send(&Req::Resize { sid: self.sid, rows, cols });
    }

    pub fn write(&self, bytes: &[u8]) {
        self.remote.sudo.store(false, Ordering::Relaxed);
        self.client.send(&Req::Input { sid: self.sid, data: B64.encode(bytes) });
    }

    pub fn sudo_prompt(&self) -> bool {
        self.remote.sudo.load(Ordering::Relaxed)
    }

    pub fn exit_code(&self) -> Option<u32> {
        *lock(&self.remote.exit)
    }

    pub fn agent(&self) -> Option<AgentInfo> {
        lock(&self.remote.agent).clone()
    }

    pub fn take_done(&mut self) -> bool {
        self.remote.done.swap(false, Ordering::Relaxed)
    }

    pub fn with_screen<R>(&self, f: impl FnOnce(&vt100::Screen) -> R) -> R {
        f(lock(&self.remote.parser).screen())
    }

    pub fn scroll(&self, delta: i32) {
        let mut p = lock(&self.remote.parser);
        let cur = p.screen().scrollback() as i32;
        p.screen_mut().set_scrollback((cur + delta).max(0) as usize);
    }

    pub fn reset_scroll(&self) {
        lock(&self.remote.parser).screen_mut().set_scrollback(0);
    }

    /// Ends the session in the server. Dropping a `RemoteSession` only detaches.
    pub fn kill(&mut self) {
        lock(&self.client.sessions).remove(&self.sid);
        self.client.send(&Req::Kill { sid: self.sid });
    }

    pub fn set_meta(&self, meta: &Value) {
        self.client.send(&Req::SetMeta { sid: self.sid, meta: meta.clone() });
    }
}

// ---------------------------------------------------------------- finding or starting the server

/// Connects to the server, starting it first if it is not running.
pub fn connect_or_start() -> std::result::Result<Daemon, ConnectError> {
    let path = socket_path();
    match Daemon::connect_at(&path) {
        Err(ConnectError::None) => {}
        other => return other,
    }
    start_server().map_err(ConnectError::Other)?;
    let end = Instant::now() + Duration::from_secs(4);
    loop {
        match Daemon::connect_at(&path) {
            Err(ConnectError::None) if Instant::now() < end => std::thread::sleep(Duration::from_millis(50)),
            other => return other,
        }
    }
}

/// Launches `ship daemon` detached from this terminal, so it outlives it.
fn start_server() -> Result<()> {
    let exe = std::env::current_exe()?;
    let mut cmd = Command::new(exe);
    cmd.arg("daemon").stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    // SAFETY: setsid only detaches the child from the controlling terminal; it is async-signal-safe.
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    cmd.spawn().context("could not start the background server")?;
    Ok(())
}

/// `ship kill-server`: stops the server and every session in it.
pub fn kill_server() -> Result<String> {
    match Daemon::connect_at(&socket_path()) {
        Ok(d) => {
            let n = d.list().map(|l| l.len()).unwrap_or(0);
            d.shared.send(&Req::Shutdown);
            Ok(format!("Stopped the background server ({n} session(s) closed)."))
        }
        Err(ConnectError::None) => Ok("No background server is running.".into()),
        Err(ConnectError::Incompatible(v)) => bail!("The running server is version {v}, which this one cannot talk to."),
        Err(ConnectError::Other(e)) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::Session;

    fn start(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ship-daemon-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("d.sock");
        let p = path.clone();
        std::thread::spawn(move || {
            let _ = serve(p, false, false);
        });
        let end = Instant::now() + Duration::from_secs(3);
        while Daemon::connect_at(&path).is_err() && Instant::now() < end {
            std::thread::sleep(Duration::from_millis(20));
        }
        path
    }

    fn sh(script: &str) -> SpawnOpts {
        SpawnOpts {
            argv: vec!["sh".into(), "-c".into(), script.into()],
            rows: 24,
            cols: 80,
            fills: vec![],
            login_expected: false,
            cwd: None,
            env: None,
            tap: None,
        }
    }

    fn wait_for(s: &mut Session, want: &str) -> bool {
        let end = Instant::now() + Duration::from_secs(5);
        while Instant::now() < end {
            s.poll_exit();
            if s.with_screen(|sc| sc.contents()).contains(want) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        false
    }

    #[test]
    fn a_session_runs_in_the_server_and_takes_input() {
        let path = start("run");
        let d = Daemon::connect_at(&path).ok().unwrap();
        let mut s = Session::spawn_with(sh("echo hello; read x; echo got:$x"), Some(&d)).unwrap();
        assert!(wait_for(&mut s, "hello"));
        s.write(b"abc\r");
        assert!(wait_for(&mut s, "got:abc"));
    }

    /// The point of the whole thing: a new connection finds the session, with its screen, still running.
    #[test]
    fn sessions_survive_the_interface_disconnecting() {
        let path = start("survive");
        {
            let d = Daemon::connect_at(&path).ok().unwrap();
            let mut s = Session::spawn_with(sh("echo before-the-detach; read x; echo after:$x"), Some(&d)).unwrap();
            s.set_meta(&serde_json::json!({"title": "work", "space": 7, "order": 1}));
            assert!(wait_for(&mut s, "before-the-detach"));
            std::thread::sleep(Duration::from_millis(200)); // let the meta arrive
        } // the interface goes away: connection dropped, no Kill

        let d2 = Daemon::connect_at(&path).ok().unwrap();
        let list = d2.list().unwrap();
        assert_eq!(list.len(), 1, "the session is still there");
        assert_eq!(list[0].meta["title"], "work");
        assert_eq!(list[0].meta["space"], 7);
        let mut s = Session::from_remote(d2.attach(list[0].sid, 24, 80));
        assert!(wait_for(&mut s, "before-the-detach"), "the screen comes back");
        s.write(b"again\r");
        assert!(wait_for(&mut s, "after:again"), "and the process was never interrupted");
    }

    /// What scrolled off the screen before the interface came back can still be scrolled up to.
    #[test]
    fn scroll_back_survives_reattaching() {
        let path = start("history");
        let sid = {
            let d = Daemon::connect_at(&path).ok().unwrap();
            let mut s = Session::spawn_with(sh("for i in $(seq 1 120); do echo history-line-$i; done; read x"), Some(&d)).unwrap();
            assert!(wait_for(&mut s, "history-line-120"));
            d.list().unwrap()[0].sid
        };
        let d2 = Daemon::connect_at(&path).ok().unwrap();
        let mut s = Session::from_remote(d2.attach(sid, 24, 80));
        assert!(wait_for(&mut s, "history-line-120"));
        s.scroll(100_000); // as far back as it goes
        let top = s.with_screen(|sc| sc.contents());
        assert!(top.lines().any(|l| l == "history-line-1"), "the oldest line is reachable: {top:?}");
    }

    #[test]
    fn exit_codes_and_kill() {
        let path = start("exit");
        let d = Daemon::connect_at(&path).ok().unwrap();
        let mut s = Session::spawn_with(sh("exit 3"), Some(&d)).unwrap();
        let end = Instant::now() + Duration::from_secs(5);
        while s.exit_code.is_none() && Instant::now() < end {
            s.poll_exit();
            std::thread::sleep(Duration::from_millis(30));
        }
        assert_eq!(s.exit_code, Some(3));
        s.kill();
        std::thread::sleep(Duration::from_millis(200));
        assert!(d.list().unwrap().is_empty(), "a killed session is forgotten");
    }

    #[test]
    fn the_new_client_sees_a_running_agent_and_the_environment_is_the_clients() {
        let path = start("env");
        let d = Daemon::connect_at(&path).ok().unwrap();
        let mut opts = sh("echo \"v=$SHIP_TEST_VAR\"; sleep 1");
        opts.env = Some(vec![("SHIP_TEST_VAR".into(), "from-the-client".into()), ("PATH".into(), "/usr/bin:/bin".into())]);
        let mut s = Session::spawn_with(opts, Some(&d)).unwrap();
        assert!(wait_for(&mut s, "v=from-the-client"), "the session gets the client's environment, not the server's");
    }

    #[test]
    fn a_slow_or_gone_client_does_not_disturb_the_session() {
        let path = start("gone");
        let d1 = Daemon::connect_at(&path).ok().unwrap();
        let s1 = Session::spawn_with(sh("for i in 1 2 3 4 5; do echo line$i; sleep 0.2; done; read x; echo done:$x"), Some(&d1)).unwrap();
        drop(s1);
        drop(d1);
        let d2 = Daemon::connect_at(&path).ok().unwrap();
        let sid = d2.list().unwrap()[0].sid;
        let mut s = Session::from_remote(d2.attach(sid, 24, 80));
        assert!(wait_for(&mut s, "line5"));
        s.write(b"ok\r");
        assert!(wait_for(&mut s, "done:ok"));
    }
}
