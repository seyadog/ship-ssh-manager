//! Application state and keyboard/mouse handling. Drawing lives in `ui.rs`.

use crate::keys;
use crate::clipboard;
use crate::daemon::Daemon;
use crate::session::{Autofill, Session, SpawnOpts};
use crate::spaces::{self, Spaces};
use crate::store::{Auth, NodeId, Server, Store};
use crate::vault::{Kind, UnlockError, Vault};
use base64::{Engine, engine::general_purpose::STANDARD as B64};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use zeroize::Zeroize;

const VAULT_IDLE: Duration = Duration::from_secs(5 * 60);
const REVEAL_FOR: Duration = Duration::from_secs(8);
const CLIPBOARD_FOR: Duration = Duration::from_secs(30);
const MIN_MASTER: usize = 8;

/// Copies to the system clipboard through the terminal (OSC 52). An empty string clears it.
fn osc52(text: &str) {
    let mut out = std::io::stdout();
    let _ = write!(out, "\x1b]52;c;{}\x07", B64.encode(text));
    let _ = out.flush();
}

impl VaultView {
    /// Rebuilds the list, keeping `select` (a server id) and `col` (0 = login, 1 = sudo).
    fn new(store: &Store, select: Option<u64>, col: usize) -> Self {
        let rows = store
            .servers
            .iter()
            .filter(|s| s.has_secret || s.has_sudo)
            .map(|s| VaultRow {
                id: s.id,
                name: s.name.clone(),
                host: if s.user.is_empty() { s.host.clone() } else { format!("{}@{}", s.user, s.host) },
                login: s.has_secret,
                sudo: s.has_sudo,
            })
            .collect();
        let rows: Vec<VaultRow> = rows;
        let selected = select.and_then(|id| rows.iter().position(|r| r.id == id)).unwrap_or(0);
        VaultView { rows, selected, col, shown: None }
    }
}

// ---------------------------------------------------------------- text input

#[derive(Default, Clone)]
pub struct Input {
    pub value: String,
    /// Cursor position, in characters.
    pub cursor: usize,
}

impl Drop for Input {
    fn drop(&mut self) {
        self.value.zeroize();
    }
}

impl Input {
    pub fn new(s: &str) -> Self {
        Input { value: s.to_string(), cursor: s.chars().count() }
    }

    fn byte_idx(&self) -> usize {
        self.value.char_indices().nth(self.cursor).map(|(i, _)| i).unwrap_or(self.value.len())
    }

    pub fn insert_str(&mut self, s: &str) {
        for c in s.chars().filter(|c| !c.is_control()) {
            let i = self.byte_idx();
            self.value.insert(i, c);
            self.cursor += 1;
        }
    }

    pub fn handle(&mut self, key: KeyEvent) {
        let plain = !key.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Char('u') if ctrl => {
                let at = self.byte_idx();
                self.value.drain(..at);
                self.cursor = 0;
            }
            KeyCode::Char('w') if ctrl => {
                let chars: Vec<char> = self.value.chars().collect();
                let mut i = self.cursor;
                while i > 0 && chars[i - 1].is_whitespace() {
                    i -= 1;
                }
                while i > 0 && !chars[i - 1].is_whitespace() {
                    i -= 1;
                }
                let kept: String = chars[..i].iter().chain(chars[self.cursor..].iter()).collect();
                self.value = kept;
                self.cursor = i;
            }
            KeyCode::Char('a') if ctrl => self.cursor = 0,
            KeyCode::Char('e') if ctrl => self.cursor = self.value.chars().count(),
            KeyCode::Char(c) if plain => self.insert_str(&c.to_string()),
            KeyCode::Backspace if self.cursor > 0 => {
                self.cursor -= 1;
                let i = self.byte_idx();
                self.value.remove(i);
            }
            KeyCode::Delete if self.cursor < self.value.chars().count() => {
                let i = self.byte_idx();
                self.value.remove(i);
            }
            KeyCode::Left => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Right => self.cursor = (self.cursor + 1).min(self.value.chars().count()),
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = self.value.chars().count(),
            _ => {}
        }
    }
}

// ---------------------------------------------------------------- tree

#[derive(Clone, Copy)]
pub struct Row {
    pub node: NodeId,
    pub depth: usize,
}

/// Text selected with the mouse in the terminal area, as (row, column) cells of that area.
#[derive(Clone, Copy)]
pub struct Selection {
    pub anchor: (u16, u16),
    pub head: (u16, u16),
}

impl Selection {
    /// The two ends, first one first.
    pub fn ordered(&self) -> ((u16, u16), (u16, u16)) {
        if self.anchor <= self.head { (self.anchor, self.head) } else { (self.head, self.anchor) }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Focus {
    Sidebar,
    Terminal,
}

/// Where a tab lives: the SSH section, or one space. Each has its own tab bar, so what runs in a space
/// (an AI agent, say) never shows up among the SSH tabs. `Space(0)` stands for "no space selected".
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Scope {
    Ssh,
    Space(u64),
}

pub struct Tab {
    pub id: u64,
    pub title: String,
    pub server_id: u64,
    pub scope: Scope,
    pub session: Session,
    /// An agent finished here and the user has not looked yet.
    pub attention: bool,
    /// The last description of this tab sent to the background server.
    meta_sent: String,
}

// ---------------------------------------------------------------- modales

pub const F_NAME: usize = 0;
pub const F_HOST: usize = 1;
pub const F_PORT: usize = 2;
pub const F_USER: usize = 3;
pub const F_AUTH: usize = 4;
pub const F_KEY: usize = 5;
pub const F_SECRET: usize = 6;
pub const F_JUMP: usize = 7;
pub const F_SUDO: usize = 8;

pub struct ServerForm {
    pub editing: Option<u64>,
    pub parent: Option<u64>,
    pub name: Input,
    pub host: Input,
    pub port: Input,
    pub user: Input,
    pub key: Input,
    pub secret: Input,
    pub sudo: Input,
    pub auth: Auth,
    pub jump: Option<u64>,
    pub focus: usize,
    pub error: Option<String>,
    pub secret_saved: bool,
    pub sudo_saved: bool,
    /// Ctrl+X on a saved secret: remove it from the vault when the form is saved.
    pub clear_secret: bool,
    pub clear_sudo: bool,
}

impl ServerForm {
    fn new(parent: Option<u64>, jump: Option<u64>) -> Self {
        ServerForm {
            editing: None,
            parent,
            jump,
            name: Input::default(),
            host: Input::default(),
            port: Input::new("22"),
            user: Input::default(),
            key: Input::default(),
            secret: Input::default(),
            sudo: Input::default(),
            auth: Auth::Agent,
            focus: F_NAME,
            error: None,
            secret_saved: false,
            sudo_saved: false,
            clear_secret: false,
            clear_sudo: false,
        }
    }

    fn from_server(s: &Server) -> Self {
        ServerForm {
            editing: Some(s.id),
            parent: s.parent,
            jump: s.jump,
            name: Input::new(&s.name),
            host: Input::new(&s.host),
            port: Input::new(&s.port.to_string()),
            user: Input::new(&s.user),
            key: Input::new(&s.key_path),
            secret: Input::default(),
            sudo: Input::default(),
            auth: s.auth,
            focus: F_NAME,
            error: None,
            secret_saved: s.has_secret,
            sudo_saved: s.has_sudo,
            clear_secret: false,
            clear_sudo: false,
        }
    }

    /// Fields shown for the current auth method.
    pub fn visible(&self) -> Vec<usize> {
        let mut v = vec![F_NAME, F_HOST, F_PORT, F_USER, F_JUMP, F_AUTH];
        match self.auth {
            Auth::Agent => {}
            Auth::Key => v.extend([F_KEY, F_SECRET]),
            Auth::Password => v.push(F_SECRET),
        }
        v.push(F_SUDO);
        v
    }

    fn step(&mut self, dir: i32) {
        let v = self.visible();
        let i = v.iter().position(|&f| f == self.focus).unwrap_or(0) as i32;
        self.focus = v[(i + dir).rem_euclid(v.len() as i32) as usize];
    }

    fn is_last(&self) -> bool {
        self.visible().last() == Some(&self.focus)
    }

    pub fn input_mut(&mut self) -> Option<&mut Input> {
        match self.focus {
            F_NAME => Some(&mut self.name),
            F_HOST => Some(&mut self.host),
            F_PORT => Some(&mut self.port),
            F_USER => Some(&mut self.user),
            F_KEY => Some(&mut self.key),
            F_SECRET => Some(&mut self.secret),
            F_SUDO => Some(&mut self.sudo),
            _ => None,
        }
    }
}

pub struct PickEntry {
    pub name: String,
    pub is_dir: bool,
    pub key_like: bool,
}

/// File browser for choosing the private key.
pub struct Picker {
    pub dir: PathBuf,
    pub entries: Vec<PickEntry>,
    pub selected: usize,
    pub offset: usize,
    pub show_hidden: bool,
}

impl Picker {
    fn new(dir: PathBuf) -> Self {
        let mut p = Picker { dir, entries: vec![], selected: 0, offset: 0, show_hidden: true };
        p.reload();
        p
    }

    fn reload(&mut self) {
        let mut dirs = vec![];
        let mut files = vec![];
        if let Ok(rd) = std::fs::read_dir(&self.dir) {
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().to_string();
                if !self.show_hidden && name.starts_with('.') {
                    continue;
                }
                let path = e.path();
                if path.is_dir() {
                    dirs.push(PickEntry { name, is_dir: true, key_like: false });
                } else {
                    let key_like = looks_like_private_key(&path);
                    files.push(PickEntry { name, is_dir: false, key_like });
                }
            }
        }
        dirs.sort_by_key(|e| e.name.to_lowercase());
        // Keys first, then everything else.
        files.sort_by_key(|e| (!e.key_like, e.name.to_lowercase()));
        self.entries = dirs.into_iter().chain(files).collect();
        if self.dir.parent().is_some() {
            self.entries.insert(0, PickEntry { name: "..".into(), is_dir: true, key_like: false });
        }
        self.selected = 0;
        self.offset = 0;
    }

    fn go(&mut self, dir: PathBuf) {
        self.dir = dir;
        self.reload();
    }

    fn up(&mut self) {
        if let Some(p) = self.dir.parent() {
            let from = self.dir.file_name().map(|n| n.to_string_lossy().to_string());
            self.go(p.to_path_buf());
            if let Some(from) = from {
                if let Some(i) = self.entries.iter().position(|e| e.name == from) {
                    self.selected = i;
                }
            }
        }
    }

    /// Enters the selected folder, or returns the chosen file.
    fn activate(&mut self) -> Option<PathBuf> {
        let e = self.entries.get(self.selected)?;
        if e.name == ".." {
            self.up();
            None
        } else if e.is_dir {
            let d = self.dir.join(&e.name);
            self.go(d);
            None
        } else {
            Some(self.dir.join(&e.name))
        }
    }

    fn mv(&mut self, delta: i32) {
        let max = self.entries.len().saturating_sub(1) as i32;
        self.selected = (self.selected as i32 + delta).clamp(0, max) as usize;
    }
}

fn looks_like_private_key(path: &Path) -> bool {
    use std::io::Read;
    let Ok(meta) = std::fs::metadata(path) else { return false };
    if meta.len() == 0 || meta.len() > 16 * 1024 {
        return false;
    }
    let mut buf = Vec::new();
    std::fs::File::open(path)
        .and_then(|f| f.take(16 * 1024).read_to_end(&mut buf))
        .map(|_| String::from_utf8_lossy(&buf).contains("PRIVATE KEY-----"))
        .unwrap_or(false)
}

pub enum PromptKind {
    RenameSpace(u64),
    NewFolder(Option<u64>),
    RenameFolder(u64),
    RenameTab(usize),
}

pub struct Prompt {
    pub title: String,
    pub input: Input,
    pub kind: PromptKind,
}

pub enum ConfirmAction {
    DeleteSecret(u64, Kind),
    Delete(NodeId),
    DeleteSpace(u64),
    Quit,
}

pub struct Confirm {
    pub text: String,
    pub action: ConfirmAction,
}

/// What to do once the vault is unlocked.
pub enum Pending {
    Open(u64),
    Reconnect(usize),
    FillSudo(usize),
    SaveForm(Box<ServerForm>),
    Purge(Vec<u64>),
    OpenVault,
}

pub struct UnlockModal {
    pub input: Input,
    pub confirm: Input,
    /// First use: the master password is being chosen, not entered.
    pub creating: bool,
    pub focus: usize,
    pub error: Option<String>,
    pub pending: Pending,
}

pub struct MasterModal {
    pub input: Input,
    pub confirm: Input,
    pub focus: usize,
    pub error: Option<String>,
}

/// Secrets currently revealed in the vault screen; wiped on drop.
pub struct Shown {
    pub id: u64,
    pub login: Option<String>,
    pub sudo: Option<String>,
    pub at: Instant,
}

impl Drop for Shown {
    fn drop(&mut self) {
        self.login.zeroize();
        self.sudo.zeroize();
    }
}

pub struct VaultRow {
    pub id: u64,
    pub name: String,
    pub host: String,
    pub login: bool,
    pub sudo: bool,
}

pub struct VaultView {
    pub rows: Vec<VaultRow>,
    pub selected: usize,
    /// Highlighted column: 0 = login, 1 = sudo.
    pub col: usize,
    pub shown: Option<Shown>,
}

/// Change one saved secret from the vault screen.
pub struct SecretEdit {
    pub id: u64,
    pub kind: Kind,
    pub title: String,
    pub input: Input,
    pub show: bool,
    pub error: Option<String>,
    /// Column to return to in the vault screen.
    pub col: usize,
}

pub enum Modal {
    SecretEdit(Box<SecretEdit>),
    Unlock(Box<UnlockModal>),
    Master(Box<MasterModal>),
    Vault(Box<VaultView>),
    Form(Box<ServerForm>),
    Picker(Box<Picker>, Box<ServerForm>),
    Prompt(Prompt),
    Confirm(Confirm),
}

// ---------------------------------------------------------------- layout (lo rellena ui.rs al dibujar)

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Hit {
    ViewFolders,
    ViewJump,
    AddServer,
    AddFolder,
    Edit,
    ViewSpaces,
    NewTab,
    Field(usize),
    Browse,
    Save,
    Cancel,
    Yes,
    No,
}

/// Sidebar menu items in keyboard order: the three buttons in the line at the bottom (0..3), then the three
/// views at the top (3..6).
pub const HEADER: [Hit; 6] = [Hit::AddServer, Hit::AddFolder, Hit::Edit, Hit::ViewSpaces, Hit::ViewFolders, Hit::ViewJump];

/// `Tab::server_id` of the terminal of this computer (real servers start at 1).
pub const LOCAL: u64 = 0;

#[derive(Clone, Copy)]
pub struct TabHit {
    /// Index in `App::tabs` (the bar can be scrolled, so it is not the position in the hit list).
    pub idx: usize,
    pub rect: Rect,
    pub close: Rect,
}

#[derive(Default)]
pub struct Layout {
    pub sidebar: Rect,
    pub list: Rect,
    /// Height in screen lines of one row of the list (spaces are taller blocks).
    pub row_h: u16,
    pub tabbar: Rect,
    pub content: Rect,
    pub tabs: Vec<TabHit>,
    /// Rows of the agents panel: (area, index in `App::tabs`).
    pub agents: Vec<(Rect, usize)>,
    pub toolbar: Vec<(Rect, Hit)>,
    pub modal: Vec<(Rect, Hit)>,
    pub picker_list: Rect,
}

fn inside(r: Rect, x: u16, y: u16) -> bool {
    x >= r.x && x < r.x + r.width && y >= r.y && y < r.y + r.height
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum View {
    Folders,
    Jump,
    Spaces,
}

enum Drag {
    Node(NodeId),
    Tab(usize),
}

// ---------------------------------------------------------------- app

pub struct App {
    pub store: Store,
    pub vault: Vault,
    clip_clear_at: Option<Instant>,
    pub rows: Vec<Row>,
    pub selected: usize,
    pub offset: usize,
    pub tabs: Vec<Tab>,
    /// Index in `tabs` of the active tab of the current scope (meaningless while the scope has no tabs).
    pub active: usize,
    pub scope: Scope,
    /// The last active tab (by id) of each scope, to come back to it.
    remembered: HashMap<Scope, u64>,
    next_tab_id: u64,
    pub spaces: Spaces,
    /// Git branch of each space's directory, refreshed every few seconds.
    pub branches: HashMap<u64, String>,
    branches_at: Option<Instant>,
    /// Play a sound when an agent finishes.
    pub sound: bool,
    pub selection: Option<Selection>,
    /// The mouse button is down and the selection is being dragged out.
    selecting: bool,
    /// The background server holding the sessions, if there is one.
    pub daemon: Option<Arc<Daemon>>,
    lost_warned: bool,
    /// Sessions left running in the server when the interface quit (for the message after exiting).
    pub left_running: usize,
    pub focus: Focus,
    pub modal: Option<Modal>,
    pub layout: Layout,
    pub quit: bool,
    pub flash: Option<(String, Instant)>,
    /// Row under the pointer while dragging (highlighted as the drop target).
    pub drop_hover: Option<usize>,
    pub view: View,
    /// Keyboard focus on the sidebar header instead of the list: an index into `HEADER`.
    pub header: Option<usize>,
    /// Servers whose hidden hosts are unfolded in the jump view.
    /// Bastions the user folded in the jump view (everything is unfolded by default).
    jump_collapsed: HashSet<u64>,
    drag: Option<Drag>,
    last_click: Option<(Instant, u16, u16)>,
}

impl App {
    pub fn new(store: Store, vault: Vault) -> Self {
        let mut app = App {
            store,
            vault,
            clip_clear_at: None,
            rows: vec![],
            selected: 0,
            offset: 0,
            tabs: vec![],
            active: 0,
            scope: Scope::Ssh,
            remembered: HashMap::new(),
            next_tab_id: 0,
            spaces: Spaces::default(),
            branches: HashMap::new(),
            branches_at: None,
            sound: true,
            selection: None,
            selecting: false,
            daemon: None,
            lost_warned: false,
            left_running: 0,
            focus: Focus::Sidebar,
            modal: None,
            layout: Layout::default(),
            quit: false,
            flash: None,
            drop_hover: None,
            view: View::Folders,
            jump_collapsed: HashSet::new(),
            header: None,
            drag: None,
            last_click: None,
        };
        app.rebuild();
        app
    }

    /// A path with the home directory written as `~`.
    pub fn tilde(&self, path: &str) -> String {
        match home_dir() {
            Some(h) => match Path::new(path).strip_prefix(&h) {
                Ok(rest) if rest.as_os_str().is_empty() => "~".into(),
                Ok(rest) => format!("~/{}", rest.display()),
                Err(_) => path.to_string(),
            },
            None => path.to_string(),
        }
    }

    pub fn set_flash(&mut self, msg: impl Into<String>) {
        self.flash = Some((msg.into(), Instant::now()));
    }

    pub fn rebuild(&mut self) {
        fn walk(store: &Store, parent: Option<u64>, depth: usize, out: &mut Vec<Row>) {
            for f in store.folders.iter().filter(|f| f.parent == parent) {
                out.push(Row { node: NodeId::Folder(f.id), depth });
                if f.expanded {
                    walk(store, Some(f.id), depth + 1, out);
                }
            }
            for s in store.servers.iter().filter(|s| s.parent == parent) {
                out.push(Row { node: NodeId::Server(s.id), depth });
            }
        }
        fn walk_jump(store: &Store, via: Option<u64>, depth: usize, collapsed: &HashSet<u64>, out: &mut Vec<Row>) {
            if depth > 16 {
                return;
            }
            for s in store.servers.iter().filter(|s| s.jump == via) {
                // At the top level only bastions: servers that others are reached through.
                if via.is_none() && !store.servers.iter().any(|x| x.jump == Some(s.id)) {
                    continue;
                }
                out.push(Row { node: NodeId::Server(s.id), depth });
                if !collapsed.contains(&s.id) {
                    walk_jump(store, Some(s.id), depth + 1, collapsed, out);
                }
            }
        }
        self.rows.clear();
        match self.view {
            View::Folders => walk(&self.store, None, 0, &mut self.rows),
            View::Jump => walk_jump(&self.store, None, 0, &self.jump_collapsed, &mut self.rows),
            View::Spaces => self.rows.extend(self.spaces.spaces.iter().map(|s| Row { node: NodeId::Space(s.id), depth: 0 })),
        }
        self.selected = self.selected.min(self.rows.len().saturating_sub(1));
    }

    pub fn has_children(&self, node: NodeId) -> bool {
        match (node, self.view) {
            (NodeId::Folder(id), _) => {
                self.store.folders.iter().any(|f| f.parent == Some(id))
                    || self.store.servers.iter().any(|s| s.parent == Some(id))
            }
            (NodeId::Server(id), View::Jump) => self.store.servers.iter().any(|s| s.jump == Some(id)),
            (NodeId::Server(_), _) => false,
            (NodeId::Space(_), _) => false,
        }
    }

    pub fn is_open(&self, node: NodeId) -> bool {
        match node {
            NodeId::Folder(id) => self.store.folder(id).is_some_and(|f| f.expanded),
            NodeId::Server(id) => !self.jump_collapsed.contains(&id),
            NodeId::Space(_) => false,
        }
    }

    fn set_open(&mut self, node: NodeId, open: bool) {
        match node {
            NodeId::Folder(id) => {
                if let Some(f) = self.store.folders.iter_mut().find(|f| f.id == id) {
                    f.expanded = open;
                }
                self.persist();
            }
            NodeId::Server(id) => {
                if open {
                    self.jump_collapsed.remove(&id);
                } else {
                    self.jump_collapsed.insert(id);
                }
            }
            NodeId::Space(_) => {}
        }
        self.rebuild();
    }

    fn set_view(&mut self, view: View) {
        if self.view == view {
            return;
        }
        let keep = self.selected_node();
        self.view = view;
        self.offset = 0;
        self.rebuild();
        if view == View::Spaces {
            // Start on the space whose tabs are on screen.
            match self.scope {
                Scope::Space(id) => self.select_node(NodeId::Space(id)),
                Scope::Ssh => self.selected = 0,
            }
        } else if let Some(n) = keep {
            self.select_node(n);
        }
        self.sync_scope();
    }

    // ------------------------------------------------------------ scopes

    /// The scope whose tabs the current view shows: a space in the Spaces view, the SSH section otherwise.
    fn sync_scope(&mut self) {
        let want = match self.view {
            View::Spaces => match self.selected_node() {
                Some(NodeId::Space(id)) => Scope::Space(id),
                _ => Scope::Space(0),
            },
            _ => Scope::Ssh,
        };
        self.set_scope(want);
    }

    fn set_scope(&mut self, scope: Scope) {
        if self.scope == scope {
            return;
        }
        if let Some(t) = self.tabs.get(self.active).filter(|t| t.scope == self.scope) {
            self.remembered.insert(self.scope, t.id);
        }
        self.scope = scope;
        let back = self.remembered.get(&scope).and_then(|id| self.tabs.iter().position(|t| t.id == *id));
        if let Some(i) = back {
            self.active = i;
        }
        self.fix_active();
    }

    /// Indices in `tabs` of the tabs of the current scope, in order.
    pub fn scoped(&self) -> Vec<usize> {
        self.tabs.iter().enumerate().filter(|(_, t)| t.scope == self.scope).map(|(i, _)| i).collect()
    }

    /// Keeps `active` pointing at a tab of the current scope, and the keyboard off an empty terminal.
    fn fix_active(&mut self) {
        let sc = self.scoped();
        match sc.last() {
            None => self.focus = Focus::Sidebar,
            Some(&last) if !sc.contains(&self.active) => self.active = last,
            _ => {}
        }
    }

    /// Indices of the tabs where an AI agent is running, in any scope.
    pub fn agent_tabs(&self) -> Vec<usize> {
        self.tabs.iter().enumerate().filter(|(_, t)| t.session.agent().is_some()).map(|(i, _)| i).collect()
    }

    /// Name shown next to an agent: the space it runs in.
    pub fn tab_place(&self, t: &Tab) -> String {
        match t.scope {
            Scope::Space(id) => self.spaces.get(id).map(|s| s.name.clone()).unwrap_or_default(),
            Scope::Ssh => "local".into(),
        }
    }

    /// Shows tab `idx` wherever it lives and puts the keyboard on it.
    pub fn focus_tab(&mut self, idx: usize) {
        let Some(scope) = self.tabs.get(idx).map(|t| t.scope) else { return };
        match scope {
            Scope::Space(id) => {
                self.set_view(View::Spaces);
                self.select_node(NodeId::Space(id));
            }
            Scope::Ssh if self.view == View::Spaces => self.set_view(View::Folders),
            Scope::Ssh => {}
        }
        self.sync_scope();
        self.active = idx;
        self.tabs[idx].attention = false;
        self.focus = Focus::Terminal;
    }

    /// Alt+N: the agent that wants attention, else the next one after the current tab.
    fn next_agent(&mut self) {
        let agents = self.agent_tabs();
        let pick = agents
            .iter()
            .copied()
            .find(|&i| self.tabs[i].attention)
            .or_else(|| agents.iter().copied().find(|&i| i > self.active))
            .or_else(|| agents.first().copied());
        match pick {
            Some(i) => self.focus_tab(i),
            None => self.set_flash("No agents running"),
        }
    }

    fn select_node(&mut self, node: NodeId) {
        if let Some(i) = self.rows.iter().position(|r| r.node == node) {
            self.selected = i;
        }
    }

    pub fn selected_node(&self) -> Option<NodeId> {
        self.rows.get(self.selected).map(|r| r.node)
    }

    /// Folder where new items would be created.
    fn current_parent(&self) -> Option<u64> {
        match self.selected_node()? {
            NodeId::Folder(id) if self.view == View::Folders => Some(id),
            n => self.store.parent_of(n),
        }
    }

    /// Parent for a new folder: always a sibling of the selected item.
    fn new_folder_parent(&self) -> Option<u64> {
        self.store.parent_of(self.selected_node()?)
    }

    fn persist(&mut self) {
        if let Err(e) = self.store.save() {
            self.set_flash(format!("Could not save: {e}"));
        }
    }

    /// Tells the background server what each tab is, whenever it changes, so the tabs can be rebuilt later.
    fn sync_meta(&mut self) {
        if self.daemon.is_none() {
            return;
        }
        for (i, t) in self.tabs.iter_mut().enumerate() {
            let space = match t.scope {
                Scope::Space(id) => Some(id),
                Scope::Ssh => None,
            };
            let meta = serde_json::json!({
                "title": t.title,
                "server_id": t.server_id,
                "space": space,
                "order": i,
                "attention": t.attention,
            });
            let text = meta.to_string();
            if text != t.meta_sent {
                t.session.set_meta(&meta);
                t.meta_sent = text;
            }
        }
    }

    /// Re-creates the tabs of the sessions that were left running in the background server.
    pub fn restore_sessions(&mut self, rows: u16, cols: u16) {
        let Some(d) = self.daemon.clone() else { return };
        let Ok(mut list) = d.list() else { return };
        list.sort_by_key(|i| i.meta.get("order").and_then(|v| v.as_u64()).unwrap_or(u64::MAX));
        for info in &list {
            let meta = &info.meta;
            let scope = match meta.get("space").and_then(|v| v.as_u64()) {
                Some(id) if self.spaces.get(id).is_some() => Scope::Space(id),
                _ => Scope::Ssh,
            };
            let title = meta.get("title").and_then(|v| v.as_str()).unwrap_or("session").to_string();
            let server_id = meta.get("server_id").and_then(|v| v.as_u64()).unwrap_or(LOCAL);
            self.next_tab_id += 1;
            self.tabs.push(Tab {
                id: self.next_tab_id,
                title,
                server_id,
                scope,
                session: Session::from_remote(d.attach(info.sid, rows, cols)),
                attention: meta.get("attention").and_then(|v| v.as_bool()).unwrap_or(false),
                meta_sent: String::new(),
            });
        }
        if !list.is_empty() {
            self.fix_active();
            self.set_flash(format!("Restored {} session(s) from the background server", list.len()));
        }
    }

    pub fn live_sessions(&self) -> usize {
        self.tabs.iter().filter(|t| t.session.exit_code.is_none()).count()
    }

    /// Called on every pass of the event loop.
    pub fn tick(&mut self) {
        let mut finished = false;
        for t in &mut self.tabs {
            t.session.poll_exit();
            t.session.poll_agent();
            if t.session.take_done() {
                t.attention = true;
                finished = true;
            }
        }
        if finished && self.sound {
            crate::notify::ring();
        }
        // Looking at an agent's tab is the attention it asked for.
        if self.focus == Focus::Terminal && self.modal.is_none() {
            let scope = self.scope;
            if let Some(t) = self.tabs.get_mut(self.active).filter(|t| t.scope == scope) {
                t.attention = false;
            }
        }
        self.follow_directories();
        self.sync_meta();
        if !self.lost_warned && self.daemon.as_ref().is_some_and(|d| !d.alive()) {
            self.lost_warned = true;
            self.set_flash("Lost the background server: its sessions ended");
        }
        if self.branches_at.is_none_or(|t| t.elapsed() > Duration::from_secs(3)) {
            self.branches_at = Some(Instant::now());
            self.branches = self
                .spaces
                .spaces
                .iter()
                .filter_map(|s| spaces::git_branch(Path::new(&expand_tilde(&s.cwd))).map(|b| (s.id, b)))
                .collect();
        }
        if self.flash.as_ref().is_some_and(|(_, t)| t.elapsed() > Duration::from_secs(5)) {
            self.flash = None;
        }
        self.vault.lock_if_idle(VAULT_IDLE);
        if !self.vault.is_unlocked() && matches!(self.modal, Some(Modal::Vault(_)) | Some(Modal::Master(_))) {
            self.modal = None;
            self.set_flash("Vault locked after being idle");
        }
        if let Some(Modal::Vault(v)) = self.modal.as_mut() {
            if v.shown.as_ref().is_some_and(|s| s.at.elapsed() > REVEAL_FOR) {
                v.shown = None;
            }
        }
        if self.clip_clear_at.is_some_and(|t| Instant::now() >= t) {
            self.clip_clear_at = None;
            osc52("");
        }
    }

    fn request_quit(&mut self) {
        let n = self.live_sessions();
        if n == 0 {
            self.quit = true;
        } else if self.daemon.as_ref().is_some_and(|d| d.alive()) {
            // The sessions live in the background server: leaving only detaches from them.
            self.left_running = n;
            self.quit = true;
        } else {
            self.modal = Some(Modal::Confirm(Confirm {
                text: format!("{n} session(s) still open. Quit ship?"),
                action: ConfirmAction::Quit,
            }));
        }
    }

    // ------------------------------------------------------------ sessions and tabs

    pub fn open_server(&mut self, id: u64) {
        self.modal = self.gate(Pending::Open(id));
    }

    fn reconnect(&mut self, idx: usize) {
        self.modal = self.gate(Pending::Reconnect(idx));
    }

    /// Starts `ssh` for a server, in a new tab or replacing the session of tab `reuse`.
    fn connect(&mut self, id: u64, reuse: Option<usize>, fills: Vec<Autofill>) {
        let reuse = reuse.filter(|&i| i < self.tabs.len());
        // Where the tab lives: a reconnected tab stays put, a local terminal opens in the current space,
        // and servers always go in the SSH section.
        let in_space = matches!(self.scope, Scope::Space(s) if s != 0);
        let scope = match reuse {
            Some(i) => self.tabs[i].scope,
            None if id == LOCAL && in_space => self.scope,
            None => Scope::Ssh,
        };
        if reuse.is_none() && id == LOCAL && self.view == View::Spaces && !in_space {
            return self.set_flash("Create an agent first (press a)");
        }
        let cwd = match scope {
            Scope::Space(sid) => self.spaces.get(sid).map(|s| PathBuf::from(expand_tilde(&s.cwd))),
            Scope::Ssh => None,
        };
        let (argv, title, login_expected) = if id == LOCAL {
            (local_shell(), if cwd.is_some() { "shell" } else { "Local" }.to_string(), false)
        } else {
            let Some(server) = self.store.server(id).cloned() else {
                return self.set_flash("This server no longer exists");
            };
            (ssh_argv(&self.store, &server), server.name.clone(), server.auth == Auth::Password)
        };
        let (rows, cols) = match reuse {
            Some(i) => self.tabs[i].session.size(),
            None => {
                let c = self.layout.content;
                if c.width > 0 && c.height > 0 { (c.height, c.width) } else { (24, 80) }
            }
        };
        if self.daemon.as_ref().is_some_and(|d| !d.alive()) {
            self.daemon = None; // the server is gone: carry on inside this process
        }
        let opts = SpawnOpts { argv: argv.clone(), rows, cols, fills, login_expected, cwd, env: None, tap: None };
        match Session::spawn_with(opts, self.daemon.as_deref()) {
            Ok(session) => match reuse {
                Some(i) => {
                    let mut old = std::mem::replace(&mut self.tabs[i].session, session);
                    old.kill();
                    self.tabs[i].meta_sent.clear();
                }
                None => {
                    self.next_tab_id += 1;
                    self.tabs.push(Tab {
                        id: self.next_tab_id,
                        title,
                        server_id: id,
                        scope,
                        session,
                        attention: false,
                        meta_sent: String::new(),
                    });
                    self.set_scope(scope);
                    self.active = self.tabs.len() - 1;
                    self.focus = Focus::Terminal;
                }
            },
            Err(e) => self.set_flash(format!("Could not start {}: {e:#}", argv[0])),
        }
    }

    // ------------------------------------------------------------ vault

    fn needs_vault(&self, p: &Pending) -> bool {
        let wants_secret = |s: &Server| s.auth != Auth::Agent && s.has_secret;
        // The destination or any jump host on the way may have a saved secret.
        let chain_wants = |id: u64| {
            self.store.server(id).is_some_and(wants_secret) || self.store.jump_chain(id).into_iter().any(wants_secret)
        };
        match p {
            Pending::Open(id) => chain_wants(*id),
            Pending::Reconnect(i) => self.tabs.get(*i).is_some_and(|t| chain_wants(t.server_id)),
            Pending::Purge(ids) => !ids.is_empty() && self.vault.exists(),
            _ => true,
        }
    }

    /// Runs `p` now if the vault is not needed (or already unlocked); otherwise asks for the master password.
    fn gate(&mut self, p: Pending) -> Option<Modal> {
        if self.vault.is_unlocked() || !self.needs_vault(&p) {
            return self.run_pending(p);
        }
        Some(Modal::Unlock(Box::new(UnlockModal {
            input: Input::default(),
            confirm: Input::default(),
            creating: !self.vault.exists(),
            focus: 0,
            error: None,
            pending: p,
        })))
    }

    /// The saved secrets for reaching `id`: its own and those of every jump host on the way, each tied to the
    /// prompt it answers, so that a hop's password is never typed at another host's prompt.
    fn autofills(&mut self, id: u64) -> Vec<Autofill> {
        let mut servers: Vec<Server> = self.store.jump_chain(id).into_iter().cloned().collect();
        servers.extend(self.store.server(id).cloned());
        let mut out = vec![];
        for s in servers {
            if s.auth == Auth::Agent || !s.has_secret {
                continue;
            }
            let Some(secret) = self.vault.get(s.id, Kind::Login) else { continue };
            let when = match s.auth {
                Auth::Key => format!("'{}'", expand_tilde(&s.key_path)).to_lowercase(),
                _ => format!("{}@{}", s.user, s.host).to_lowercase(),
            };
            out.push(Autofill { when, secret, fallback: s.id == id });
        }
        out
    }

    fn run_pending(&mut self, p: Pending) -> Option<Modal> {
        match p {
            Pending::Open(id) => {
                let fills = self.autofills(id);
                self.connect(id, None, fills);
                None
            }
            Pending::Reconnect(idx) => {
                let id = self.tabs.get(idx)?.server_id;
                let fills = self.autofills(id);
                self.connect(id, Some(idx), fills);
                None
            }
            Pending::FillSudo(idx) => {
                let id = self.tabs.get(idx)?.server_id;
                match self.vault.get(id, Kind::Sudo) {
                    Some(mut pw) => {
                        if let Some(t) = self.tabs.get(idx) {
                            t.session.write(format!("{pw}\r").as_bytes());
                        }
                        pw.zeroize();
                    }
                    None => self.set_flash("No sudo password is saved for this server"),
                }
                None
            }
            Pending::SaveForm(f) => self.finish_save(f),
            Pending::Purge(ids) => {
                if self.vault.is_unlocked() {
                    for id in ids {
                        let _ = self.vault.remove_server(id);
                    }
                }
                None
            }
            Pending::OpenVault => Some(Modal::Vault(Box::new(VaultView::new(&self.store, None, 0)))),
        }
    }

    /// The user dismissed the unlock prompt: carry on without the vault where that makes sense.
    fn abort_pending(&mut self, p: Pending) -> Option<Modal> {
        match p {
            Pending::Open(id) => {
                self.connect(id, None, vec![]);
                self.set_flash("Connecting without the saved password");
                None
            }
            Pending::Reconnect(idx) => {
                if let Some(id) = self.tabs.get(idx).map(|t| t.server_id) {
                    self.connect(id, Some(idx), vec![]);
                }
                None
            }
            Pending::SaveForm(f) => Some(Modal::Form(f)),
            Pending::Purge(_) => {
                self.set_flash("Saved passwords of the deleted servers remain in the vault");
                None
            }
            _ => None,
        }
    }

    fn unlock_key(&mut self, mut u: Box<UnlockModal>, key: KeyEvent) -> Option<Modal> {
        match key.code {
            KeyCode::Esc => return self.abort_pending(u.pending),
            KeyCode::Tab | KeyCode::BackTab | KeyCode::Up | KeyCode::Down if u.creating => u.focus = 1 - u.focus,
            KeyCode::Enter if u.creating && u.focus == 0 => u.focus = 1,
            KeyCode::Enter if u.creating => {
                if u.input.value.chars().count() < MIN_MASTER {
                    u.error = Some(format!("Use at least {MIN_MASTER} characters"));
                } else if u.input.value != u.confirm.value {
                    u.error = Some("The two passwords do not match".into());
                    u.confirm = Input::default();
                } else {
                    match self.vault.create(&u.input.value) {
                        Ok(()) => {
                            return self.run_pending(u.pending);
                        }
                        Err(e) => u.error = Some(format!("{e:#}")),
                    }
                }
            }
            KeyCode::Enter => match self.vault.unlock(&u.input.value) {
                Ok(()) => {
                    return self.run_pending(u.pending);
                }
                Err(UnlockError::WrongPassword) => {
                    u.error = Some("Wrong master password".into());
                    u.input = Input::default();
                }
                Err(UnlockError::Other(m)) => u.error = Some(m),
            },
            _ => {
                let field = if u.focus == 0 { &mut u.input } else { &mut u.confirm };
                field.handle(key);
            }
        }
        Some(Modal::Unlock(u))
    }

    fn master_key(&mut self, mut m: Box<MasterModal>, key: KeyEvent) -> Option<Modal> {
        match key.code {
            KeyCode::Esc => return None,
            KeyCode::Tab | KeyCode::BackTab | KeyCode::Up | KeyCode::Down => m.focus = 1 - m.focus,
            KeyCode::Enter if m.focus == 0 => m.focus = 1,
            KeyCode::Enter => {
                if m.input.value.chars().count() < MIN_MASTER {
                    m.error = Some(format!("Use at least {MIN_MASTER} characters"));
                } else if m.input.value != m.confirm.value {
                    m.error = Some("The two passwords do not match".into());
                    m.confirm = Input::default();
                } else {
                    match self.vault.change_master(&m.input.value) {
                        Ok(()) => {
                            self.set_flash("Master password changed");
                            return None;
                        }
                        Err(e) => m.error = Some(format!("{e:#}")),
                    }
                }
            }
            _ => {
                let field = if m.focus == 0 { &mut m.input } else { &mut m.confirm };
                field.handle(key);
            }
        }
        Some(Modal::Master(m))
    }

    fn vault_key(&mut self, mut v: Box<VaultView>, key: KeyEvent) -> Option<Modal> {
        self.vault.touch();
        let last = v.rows.len().saturating_sub(1);
        let row = v.rows.get(v.selected).map(|r| (r.id, r.name.clone()));
        let kind = if v.col == 0 { Kind::Login } else { Kind::Sudo };
        let what = if v.col == 0 { "login password" } else { "sudo password" };
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => return None,
            KeyCode::Up | KeyCode::Char('k') => v.selected = v.selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => v.selected = (v.selected + 1).min(last),
            KeyCode::Left | KeyCode::Char('h') => v.col = 0,
            KeyCode::Right | KeyCode::Char('l') => v.col = 1,
            KeyCode::Char('m') => {
                return Some(Modal::Master(Box::new(MasterModal {
                    input: Input::default(),
                    confirm: Input::default(),
                    focus: 0,
                    error: None,
                })));
            }
            KeyCode::Enter | KeyCode::Char('r') => {
                let Some((id, _)) = row else { return Some(Modal::Vault(v)) };
                if v.shown.as_ref().is_some_and(|s| s.id == id) {
                    v.shown = None;
                } else {
                    v.shown = Some(Shown {
                        id,
                        login: self.vault.get(id, Kind::Login),
                        sudo: self.vault.get(id, Kind::Sudo),
                        at: Instant::now(),
                    });
                }
            }
            KeyCode::Char('c') => {
                if let Some((id, _)) = row {
                    match self.vault.get(id, kind) {
                        Some(mut pw) => {
                            osc52(&pw);
                            pw.zeroize();
                            self.clip_clear_at = Some(Instant::now() + CLIPBOARD_FOR);
                            self.set_flash("Copied (the clipboard is cleared in 30 s)");
                        }
                        None => self.set_flash("Nothing saved there"),
                    }
                }
            }
            KeyCode::Char('e') => {
                if let Some((id, name)) = row {
                    return Some(Modal::SecretEdit(Box::new(SecretEdit {
                        id,
                        kind,
                        title: format!("Change the {what} - {name}"),
                        input: Input::default(),
                        show: false,
                        error: None,
                        col: v.col,
                    })));
                }
            }
            KeyCode::Char('d') | KeyCode::Delete => {
                if let Some((id, name)) = row {
                    if self.vault.get(id, kind).is_none() {
                        self.set_flash("Nothing saved there");
                    } else {
                        return Some(Modal::Confirm(Confirm {
                            text: format!("Remove the saved {what} of “{name}”?"),
                            action: ConfirmAction::DeleteSecret(id, kind),
                        }));
                    }
                }
            }
            _ => {}
        }
        Some(Modal::Vault(v))
    }

    /// Keeps the `has_secret` / `has_sudo` flag of a server in step with the vault.
    fn set_secret_flag(&mut self, id: u64, kind: Kind, saved: bool) {
        if let Some(s) = self.store.servers.iter_mut().find(|s| s.id == id) {
            match kind {
                Kind::Login => s.has_secret = saved,
                Kind::Sudo => s.has_sudo = saved,
            }
        }
        self.persist();
    }

    fn vault_view(&self, id: u64, col: usize) -> Option<Modal> {
        Some(Modal::Vault(Box::new(VaultView::new(&self.store, Some(id), col))))
    }

    fn secret_edit_key(&mut self, mut e: Box<SecretEdit>, key: KeyEvent) -> Option<Modal> {
        self.vault.touch();
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => return self.vault_view(e.id, e.col),
            KeyCode::Char('t') if ctrl => e.show = !e.show,
            KeyCode::Enter => {
                if e.input.value.is_empty() {
                    e.error = Some("Empty: use d in the vault to remove it instead".into());
                } else {
                    match self.vault.set(e.id, e.kind, Some(&e.input.value)) {
                        Ok(()) => {
                            self.set_secret_flag(e.id, e.kind, true);
                            self.set_flash("Saved");
                            return self.vault_view(e.id, e.col);
                        }
                        Err(err) => e.error = Some(format!("{err:#}")),
                    }
                }
            }
            _ => e.input.handle(key),
        }
        Some(Modal::SecretEdit(e))
    }

    fn sudo_ready(&self) -> bool {
        self.focus == Focus::Terminal
            && self.tabs.get(self.active).is_some_and(|t| {
                t.session.exit_code.is_none()
                    && t.session.sudo_prompt()
                    && self.store.server(t.server_id).is_some_and(|s| s.has_sudo)
            })
    }

    fn close_tab(&mut self, idx: usize) {
        if idx >= self.tabs.len() {
            return;
        }
        // The tab to land on: its neighbour in the same scope.
        let scope = self.tabs[idx].scope;
        let same: Vec<usize> = self.tabs.iter().enumerate().filter(|(_, t)| t.scope == scope).map(|(i, _)| i).collect();
        let pos = same.iter().position(|&i| i == idx).unwrap_or(0);
        let neighbour = if pos > 0 { Some(same[pos - 1]) } else { same.get(1).copied() };
        let was_active = idx == self.active;
        self.tabs.remove(idx).session.kill();
        if self.active > idx {
            self.active -= 1;
        }
        if was_active {
            if let Some(n) = neighbour {
                self.active = if n > idx { n - 1 } else { n };
            }
        }
        self.fix_active();
    }

    /// Closes the open session of the selected server (the active tab if it is that server's, else the newest one).
    fn close_selected_tab(&mut self) {
        let id = match self.selected_node() {
            Some(NodeId::Server(id)) => id,
            Some(NodeId::Space(_)) => {
                return match self.scoped().contains(&self.active) {
                    true => self.close_tab(self.active),
                    false => self.set_flash("No open terminal in this agent"),
                };
            }
            _ => return,
        };
        let idx = match self.tabs.get(self.active) {
            Some(t) if t.server_id == id => Some(self.active),
            _ => self.tabs.iter().rposition(|t| t.server_id == id),
        };
        match idx {
            Some(i) => self.close_tab(i),
            None => self.set_flash("No open session for this server"),
        }
    }

    /// Switches to tab `i` (0-based) and puts the keyboard on its terminal.
    fn goto_tab(&mut self, i: usize) {
        if let Some(&g) = self.scoped().get(i) {
            self.active = g;
            self.focus = Focus::Terminal;
        } else {
            self.set_flash(format!("No tab {}", i + 1));
        }
    }

    fn move_tab(&mut self, from: usize, to: usize) {
        if from == to || from >= self.tabs.len() || to >= self.tabs.len() {
            return;
        }
        let t = self.tabs.remove(from);
        self.tabs.insert(to, t);
        self.active = to;
    }

    fn rename_tab_prompt(&mut self) {
        if let Some(t) = self.tabs.get(self.active) {
            self.modal = Some(Modal::Prompt(Prompt {
                title: "Rename tab".into(),
                input: Input::new(&t.title),
                kind: PromptKind::RenameTab(self.active),
            }));
        }
    }

    // ------------------------------------------------------------ teclado

    pub fn on_key(&mut self, key: KeyEvent) {
        self.on_key_inner(key);
        self.sync_scope();
    }

    fn on_key_inner(&mut self, key: KeyEvent) {
        self.selection = None;
        self.selecting = false;
        if self.modal.is_some() {
            self.modal_key(key);
            return;
        }
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let scoped = self.scoped();
        let n = scoped.len();
        let pos = scoped.iter().position(|&g| g == self.active).unwrap_or(0);
        match key.code {
            KeyCode::F(6) => {
                self.focus = if self.focus == Focus::Terminal || n == 0 { Focus::Sidebar } else { Focus::Terminal };
                return;
            }
            KeyCode::F(2) if n > 0 => return self.rename_tab_prompt(),
            KeyCode::Char('p') if alt && self.sudo_ready() => {
                self.modal = self.gate(Pending::FillSudo(self.active));
                return;
            }
            KeyCode::Left if alt && shift && n > 0 => return self.move_tab(self.active, scoped[pos.saturating_sub(1)]),
            KeyCode::Right if alt && shift && n > 0 => return self.move_tab(self.active, scoped[(pos + 1).min(n - 1)]),
            KeyCode::Left if alt && n > 0 => return self.active = scoped[(pos + n - 1) % n],
            KeyCode::Right if alt && n > 0 => return self.active = scoped[(pos + 1) % n],
            KeyCode::Char('n') if alt => return self.next_agent(),
            KeyCode::Char('w') if alt && n > 0 => return self.close_tab(self.active),
            KeyCode::Char(c @ '1'..='9') if alt => return self.goto_tab(c as usize - '1' as usize),
            _ => {}
        }
        if self.focus == Focus::Terminal && n > 0 {
            self.terminal_key(key);
        } else {
            self.sidebar_key(key);
        }
    }

    fn terminal_key(&mut self, key: KeyEvent) {
        let idx = self.active;
        let Some(tab) = self.tabs.get(idx) else { return };
        if tab.session.exit_code.is_some() {
            if key.code == KeyCode::Enter {
                self.reconnect(idx);
            }
            return;
        }
        if key.modifiers.contains(KeyModifiers::SHIFT) {
            match key.code {
                KeyCode::PageUp => return tab.session.scroll(tab.session.size().0 as i32 / 2),
                KeyCode::PageDown => return tab.session.scroll(-(tab.session.size().0 as i32 / 2)),
                KeyCode::Home => return tab.session.scroll(1_000_000),
                KeyCode::End => return tab.session.reset_scroll(),
                _ => {}
            }
        }
        let app_cursor = tab.session.with_screen(|s| s.application_cursor());
        if let Some(bytes) = keys::encode(key, app_cursor) {
            tab.session.reset_scroll();
            tab.session.write(&bytes);
        }
    }

    /// The header item (index into `HEADER`) of the current view.
    fn view_index(&self) -> usize {
        match self.view {
            View::Spaces => 3,
            View::Folders => 4,
            View::Jump => 5,
        }
    }

    /// Runs a sidebar header button (by mouse or keyboard).
    fn activate(&mut self, hit: Hit) {
        let spaces = self.view == View::Spaces;
        match hit {
            Hit::ViewFolders => self.set_view(View::Folders),
            Hit::ViewJump => self.set_view(View::Jump),
            Hit::ViewSpaces => self.set_view(View::Spaces),
            Hit::AddServer if spaces => self.new_space(),
            Hit::AddServer => self.new_server_form(),
            Hit::AddFolder if spaces => self.open_server(LOCAL),
            Hit::AddFolder => self.new_folder_prompt(),
            Hit::Edit => self.edit_selected(),
            Hit::NewTab => self.open_server(LOCAL),
            _ => {}
        }
    }

    /// Moves the menu highlight. On the views it also opens the view, so the arrows are enough to switch.
    fn move_in_menu(&mut self, to: usize, buttons: bool) {
        self.header = Some(to);
        if !buttons {
            self.activate(HEADER[to]);
        }
    }

    /// Keys while the menu has focus: the views above the list or the buttons below it. Returns true if the
    /// key was consumed.
    fn header_key(&mut self, i: usize, key: KeyEvent) -> bool {
        let buttons = i < 3;
        let (row_start, row_end) = if buttons { (0, 3) } else { (3, 6) };
        match key.code {
            KeyCode::Left | KeyCode::Char('h') => self.move_in_menu(i.saturating_sub(1).max(row_start), buttons),
            KeyCode::Right | KeyCode::Char('l') => self.move_in_menu((i + 1).min(row_end - 1), buttons),
            // The buttons are below the list and the views above it: the arrow that leads back to the list.
            KeyCode::Up | KeyCode::Char('k') if buttons => self.header = None,
            KeyCode::Down | KeyCode::Char('j') if !buttons => self.header = None,
            KeyCode::Up | KeyCode::Char('k') | KeyCode::Down | KeyCode::Char('j') => {}
            KeyCode::Esc => self.header = None,
            KeyCode::Enter | KeyCode::Char(' ') => {
                // A button opens a form or prompt; on a view, Enter is just a way to (re)open it.
                if buttons {
                    self.header = None;
                }
                self.activate(HEADER[i]);
            }
            _ => return false,
        }
        true
    }

    fn sidebar_key(&mut self, key: KeyEvent) {
        if let Some(i) = self.header {
            if self.header_key(i, key) {
                return;
            }
            self.header = None;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let last = self.rows.len().saturating_sub(1);
        match key.code {
            KeyCode::Up | KeyCode::Char('k') if alt => self.shift_selected(-1),
            KeyCode::Down | KeyCode::Char('j') if alt => self.shift_selected(1),
            KeyCode::Up | KeyCode::Char('k') if self.selected == 0 => self.header = Some(self.view_index()),
            KeyCode::Up | KeyCode::Char('k') => self.selected = self.selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') if self.selected >= last => self.header = Some(0),
            KeyCode::Down | KeyCode::Char('j') => self.selected += 1,
            KeyCode::Home | KeyCode::Char('g') => self.selected = 0,
            KeyCode::End | KeyCode::Char('G') => self.selected = last,
            KeyCode::Right | KeyCode::Char('l') if matches!(self.selected_node(), Some(NodeId::Space(_))) => {
                if let Some(NodeId::Space(id)) = self.selected_node() {
                    self.open_space(id);
                }
            }
            KeyCode::Right | KeyCode::Char('l') => self.step_in(),
            KeyCode::Left | KeyCode::Char('h') => self.step_out(),
            KeyCode::Enter | KeyCode::Char(' ') => match self.selected_node() {
                Some(NodeId::Space(id)) => self.open_space(id),
                Some(NodeId::Server(id)) => self.open_server(id),
                Some(n @ NodeId::Folder(_)) => self.toggle(n),
                None => {}
            },
            KeyCode::Char('v') | KeyCode::Tab => self.set_view(match self.view {
                View::Spaces => View::Folders,
                View::Folders => View::Jump,
                View::Jump => View::Spaces,
            }),
            KeyCode::Char(c @ '1'..='9') if !ctrl && !alt => self.goto_tab(c as usize - '1' as usize),
            KeyCode::Char('t') => self.open_server(LOCAL),
            KeyCode::Char('p') => self.modal = self.gate(Pending::OpenVault),
            KeyCode::Char('a') if self.view == View::Spaces => self.new_space(),
            KeyCode::Char('a') => self.new_server_form(),
            KeyCode::Char('f') => self.new_folder_prompt(),
            KeyCode::Char('e') | KeyCode::F(4) => self.edit_selected(),
            KeyCode::Char('d') | KeyCode::Delete => self.ask_delete(),
            KeyCode::Char('c') if !ctrl => self.close_selected_tab(),
            KeyCode::Char('q') => self.request_quit(),
            KeyCode::Char('c') if ctrl => self.request_quit(),
            _ => {}
        }
    }

    fn shift_selected(&mut self, delta: i32) {
        if let Some(NodeId::Space(id)) = self.selected_node() {
            if self.spaces.shift(id, delta) {
                self.save_spaces();
                self.rebuild();
                self.select_node(NodeId::Space(id));
            }
            return;
        }
        if let Some(n) = self.selected_node() {
            if self.store.shift(n, delta, self.view == View::Jump) {
                self.persist();
                self.rebuild();
                self.select_node(n);
            }
        }
    }

    fn toggle(&mut self, node: NodeId) {
        let open = self.is_open(node);
        self.set_open(node, !open);
    }

    /// Right arrow: unfold, or move into the first child if already unfolded.
    fn step_in(&mut self) {
        let Some(node) = self.selected_node() else { return };
        if !self.has_children(node) {
            return;
        }
        if !self.is_open(node) {
            return self.set_open(node, true);
        }
        let depth = self.rows[self.selected].depth;
        if self.rows.get(self.selected + 1).is_some_and(|r| r.depth > depth) {
            self.selected += 1;
        }
    }

    /// Left arrow: fold, or go back up to the parent row.
    fn step_out(&mut self) {
        let Some(node) = self.selected_node() else { return };
        if self.has_children(node) && self.is_open(node) {
            return self.set_open(node, false);
        }
        let depth = self.rows[self.selected].depth;
        if let Some(i) = self.rows[..self.selected].iter().rposition(|r| r.depth < depth) {
            self.selected = i;
        }
    }

    fn new_server_form(&mut self) {
        let jump = match (self.view, self.selected_node()) {
            (View::Jump, Some(NodeId::Server(id))) => Some(id),
            _ => None,
        };
        self.modal = Some(Modal::Form(Box::new(ServerForm::new(self.current_parent(), jump))));
    }

    fn new_folder_prompt(&mut self) {
        if self.view != View::Folders {
            return self.set_flash("Folders are created in the SSH view (press v)");
        }
        self.modal = Some(Modal::Prompt(Prompt {
            title: "New folder".into(),
            input: Input::default(),
            kind: PromptKind::NewFolder(self.new_folder_parent()),
        }));
    }

    fn save_spaces(&mut self) {
        if let Err(e) = self.spaces.save() {
            self.set_flash(format!("Could not save agents: {e}"));
        }
    }

    /// A new space: opens a plain terminal. Wherever you leave it (`cd`, `mkdir`...) becomes the space's directory.
    fn new_space(&mut self) {
        let start = match self.selected_node() {
            Some(NodeId::Space(id)) if self.view == View::Spaces => {
                self.spaces.get(id).map(|s| PathBuf::from(expand_tilde(&s.cwd)))
            }
            _ => None,
        };
        let dir = start.filter(|d| d.is_dir()).or_else(home_dir).unwrap_or_else(|| PathBuf::from("/"));
        let id = self.spaces.add(space_name(&dir), dir.display().to_string());
        self.save_spaces();
        self.rebuild();
        self.open_space(id);
    }

    /// Spaces follow their terminal: the directory the shell is in becomes the space's directory (and,
    /// unless it was renamed, its name and git branch).
    fn follow_directories(&mut self) {
        let mut moved = vec![];
        for sp in &self.spaces.spaces {
            let scope = Scope::Space(sp.id);
            // The terminal that counts is the one the space is showing, or the one it showed last.
            let tab = if scope == self.scope {
                self.tabs.get(self.active).filter(|t| t.scope == scope)
            } else {
                self.remembered
                    .get(&scope)
                    .and_then(|id| self.tabs.iter().find(|t| t.id == *id))
                    .or_else(|| self.tabs.iter().find(|t| t.scope == scope))
            };
            if let Some(dir) = tab.and_then(|t| t.session.cwd()) {
                moved.push((sp.id, dir.display().to_string()));
            }
        }
        let mut changed = false;
        for (id, dir) in moved {
            changed |= self.spaces.follow_by_dir(id, &dir, space_name(Path::new(&dir)));
        }
        if changed {
            self.save_spaces();
        }
    }

    /// Shows a space and puts the keyboard on its terminal, opening the first one if it has none.
    fn open_space(&mut self, id: u64) {
        self.select_node(NodeId::Space(id));
        self.sync_scope();
        if self.scoped().is_empty() {
            self.open_server(LOCAL);
        } else {
            self.focus = Focus::Terminal;
        }
    }

    fn edit_selected(&mut self) {
        match self.selected_node() {
            Some(NodeId::Space(id)) => {
                if let Some(sp) = self.spaces.get(id) {
                    self.modal = Some(Modal::Prompt(Prompt {
                        title: "Rename agent".into(),
                        input: Input::new(&sp.name),
                        kind: PromptKind::RenameSpace(id),
                    }));
                }
            }
            Some(NodeId::Server(id)) => {
                if let Some(s) = self.store.server(id) {
                    self.modal = Some(Modal::Form(Box::new(ServerForm::from_server(s))));
                }
            }
            Some(NodeId::Folder(id)) => {
                if let Some(f) = self.store.folder(id) {
                    self.modal = Some(Modal::Prompt(Prompt {
                        title: "Rename folder".into(),
                        input: Input::new(&f.name),
                        kind: PromptKind::RenameFolder(id),
                    }));
                }
            }
            None => {}
        }
    }

    fn ask_delete(&mut self) {
        let Some(node) = self.selected_node() else { return };
        let text = match node {
            NodeId::Space(id) => format!(
                "Delete “{}”? Its terminals are closed; the directory is not touched.",
                self.spaces.get(id).map(|s| s.name.as_str()).unwrap_or("?")
            ),
            NodeId::Server(id) => {
                format!("Delete server “{}”?", self.store.server(id).map(|s| s.name.as_str()).unwrap_or("?"))
            }
            NodeId::Folder(id) => format!(
                "Delete folder “{}” and everything in it?",
                self.store.folder(id).map(|f| f.name.as_str()).unwrap_or("?")
            ),
        };
        let action = match node {
            NodeId::Space(id) => ConfirmAction::DeleteSpace(id),
            n => ConfirmAction::Delete(n),
        };
        self.modal = Some(Modal::Confirm(Confirm { text, action }));
    }

    // ------------------------------------------------------------ modales (teclado)

    fn modal_key(&mut self, key: KeyEvent) {
        let Some(modal) = self.modal.take() else { return };
        self.modal = match modal {
            Modal::SecretEdit(e) => self.secret_edit_key(e, key),
            Modal::Unlock(u) => self.unlock_key(u, key),
            Modal::Master(m) => self.master_key(m, key),
            Modal::Vault(v) => self.vault_key(v, key),
            Modal::Form(f) => self.form_key(f, key),
            Modal::Picker(p, f) => Self::picker_key(p, f, key),
            Modal::Prompt(p) => self.prompt_key(p, key),
            Modal::Confirm(c) => self.confirm_key(c, key),
        };
    }

    fn form_key(&mut self, mut f: Box<ServerForm>, key: KeyEvent) -> Option<Modal> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => return None,
            KeyCode::Char('s') if ctrl => return self.save_form(f),
            KeyCode::Char('x') if ctrl && f.focus == F_SECRET && f.secret_saved => f.clear_secret = !f.clear_secret,
            KeyCode::Char('x') if ctrl && f.focus == F_SUDO && f.sudo_saved => f.clear_sudo = !f.clear_sudo,
            KeyCode::Char('o') if ctrl && f.auth == Auth::Key => return Some(open_picker(f)),
            KeyCode::Tab | KeyCode::Down => f.step(1),
            KeyCode::BackTab | KeyCode::Up => f.step(-1),
            KeyCode::Enter => {
                if f.is_last() {
                    return self.save_form(f);
                }
                f.step(1);
            }
            KeyCode::Left if f.focus == F_JUMP => self.cycle_jump(&mut f, -1),
            KeyCode::Right | KeyCode::Char(' ') if f.focus == F_JUMP => self.cycle_jump(&mut f, 1),
            KeyCode::Left if f.focus == F_AUTH => f.auth = f.auth.prev(),
            KeyCode::Right | KeyCode::Char(' ') if f.focus == F_AUTH => f.auth = f.auth.next(),
            _ => {
                if let Some(i) = f.input_mut() {
                    i.handle(key);
                }
            }
        }
        Some(Modal::Form(f))
    }

    /// Cycle through the servers `f` may be routed through (never itself or a loop).
    fn cycle_jump(&self, f: &mut ServerForm, dir: i32) {
        let mut options: Vec<Option<u64>> = vec![None];
        options.extend(
            self.store
                .servers
                .iter()
                .filter(|s| f.editing.is_none_or(|id| !self.store.would_cycle(id, s.id)))
                .map(|s| Some(s.id)),
        );
        let i = options.iter().position(|o| *o == f.jump).unwrap_or(0) as i32;
        f.jump = options[(i + dir).rem_euclid(options.len() as i32) as usize];
    }

    fn picker_key(mut p: Box<Picker>, mut f: Box<ServerForm>, key: KeyEvent) -> Option<Modal> {
        match key.code {
            KeyCode::Esc => return Some(Modal::Form(f)),
            KeyCode::Up | KeyCode::Char('k') => p.mv(-1),
            KeyCode::Down | KeyCode::Char('j') => p.mv(1),
            KeyCode::PageUp => p.mv(-10),
            KeyCode::PageDown => p.mv(10),
            KeyCode::Home => p.selected = 0,
            KeyCode::End => p.selected = p.entries.len().saturating_sub(1),
            KeyCode::Backspace | KeyCode::Left => p.up(),
            KeyCode::Char('.') => {
                p.show_hidden = !p.show_hidden;
                p.reload();
            }
            KeyCode::Char('~') => {
                if let Some(h) = home_dir() {
                    p.go(h);
                }
            }
            KeyCode::Enter | KeyCode::Right => {
                if let Some(path) = p.activate() {
                    f.key = Input::new(&path.to_string_lossy());
                    return Some(Modal::Form(f));
                }
            }
            _ => {}
        }
        Some(Modal::Picker(p, f))
    }

    fn prompt_key(&mut self, mut p: Prompt, key: KeyEvent) -> Option<Modal> {
        match key.code {
            KeyCode::Esc => return None,
            KeyCode::Enter => {
                self.submit_prompt(&p);
                return None;
            }
            _ => p.input.handle(key),
        }
        Some(Modal::Prompt(p))
    }

    fn submit_prompt(&mut self, p: &Prompt) {
        let name = p.input.value.trim().to_string();
        if name.is_empty() {
            return;
        }
        match p.kind {
            PromptKind::RenameSpace(id) => {
                self.spaces.rename(id, name);
                self.save_spaces();
            }
            PromptKind::NewFolder(parent) => {
                let id = self.store.add_folder(name, parent);
                self.persist();
                self.rebuild();
                self.select_node(NodeId::Folder(id));
                self.focus = Focus::Sidebar;
            }
            PromptKind::RenameFolder(id) => {
                if let Some(f) = self.store.folders.iter_mut().find(|f| f.id == id) {
                    f.name = name;
                }
                self.persist();
            }
            PromptKind::RenameTab(i) => {
                if let Some(t) = self.tabs.get_mut(i) {
                    t.title = name;
                }
            }
        }
    }

    fn confirm_key(&mut self, c: Confirm, key: KeyEvent) -> Option<Modal> {
        match key.code {
            KeyCode::Char('y') | KeyCode::Char('s') | KeyCode::Enter => self.run_confirmed(c.action),
            KeyCode::Char('n') | KeyCode::Esc => match c.action {
                ConfirmAction::DeleteSecret(id, kind) => self.vault_view(id, if kind == Kind::Login { 0 } else { 1 }),
                _ => None,
            },
            _ => Some(Modal::Confirm(c)),
        }
    }

    fn run_confirmed(&mut self, action: ConfirmAction) -> Option<Modal> {
        match action {
            ConfirmAction::DeleteSecret(id, kind) => {
                let col = if kind == Kind::Login { 0 } else { 1 };
                match self.vault.set(id, kind, None) {
                    Ok(()) => {
                        self.set_secret_flag(id, kind, false);
                        self.set_flash("Removed");
                    }
                    Err(e) => self.set_flash(format!("Could not remove: {e:#}")),
                }
                self.vault_view(id, col)
            }
            ConfirmAction::Quit => {
                self.quit = true;
                None
            }
            ConfirmAction::DeleteSpace(id) => {
                let doomed: Vec<usize> =
                    self.tabs.iter().enumerate().filter(|(_, t)| t.scope == Scope::Space(id)).map(|(i, _)| i).collect();
                for i in doomed.into_iter().rev() {
                    self.close_tab(i);
                }
                self.remembered.remove(&Scope::Space(id));
                self.spaces.remove(id);
                self.save_spaces();
                self.rebuild();
                self.sync_scope();
                None
            }
            ConfirmAction::Delete(node) => {
                let with_secrets: Vec<u64> =
                    self.store.servers.iter().filter(|s| s.has_secret || s.has_sudo).map(|s| s.id).collect();
                let gone: Vec<u64> =
                    self.store.delete(node).into_iter().filter(|id| with_secrets.contains(id)).collect();
                self.persist();
                self.rebuild();
                self.gate(Pending::Purge(gone))
            }
        }
    }

    fn save_form(&mut self, mut f: Box<ServerForm>) -> Option<Modal> {
        let host = f.host.value.trim().to_string();
        if !f.port.value.trim().parse::<u16>().is_ok_and(|p| p > 0) {
            f.error = Some("Port must be a number between 1 and 65535".into());
            f.focus = F_PORT;
            return Some(Modal::Form(f));
        }
        if host.is_empty() {
            f.error = Some("Host is required".into());
            f.focus = F_HOST;
            return Some(Modal::Form(f));
        }
        let key_path = f.key.value.trim().to_string();
        if f.auth == Auth::Key && key_path.is_empty() {
            f.error = Some("Choose the private key (Ctrl+O or “Browse”)".into());
            f.focus = F_KEY;
            return Some(Modal::Form(f));
        }
        let prev = f.editing.and_then(|id| self.store.server(id)).cloned();
        let typed_secret = f.auth != Auth::Agent && !f.secret.value.is_empty();
        let drops_secret = f.auth == Auth::Agent && prev.as_ref().is_some_and(|p| p.has_secret);
        if typed_secret || !f.sudo.value.is_empty() || drops_secret || f.clear_secret || f.clear_sudo {
            return self.gate(Pending::SaveForm(f));
        }
        self.finish_save(f)
    }

    /// Writes the (already validated) form to the store, and its secrets to the vault.
    fn finish_save(&mut self, f: Box<ServerForm>) -> Option<Modal> {
        let host = f.host.value.trim().to_string();
        let port: u16 = f.port.value.trim().parse().unwrap_or(22);
        let key_path = f.key.value.trim().to_string();
        let name = match f.name.value.trim() {
            "" => host.clone(),
            n => n.to_string(),
        };
        let prev = f.editing.and_then(|id| self.store.server(id)).cloned();
        let typed_secret = f.auth != Auth::Agent && !f.secret.value.is_empty();
        let typed_sudo = !f.sudo.value.is_empty();
        let clear_secret = f.clear_secret && !typed_secret;
        let clear_sudo = f.clear_sudo && !typed_sudo;
        let had_secret = prev.as_ref().is_some_and(|p| p.has_secret);
        let server = Server {
            id: f.editing.unwrap_or(0),
            name,
            host,
            port,
            user: f.user.value.trim().to_string(),
            auth: f.auth,
            key_path: if f.auth == Auth::Key { key_path } else { String::new() },
            parent: f.parent,
            jump: f.jump,
            has_secret: f.auth != Auth::Agent && (typed_secret || (had_secret && !clear_secret)),
            has_sudo: typed_sudo || (prev.as_ref().is_some_and(|p| p.has_sudo) && !clear_sudo),
        };
        let id = match f.editing {
            Some(id) => {
                self.store.update_server(server);
                id
            }
            None => self.store.add_server(server),
        };
        let mut result = Ok(());
        if typed_secret {
            result = result.and(self.vault.set(id, Kind::Login, Some(&f.secret.value)));
        } else if (f.auth == Auth::Agent || clear_secret) && had_secret {
            result = result.and(self.vault.set(id, Kind::Login, None));
        }
        if typed_sudo {
            result = result.and(self.vault.set(id, Kind::Sudo, Some(&f.sudo.value)));
        } else if clear_sudo {
            result = result.and(self.vault.set(id, Kind::Sudo, None));
        }
        if let Err(e) = result {
            self.set_flash(format!("Could not save to the vault: {e:#}"));
        }
        self.persist();
        self.rebuild();
        self.select_node(NodeId::Server(id));
        None
    }

    pub fn on_paste(&mut self, text: &str) {
        match self.modal.as_mut() {
            Some(Modal::Form(f)) => {
                if let Some(i) = f.input_mut() {
                    i.insert_str(text);
                }
            }
            Some(Modal::Prompt(p)) => p.input.insert_str(text),
            Some(Modal::SecretEdit(e)) => e.input.insert_str(text),
            Some(Modal::Unlock(u)) => {
                let field = if u.focus == 0 { &mut u.input } else { &mut u.confirm };
                field.insert_str(text);
            }
            Some(Modal::Master(m)) => {
                let field = if m.focus == 0 { &mut m.input } else { &mut m.confirm };
                field.insert_str(text);
            }
            Some(_) => {}
            None => {
                if self.focus == Focus::Terminal {
                    if let Some(t) = self.tabs.get(self.active) {
                        if t.session.exit_code.is_none() {
                            if t.session.with_screen(|s| s.bracketed_paste()) {
                                t.session.write(format!("\x1b[200~{text}\x1b[201~").as_bytes());
                            } else {
                                t.session.write(text.as_bytes());
                            }
                        }
                    }
                }
            }
        }
    }

    // ------------------------------------------------------------ mouse

    fn is_double_click(&mut self, x: u16, y: u16) -> bool {
        let double =
            self.last_click.is_some_and(|(t, px, py)| t.elapsed() < Duration::from_millis(400) && px == x && py == y);
        self.last_click = if double { None } else { Some((Instant::now(), x, y)) };
        double
    }

    fn row_at(&self, x: u16, y: u16) -> Option<usize> {
        let l = self.layout.list;
        if inside(l, x, y) { Some(self.offset + ((y - l.y) / self.layout.row_h.max(1)) as usize) } else { None }
    }

    pub fn on_mouse(&mut self, ev: MouseEvent) {
        self.on_mouse_inner(ev);
        self.sync_scope();
    }

    fn on_mouse_inner(&mut self, ev: MouseEvent) {
        let (x, y) = (ev.column, ev.row);
        if self.modal.is_some() {
            return self.modal_mouse(ev);
        }
        match ev.kind {
            MouseEventKind::Down(MouseButton::Left) => self.mouse_down(x, y),
            MouseEventKind::Drag(MouseButton::Left) if self.selecting => self.drag_select(x, y),
            MouseEventKind::Drag(MouseButton::Left) => {
                self.drop_hover = match self.drag {
                    Some(Drag::Node(_)) => self.row_at(x, y).filter(|&i| i < self.rows.len()),
                    _ => None,
                };
            }
            MouseEventKind::Up(MouseButton::Left) => self.mouse_up(x, y),
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                let up = matches!(ev.kind, MouseEventKind::ScrollUp);
                if inside(self.layout.list, x, y) {
                    let max = self.rows.len().saturating_sub(1);
                    self.offset = if up { self.offset.saturating_sub(3) } else { (self.offset + 3).min(max) };
                } else if inside(self.layout.content, x, y) {
                    self.selection = None; // the text moves under it
                    if let Some(t) = self.tabs.get(self.active) {
                        t.session.scroll(if up { 3 } else { -3 });
                    }
                }
            }
            _ => {}
        }
    }

    fn mouse_down(&mut self, x: u16, y: u16) {
        let double = self.is_double_click(x, y);
        self.drag = None;
        self.header = None;
        self.selection = None;
        self.selecting = false;
        if let Some(&(_, hit)) = self.layout.toolbar.iter().find(|(r, _)| inside(*r, x, y)) {
            self.header = None;
            self.activate(hit);
            return;
        }
        if let Some(&(_, i)) = self.layout.agents.iter().find(|(r, _)| inside(*r, x, y)) {
            return self.focus_tab(i);
        }
        if inside(self.layout.sidebar, x, y) {
            self.focus = Focus::Sidebar;
            if let Some(i) = self.row_at(x, y).filter(|&i| i < self.rows.len()) {
                self.selected = i;
                let node = self.rows[i].node;
                if double {
                    match node {
                        NodeId::Folder(_) => self.toggle(node),
                        NodeId::Server(id) => self.open_server(id),
                        NodeId::Space(id) => self.open_space(id),
                    }
                } else {
                    self.drag = Some(Drag::Node(node));
                }
            }
        } else if inside(self.layout.tabbar, x, y) {
            let hit = self.layout.tabs.iter().find(|t| inside(t.rect, x, y)).map(|t| (t.idx, t.close));
            if let Some((i, close)) = hit {
                if inside(close, x, y) {
                    return self.close_tab(i);
                }
                self.active = i;
                self.focus = Focus::Terminal;
                if double {
                    self.rename_tab_prompt();
                } else {
                    self.drag = Some(Drag::Tab(i));
                }
            }
        } else if inside(self.layout.content, x, y) && !self.scoped().is_empty() {
            self.focus = Focus::Terminal;
            let cell = self.content_cell(x, y);
            if double {
                self.select_word(cell);
            } else {
                self.selection = Some(Selection { anchor: cell, head: cell });
                self.selecting = true;
            }
        }
    }

    /// The cell of the terminal area under (x, y), clamped to the area.
    fn content_cell(&self, x: u16, y: u16) -> (u16, u16) {
        let c = self.layout.content;
        let row = y.clamp(c.y, (c.y + c.height).saturating_sub(1)) - c.y;
        let col = x.clamp(c.x, (c.x + c.width).saturating_sub(1)) - c.x;
        (row, col)
    }

    fn scroll_active(&self, delta: i32) {
        if let Some(t) = self.tabs.get(self.active) {
            t.session.scroll(delta);
        }
    }

    /// Extends the selection to the pointer; dragging past the top or bottom edge scrolls the history.
    fn drag_select(&mut self, x: u16, y: u16) {
        let c = self.layout.content;
        let mut cell = self.content_cell(x, y);
        let last = c.height.saturating_sub(1);
        if y < c.y {
            self.scroll_active(1);
            // The text moved down with the scroll; the anchor stays on it.
            if let Some(sel) = self.selection.as_mut() {
                sel.anchor.0 = (sel.anchor.0 + 1).min(last);
            }
            cell.0 = 0;
        } else if y >= c.y + c.height {
            self.scroll_active(-1);
            if let Some(sel) = self.selection.as_mut() {
                sel.anchor.0 = sel.anchor.0.saturating_sub(1);
            }
            cell.0 = last;
        }
        if let Some(sel) = self.selection.as_mut() {
            sel.head = cell;
        }
    }

    /// Double click: selects the run of non-blank characters under the pointer (a word, a path, a URL) and copies it.
    fn select_word(&mut self, (row, col): (u16, u16)) {
        let Some(tab) = self.tabs.get(self.active) else { return };
        let cols = tab.session.size().1;
        let line: Vec<char> = tab.session.with_screen(|s| s.contents_between(row, 0, row, cols)).chars().collect();
        let c = col as usize;
        if c >= line.len() || line[c].is_whitespace() {
            return;
        }
        let mut a = c;
        while a > 0 && !line[a - 1].is_whitespace() {
            a -= 1;
        }
        let mut b = c;
        while b + 1 < line.len() && !line[b + 1].is_whitespace() {
            b += 1;
        }
        self.selection = Some(Selection { anchor: (row, a as u16), head: (row, b as u16) });
        self.copy_selection();
    }

    /// Puts the selected text on the clipboard.
    fn copy_selection(&mut self) {
        let Some(sel) = self.selection else { return };
        let ((r1, c1), (r2, c2)) = sel.ordered();
        let Some(tab) = self.tabs.get(self.active) else { return };
        let cols = tab.session.size().1;
        let text = tab.session.with_screen(|s| s.contents_between(r1, c1, r2, c2.saturating_add(1).min(cols)));
        let text = text.lines().map(str::trim_end).collect::<Vec<_>>().join("\n");
        if text.trim().is_empty() {
            return;
        }
        clipboard::copy(&text);
        self.set_flash(format!("Copied {} characters", text.chars().count()));
    }

    fn mouse_up(&mut self, x: u16, y: u16) {
        self.drop_hover = None;
        if std::mem::take(&mut self.selecting) {
            match self.selection {
                Some(s) if s.anchor != s.head => self.copy_selection(),
                _ => self.selection = None, // a plain click: just focus
            }
            return;
        }
        match self.drag.take() {
            Some(Drag::Tab(from)) => {
                if let Some(to) = self.layout.tabs.iter().find(|t| inside(t.rect, x, y)).map(|t| t.idx) {
                    self.move_tab(from, to);
                }
            }
            Some(Drag::Node(node)) => self.drop_node(node, x, y),
            None => {}
        }
    }

    fn drop_node(&mut self, node: NodeId, x: u16, y: u16) {
        if !inside(self.layout.sidebar, x, y) {
            return;
        }
        if self.view == View::Spaces {
            return;
        }
        if self.view == View::Jump {
            let NodeId::Server(id) = node else { return };
            let target = self.row_at(x, y).and_then(|i| self.rows.get(i)).map(|r| r.node);
            let via = match target {
                Some(NodeId::Server(s)) if s == id => return,
                Some(NodeId::Server(s)) => Some(s),
                _ => None,
            };
            if self.store.move_via(id, via) {
                if let Some(v) = via {
                    self.jump_collapsed.remove(&v);
                }
                self.persist();
                self.rebuild();
                self.select_node(node);
            } else {
                self.set_flash("Invalid move: that would create a loop");
            }
            return;
        }
        let moved = match self.row_at(x, y).and_then(|i| self.rows.get(i)).map(|r| r.node) {
            Some(target) if target == node => return,
            Some(NodeId::Space(_)) => false,
            Some(NodeId::Folder(f)) => self.store.move_into(node, Some(f)),
            Some(NodeId::Server(s)) => match node {
                NodeId::Server(id) => self.store.move_server_before(id, s),
                NodeId::Folder(_) => {
                    let parent = self.store.server(s).and_then(|s| s.parent);
                    self.store.move_into(node, parent)
                }
                NodeId::Space(_) => false,
            },
            None if inside(self.layout.list, x, y) || y >= self.layout.list.y => self.store.move_into(node, None),
            None => false,
        };
        if moved {
            self.persist();
            self.rebuild();
            self.select_node(node);
        } else {
            self.set_flash("Invalid move");
        }
    }

    fn modal_mouse(&mut self, ev: MouseEvent) {
        let (x, y) = (ev.column, ev.row);
        // Later entries (buttons) sit on top of earlier ones (field rows).
        let hit = self.layout.modal.iter().rev().find(|(r, _)| inside(*r, x, y)).map(|&(_, h)| h);
        let Some(modal) = self.modal.take() else { return };
        let double = matches!(ev.kind, MouseEventKind::Down(MouseButton::Left)) && self.is_double_click(x, y);
        let down = matches!(ev.kind, MouseEventKind::Down(MouseButton::Left));
        self.modal = match modal {
            Modal::Form(mut f) if down => match hit {
                Some(Hit::Field(i)) => {
                    f.focus = i;
                    Some(Modal::Form(f))
                }
                Some(Hit::Browse) => Some(open_picker(f)),
                Some(Hit::Save) => self.save_form(f),
                Some(Hit::Cancel) => None,
                _ => Some(Modal::Form(f)),
            },
            Modal::Picker(mut p, f) => {
                let l = self.layout.picker_list;
                match ev.kind {
                    MouseEventKind::ScrollUp => p.mv(-3),
                    MouseEventKind::ScrollDown => p.mv(3),
                    MouseEventKind::Down(MouseButton::Left) if inside(l, x, y) => {
                        let i = p.offset + (y - l.y) as usize;
                        if i < p.entries.len() {
                            p.selected = i;
                            if double {
                                return self.finish_picker_activate(p, f);
                            }
                        }
                    }
                    _ => {}
                }
                Some(Modal::Picker(p, f))
            }
            Modal::Confirm(c) if down => match hit {
                Some(Hit::Yes) => self.run_confirmed(c.action),
                Some(Hit::No) => match c.action {
                    ConfirmAction::DeleteSecret(id, kind) => {
                        self.vault_view(id, if kind == Kind::Login { 0 } else { 1 })
                    }
                    _ => None,
                },
                _ => Some(Modal::Confirm(c)),
            },
            other => Some(other),
        };
    }

    fn finish_picker_activate(&mut self, mut p: Box<Picker>, mut f: Box<ServerForm>) {
        if let Some(path) = p.activate() {
            f.key = Input::new(&path.to_string_lossy());
            self.modal = Some(Modal::Form(f));
        } else {
            self.modal = Some(Modal::Picker(p, f));
        }
    }
}

// ---------------------------------------------------------------- utilidades

fn home_dir() -> Option<PathBuf> {
    directories::BaseDirs::new().map(|b| b.home_dir().to_path_buf())
}

fn open_picker(f: Box<ServerForm>) -> Modal {
    let current = PathBuf::from(expand_tilde(f.key.value.trim()));
    let start = current
        .parent()
        .filter(|p| !f.key.value.trim().is_empty() && p.is_dir())
        .map(Path::to_path_buf)
        .or_else(|| home_dir().map(|h| h.join(".ssh")).filter(|p| p.is_dir()))
        .or_else(home_dir)
        .unwrap_or_else(|| PathBuf::from("/"));
    Modal::Picker(Box::new(Picker::new(start)), f)
}

pub fn expand_tilde(p: &str) -> String {
    if let Some(rest) = p.strip_prefix("~/") {
        if let Some(h) = home_dir() {
            return h.join(rest).to_string_lossy().to_string();
        }
    }
    p.to_string()
}

/// Name a space gets from its directory: the folder name, or `home` for the home directory.
fn space_name(dir: &Path) -> String {
    if home_dir().is_some_and(|h| h == dir) { "home".into() } else { spaces::default_name(dir) }
}

/// The user's shell, for the local terminal tab.
fn local_shell() -> Vec<String> {
    if cfg!(windows) {
        return vec!["powershell.exe".into(), "-NoLogo".into()];
    }
    vec![std::env::var("SHELL").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "/bin/sh".into())]
}

fn target(s: &Server) -> String {
    if s.user.is_empty() { s.host.clone() } else { format!("{}@{}", s.user, s.host) }
}

/// Quotes `s` for the shell that runs a `ProxyCommand`: `sh` on Unix, `cmd.exe` on Windows.
/// Plain words are left as they are.
fn sh_quote(s: &str) -> String {
    let plain = |c: char| c.is_ascii_alphanumeric() || "/._-:=@%,+".contains(c) || (cfg!(windows) && c == '\\');
    if !s.is_empty() && s.chars().all(plain) {
        return s.to_string();
    }
    if cfg!(windows) {
        format!("\"{}\"", s.replace('"', "\\\""))
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

/// Options shared by direct connections and by the hops of a ProxyCommand.
fn auth_args(s: &Server) -> Vec<String> {
    match s.auth {
        Auth::Agent => vec![],
        Auth::Key => vec!["-i".into(), expand_tilde(&s.key_path), "-o".into(), "IdentitiesOnly=yes".into()],
        Auth::Password => vec![
            "-o".into(),
            "PreferredAuthentications=password,keyboard-interactive".into(),
            "-o".into(),
            "PubkeyAuthentication=no".into(),
        ],
    }
}

/// `ssh` command that tunnels through `chain` (outermost first), as a single shell string.
fn proxy_command(chain: &[&Server]) -> String {
    let Some((hop, before)) = chain.split_last() else { return String::new() };
    let mut parts = vec!["ssh".to_string()];
    parts.extend(auth_args(hop).into_iter().map(|a| sh_quote(&a)));
    parts.extend(["-p".into(), hop.port.to_string()]);
    if !before.is_empty() {
        parts.extend(["-o".into(), sh_quote(&format!("ProxyCommand={}", proxy_command(before)))]);
    }
    parts.extend(["-W".into(), "%h:%p".into(), target(hop)]);
    parts.join(" ")
}

/// Arguments for the system `ssh` client, including any jump hosts.
pub fn ssh_argv(store: &Store, s: &Server) -> Vec<String> {
    let mut a: Vec<String> = vec!["ssh".into(), "-p".into(), s.port.to_string()];
    a.extend(["-o".into(), "ServerAliveInterval=30".into(), "-o".into(), "ConnectTimeout=15".into()]);
    a.extend(auth_args(s));
    let chain = store.jump_chain(s.id);
    if !chain.is_empty() {
        if chain.iter().all(|h| h.auth != Auth::Key) {
            let hops: Vec<String> = chain
                .iter()
                .map(|h| if h.port == 22 { target(h) } else { format!("{}:{}", target(h), h.port) })
                .collect();
            a.extend(["-J".into(), hops.join(",")]);
        } else {
            // -J cannot give each hop its own key, so build the tunnel by hand.
            a.extend(["-o".into(), format!("ProxyCommand={}", proxy_command(&chain))]);
        }
    }
    a.push(target(s));
    a
}

#[cfg(test)]
mod tests {
    use super::*;

    fn server(auth: Auth, key: &str, user: &str) -> Server {
        Server {
            id: 1,
            name: "n".into(),
            host: "h.example".into(),
            port: 2222,
            user: user.into(),
            auth,
            key_path: key.into(),
            parent: None,
            jump: None,
            has_secret: false,
            has_sudo: false,
        }
    }

    #[test]
    fn argv_for_key_auth() {
        let a = ssh_argv(&Store::default(), &server(Auth::Key, "/k/id", "root"));
        assert_eq!(a.last().unwrap(), "root@h.example");
        assert!(a.windows(2).any(|w| w == ["-i", "/k/id"]));
        assert!(a.windows(2).any(|w| w == ["-p", "2222"]));
    }

    #[test]
    fn argv_without_user_uses_host_only() {
        assert_eq!(ssh_argv(&Store::default(), &server(Auth::Agent, "", "")).last().unwrap(), "h.example");
    }

    fn chain_store(hop_auth: Auth) -> (Store, Server) {
        let mut st = Store::default();
        let mut bastion = server(hop_auth, "/k/bast", "ops");
        bastion.host = "bastion.example".into();
        bastion.port = 22;
        let b = st.add_server(bastion);
        let mut inner = server(Auth::Agent, "", "app");
        inner.jump = Some(b);
        let id = st.add_server(inner);
        let target = st.server(id).unwrap().clone();
        (st, target)
    }

    #[test]
    fn argv_uses_dash_j_for_agent_hops() {
        let (st, t) = chain_store(Auth::Agent);
        let a = ssh_argv(&st, &t);
        assert!(a.windows(2).any(|w| w == ["-J", "ops@bastion.example"]), "{a:?}");
    }

    #[test]
    fn quotes_only_what_needs_quoting() {
        assert_eq!(sh_quote("-W"), "-W");
        assert_eq!(sh_quote("%h:%p"), "%h:%p");
        let q = sh_quote("/k/my key");
        assert!(q == "'/k/my key'" || q == "\"/k/my key\"", "{q}");
    }

    #[test]
    fn proxy_command_quotes_key_paths_with_spaces() {
        let mut st = Store::default();
        let mut bastion = server(Auth::Key, "/k/my key", "ops");
        bastion.host = "bastion.example".into();
        let b = st.add_server(bastion);
        let mut inner = server(Auth::Agent, "", "app");
        inner.jump = Some(b);
        let id = st.add_server(inner);
        let a = ssh_argv(&st, &st.server(id).unwrap().clone());
        let pc = a.iter().find(|x| x.starts_with("ProxyCommand=")).unwrap();
        assert!(pc.contains("-i '/k/my key'") || pc.contains("-i \"/k/my key\""), "{pc}");
    }

    #[test]
    fn argv_uses_proxy_command_when_a_hop_has_a_key() {
        let (st, t) = chain_store(Auth::Key);
        let a = ssh_argv(&st, &t);
        let pc = a.iter().find(|x| x.starts_with("ProxyCommand=")).expect("ProxyCommand");
        assert!(pc.contains("-i /k/bast") && pc.contains("-W %h:%p ops@bastion.example"), "{pc}");
    }

    fn app_with_servers(n: usize) -> App {
        let mut st = Store::default();
        for i in 0..n {
            st.add_server(Server { name: format!("s{i}"), host: format!("h{i}"), ..server(Auth::Agent, "", "u") });
        }
        let vault = Vault::new(std::env::temp_dir().join(format!("ship-test-vault-{}", std::process::id())));
        let mut app = App::new(st, vault);
        app.rebuild();
        app
    }

    fn press(app: &mut App, code: KeyCode) {
        app.sidebar_key(KeyEvent::new(code, KeyModifiers::NONE));
    }

    #[test]
    fn up_from_the_first_row_reaches_the_views_and_down_returns() {
        let mut app = app_with_servers(2);
        press(&mut app, KeyCode::Down);
        assert_eq!((app.selected, app.header), (1, None));
        press(&mut app, KeyCode::Up);
        press(&mut app, KeyCode::Up);
        assert_eq!((app.header, app.view), (Some(4), View::Folders), "the views row (SSH) is the first stop");
        press(&mut app, KeyCode::Up);
        assert_eq!(app.header, Some(4), "nothing above the views");
        press(&mut app, KeyCode::Down);
        assert_eq!(app.header, None, "back on the list");
    }

    /// The arrows alone switch view: no Enter needed.
    #[test]
    fn arrows_on_the_views_switch_view_without_enter() {
        let mut app = app_with_servers(1);
        press(&mut app, KeyCode::Up);
        assert_eq!((app.header, app.view), (Some(4), View::Folders));
        press(&mut app, KeyCode::Left);
        assert_eq!((app.header, app.view), (Some(3), View::Spaces), "Agents is to the left of SSH");
        press(&mut app, KeyCode::Left);
        assert_eq!((app.header, app.view), (Some(3), View::Spaces), "stops at the first");
        press(&mut app, KeyCode::Right);
        press(&mut app, KeyCode::Right);
        assert_eq!((app.header, app.view), (Some(5), View::Jump), "Bastions at the end");
        press(&mut app, KeyCode::Right);
        assert_eq!((app.header, app.view), (Some(5), View::Jump));
        press(&mut app, KeyCode::Left);
        assert_eq!(app.view, View::Folders, "and back");
    }

    #[test]
    fn down_from_the_last_row_reaches_the_buttons_below_the_list() {
        let mut app = app_with_servers(2);
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Down);
        assert_eq!(app.header, Some(0), "the first button: + Server");
        press(&mut app, KeyCode::Right);
        press(&mut app, KeyCode::Right);
        press(&mut app, KeyCode::Right);
        assert_eq!(app.header, Some(2), "Edit is the last button");
        press(&mut app, KeyCode::Down);
        assert_eq!(app.header, Some(2), "nothing below the buttons");
        press(&mut app, KeyCode::Up);
        assert_eq!((app.header, app.selected), (None, 1), "Up returns to the last row");
    }

    #[test]
    fn enter_on_a_button_acts_and_leaves_the_menu() {
        let mut app = app_with_servers(1);
        press(&mut app, KeyCode::Down);
        assert_eq!(app.header, Some(0));
        press(&mut app, KeyCode::Enter);
        assert!(matches!(app.modal, Some(Modal::Form(_))), "+ Server opens the form");
        assert_eq!(app.header, None);
    }

    #[test]
    #[cfg(unix)]
    fn the_t_key_opens_a_local_terminal() {
        let mut app = app_with_servers(1);
        press(&mut app, KeyCode::Char('t'));
        assert_eq!(app.tabs.len(), 1);
        assert_eq!((app.tabs[0].server_id, app.tabs[0].title.as_str()), (LOCAL, "Local"));
        assert!(app.tabs[0].session.exit_code.is_none());
    }

    #[test]
    #[cfg(unix)]
    fn number_keys_jump_to_tabs_from_the_sidebar() {
        let mut app = app_with_servers(1);
        app.open_server(LOCAL);
        app.open_server(LOCAL);
        app.focus = Focus::Sidebar;
        press(&mut app, KeyCode::Char('1'));
        assert_eq!((app.active, app.focus), (0, Focus::Terminal));
        app.focus = Focus::Sidebar;
        press(&mut app, KeyCode::Char('2'));
        assert_eq!(app.active, 1);
        app.focus = Focus::Sidebar;
        press(&mut app, KeyCode::Char('5'));
        assert_eq!((app.active, app.focus), (1, Focus::Sidebar), "no such tab: nothing changes");
    }

    #[cfg(unix)]
    fn temp_dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ship-app-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d.canonicalize().unwrap()
    }

    #[cfg(unix)]
    fn wait_for_screen(app: &App, tab: usize, want: &str) -> bool {
        let end = Instant::now() + Duration::from_secs(4);
        while Instant::now() < end {
            if app.tabs[tab].session.with_screen(|s| s.contents()).contains(want) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(30));
        }
        false
    }

    #[test]
    #[cfg(unix)]
    fn a_space_terminal_starts_in_its_directory_and_stays_out_of_the_ssh_tabs() {
        let dir = temp_dir("space");
        let mut app = app_with_servers(1);
        let id = app.spaces.add("proj".into(), dir.display().to_string());
        app.rebuild();
        app.set_view(View::Spaces);
        assert_eq!(app.scope, Scope::Space(id));
        app.open_space(id);
        assert_eq!(app.tabs.len(), 1);
        assert_eq!(app.tabs[0].scope, Scope::Space(id));
        app.tabs[0].session.write(b"pwd\r");
        assert!(wait_for_screen(&app, 0, dir.file_name().unwrap().to_str().unwrap()), "the shell starts in the space directory");

        app.set_view(View::Folders);
        assert_eq!(app.scope, Scope::Ssh);
        assert!(app.scoped().is_empty(), "nothing from the space shows among the SSH tabs");
        assert_eq!(app.focus, Focus::Sidebar);
        app.open_server(LOCAL);
        assert_eq!(app.tabs[1].scope, Scope::Ssh, "a local terminal outside the spaces belongs to the SSH section");
        assert_eq!(app.scoped(), vec![1]);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    #[cfg(unix)]
    fn each_space_remembers_its_own_active_tab() {
        let (d1, d2) = (temp_dir("a"), temp_dir("b"));
        let mut app = app_with_servers(0);
        let a = app.spaces.add("a".into(), d1.display().to_string());
        let b = app.spaces.add("b".into(), d2.display().to_string());
        app.rebuild();
        app.set_view(View::Spaces);
        app.open_space(a);
        app.open_server(LOCAL); // second tab in a
        app.goto_tab(0);
        assert_eq!(app.active, 0);
        app.open_space(b);
        assert_eq!(app.scope, Scope::Space(b));
        assert_eq!(app.scoped(), vec![2], "b has its own single tab");
        app.select_node(NodeId::Space(a));
        app.sync_scope();
        assert_eq!((app.scope, app.active), (Scope::Space(a), 0), "back in a, on the tab it was left on");
        std::fs::remove_dir_all(d1).ok();
        std::fs::remove_dir_all(d2).ok();
    }

    #[test]
    #[cfg(unix)]
    fn deleting_a_space_closes_its_terminals() {
        let dir = temp_dir("del");
        let mut app = app_with_servers(0);
        let id = app.spaces.add("gone".into(), dir.display().to_string());
        app.rebuild();
        app.set_view(View::Spaces);
        app.open_space(id);
        app.open_server(LOCAL);
        assert_eq!(app.tabs.len(), 2);
        app.run_confirmed(ConfirmAction::DeleteSpace(id));
        assert!(app.tabs.is_empty() && app.spaces.spaces.is_empty() && app.rows.is_empty());
        assert_eq!(app.focus, Focus::Sidebar);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    #[cfg(unix)]
    fn focusing_an_agent_tab_switches_to_its_space() {
        let dir = temp_dir("agent");
        let mut app = app_with_servers(1);
        let id = app.spaces.add("proj".into(), dir.display().to_string());
        app.rebuild();
        app.set_view(View::Spaces);
        app.open_space(id);
        app.set_view(View::Folders);
        assert!(app.scoped().is_empty());
        app.focus_tab(0);
        assert_eq!((app.view, app.scope, app.focus), (View::Spaces, Scope::Space(id), Focus::Terminal));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn line_editing_shortcuts() {
        let ctrl = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL);
        let mut i = Input::new("one two  three");
        i.handle(ctrl('w'));
        assert_eq!(i.value, "one two  ");
        i.handle(ctrl('w'));
        assert_eq!(i.value, "one ");
        i.handle(ctrl('a'));
        i.handle(KeyEvent::new(KeyCode::Char('>'), KeyModifiers::NONE));
        assert_eq!(i.value, ">one ");
        i.handle(ctrl('e'));
        i.handle(ctrl('u'));
        assert_eq!(i.value, "");
    }

    /// An app whose only tab shows `text` (one line per entry) in a terminal area at (32, 1), 80x24.
    #[cfg(unix)]
    fn app_showing(lines: &[&str]) -> App {
        let mut app = app_with_servers(0);
        let script = format!("printf '%s\\n' {}; sleep 8", lines.iter().map(|l| format!("'{l}'")).collect::<Vec<_>>().join(" "));
        let argv = vec!["sh".into(), "-c".into(), script];
        let session = Session::spawn(&argv, 24, 80, vec![], false).unwrap();
        app.next_tab_id += 1;
        app.tabs.push(Tab {
            id: app.next_tab_id,
            title: "t".into(),
            server_id: LOCAL,
            scope: Scope::Ssh,
            session,
            attention: false,
            meta_sent: String::new(),
        });
        app.layout.content = Rect::new(32, 1, 80, 24);
        let last = lines.last().unwrap();
        assert!(wait_for_screen(&app, 0, last));
        app
    }

    #[cfg(unix)]
    fn copied() -> String {
        crate::clipboard::LAST.with(|l| l.borrow().clone())
    }

    #[test]
    #[cfg(unix)]
    fn dragging_over_the_terminal_selects_and_copies() {
        let mut app = app_showing(&["alpha beta gamma"]);
        app.mouse_down(32 + 6, 1);
        assert!(app.selecting);
        app.drag_select(32 + 9, 1);
        app.mouse_up(32 + 9, 1);
        assert_eq!(copied(), "beta");
        assert!(app.selection.is_some(), "the highlight stays until the next click or key");
        app.on_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
        assert!(app.selection.is_none(), "typing clears it");
    }

    #[test]
    #[cfg(unix)]
    fn a_selection_can_span_lines_and_be_dragged_backwards() {
        let mut app = app_showing(&["one", "two"]);
        app.mouse_down(32 + 1, 1 + 1); // second line, column 1
        app.drag_select(32 + 1, 1); // up to the first line: selection made backwards
        app.mouse_up(32 + 1, 1);
        assert_eq!(copied(), "ne\ntw");
    }

    #[test]
    #[cfg(unix)]
    fn double_click_copies_the_word_and_a_plain_click_copies_nothing() {
        let mut app = app_showing(&["see /etc/ssh/ssh_config now"]);
        app.select_word((0, 8));
        assert_eq!(copied(), "/etc/ssh/ssh_config", "a path counts as one word");
        let before = copied();
        app.mouse_down(32 + 2, 1);
        app.mouse_up(32 + 2, 1);
        assert_eq!(copied(), before, "a click without dragging copies nothing");
        assert!(app.selection.is_none());
    }

    #[test]
    #[cfg(unix)]
    fn a_opens_a_terminal_right_away_without_asking_for_a_path() {
        let mut app = app_with_servers(0);
        app.set_view(View::Spaces);
        press(&mut app, KeyCode::Char('a'));
        assert!(app.modal.is_none(), "no prompt");
        assert_eq!(app.spaces.spaces.len(), 1);
        let id = app.spaces.spaces[0].id;
        assert_eq!((app.scope, app.focus), (Scope::Space(id), Focus::Terminal));
        assert_eq!(app.scoped().len(), 1, "a terminal is already open in it");
    }

    #[test]
    #[cfg(unix)]
    fn a_space_stays_where_its_terminal_is_left() {
        let dir = temp_dir("follow");
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        let mut app = app_with_servers(0);
        let id = app.spaces.add("proj".into(), dir.display().to_string());
        app.rebuild();
        app.set_view(View::Spaces);
        app.open_space(id);
        assert_eq!(app.spaces.get(id).unwrap().name, "proj");

        let wait_for_dir = |app: &mut App, want: &Path| {
            let end = Instant::now() + Duration::from_secs(6);
            while Instant::now() < end {
                app.tick();
                if Path::new(&app.spaces.get(id).unwrap().cwd) == want {
                    return true;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            false
        };
        app.tabs[0].session.write(b"cd sub\r");
        assert!(wait_for_dir(&mut app, &dir.join("sub")), "the space follows `cd`");
        assert_eq!(app.spaces.get(id).unwrap().name, "sub", "and its automatic name follows too");

        app.spaces.rename(id, "mine".into());
        app.tabs[0].session.write(b"cd ..\r");
        assert!(wait_for_dir(&mut app, &dir));
        assert_eq!(app.spaces.get(id).unwrap().name, "mine", "a name chosen by the user is kept");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn a_click_anywhere_in_a_tall_block_hits_its_row() {
        let mut app = app_with_servers(3);
        app.layout.list = Rect::new(1, 10, 30, 30);
        app.layout.row_h = 3; // spaces are three-line blocks (two lines and a gap)
        for (y, row) in [(10, 0), (11, 0), (12, 0), (13, 1), (15, 1), (16, 2)] {
            assert_eq!(app.row_at(5, y), Some(row), "line {y}");
        }
        app.offset = 2;
        assert_eq!(app.row_at(5, 10), Some(2), "rows are counted from the scroll offset");
        app.layout.row_h = 1;
        assert_eq!(app.row_at(5, 12), Some(4), "one-line rows as before");
    }

    #[test]
    fn input_edits_unicode() {
        let mut i = Input::new("añb");
        i.handle(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        i.handle(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        assert_eq!(i.value, "ab");
        assert_eq!(i.cursor, 1);
    }
}
