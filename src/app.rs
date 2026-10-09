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
    /// The row sits in the bastions section (the jump tree), not in the folder tree.
    pub jump: bool,
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
    /// Shown in the right group of tabs (see `App::is_split`).
    pub right: bool,
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
    NewSpaceFolder,
    RenameSpaceFolder(u64),
    MoveSpace(u64),
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

/// What the quick open box can open.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SearchTarget {
    Server(u64),
    Space(u64),
    Local,
}

pub struct SearchItem {
    pub label: String,
    pub detail: String,
    pub target: SearchTarget,
}

/// The quick open box: type a few letters of a server or project and Enter opens it.
pub struct SearchModal {
    pub input: Input,
    pub items: Vec<SearchItem>,
    /// Indices into `items` of what matches the input, best first.
    pub hits: Vec<usize>,
    pub selected: usize,
}

impl SearchModal {
    fn new(items: Vec<SearchItem>) -> Self {
        let mut s = SearchModal { input: Input::new(""), items, hits: vec![], selected: 0 };
        s.refilter();
        s
    }

    /// Every word typed must appear in the name or the detail (any case); names that start with the first
    /// word come first.
    fn refilter(&mut self) {
        let q = self.input.value.to_lowercase();
        let words: Vec<&str> = q.split_whitespace().collect();
        let mut hits: Vec<usize> = (0..self.items.len())
            .filter(|&i| {
                let text = format!("{} {}", self.items[i].label, self.items[i].detail).to_lowercase();
                words.iter().all(|w| text.contains(w))
            })
            .collect();
        let first = words.first().copied().unwrap_or("");
        hits.sort_by_key(|&i| (!self.items[i].label.to_lowercase().starts_with(first), self.items[i].label.len()));
        self.hits = hits;
        self.selected = 0;
    }

    fn target(&self) -> Option<SearchTarget> {
        self.hits.get(self.selected).map(|&i| self.items[i].target)
    }
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
    Search(Box<SearchModal>),
}

// ---------------------------------------------------------------- layout (lo rellena ui.rs al dibujar)

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Hit {
    ViewFolders,
    NewProject,
    NewServer,
    Edit,
    Term,
    Vault,
    /// Folds a column of the sidebar away (0: Projects, 1: Servers) or brings it back.
    Fold(usize),
    ViewSpaces,
    NewTab,
    NewTabRight,
    Field(usize),
    Browse,
    Save,
    Cancel,
    Yes,
    No,
}

/// The titles of the two columns, in keyboard order: Projects, Servers. The sidebar pushes its toolbar hits in
/// this same order.
pub const HEADER: [Hit; 2] = [Hit::ViewSpaces, Hit::ViewFolders];

/// `Tab::server_id` of the terminal of this computer (real servers start at 1).
pub const LOCAL: u64 = 0;

#[derive(Clone, Copy)]
pub struct TabHit {
    /// Index in `App::tabs` (the bar can be scrolled, so it is not the position in the hit list).
    pub idx: usize,
    pub rect: Rect,
    pub close: Rect,
}

/// How a list is laid out in its column: the top part, the part below it (the folders or the bastions) and
/// the height in screen lines of a row of each.
#[derive(Clone, Copy, Default)]
pub struct ListGeom {
    pub top: Rect,
    pub bottom: Rect,
    /// Index in the rows where the bottom part starts.
    pub split: Option<usize>,
    pub row_h: u16,
    pub bottom_row_h: u16,
    pub top_count: usize,
}

#[derive(Default)]
pub struct Layout {
    pub sidebar: Rect,
    /// The list, or its upper half when the SSH view is split in two.
    pub list: Rect,
    /// The lower half (the bastions); empty when the list is not split.
    pub list_bottom: Rect,
    /// Height in screen lines of one row of the list (spaces are taller blocks).
    pub row_h: u16,
    pub tabbar: Rect,
    /// The terminal area of the focused group.
    pub content: Rect,
    /// The terminal areas of the left and right groups (the right one is empty when there is no split).
    pub panes: [Rect; 2],
    /// The draggable line between the groups, and the whole area they share.
    pub divider: Rect,
    pub split_area: Rect,
    /// The two columns of the sidebar (0: Projects, 1: Servers), whole boxes.
    pub cols: [Rect; 2],
    /// How the list of each column is laid out (where the one that is not the active view was drawn).
    pub col_geom: [ListGeom; 2],
    pub tabs: Vec<TabHit>,
    pub toolbar: Vec<(Rect, Hit)>,
    pub modal: Vec<(Rect, Hit)>,
    pub picker_list: Rect,
}

fn inside(r: Rect, x: u16, y: u16) -> bool {
    x >= r.x && x < r.x + r.width && y >= r.y && y < r.y + r.height
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum View {
    /// Shown as “SSH”: the folder tree, and below it the bastions.
    Folders,
    /// Shown as “Agents”.
    Spaces,
}

enum Drag {
    /// A row of the list and whether it was in the bastions section.
    Node(NodeId, bool),
    Tab(usize),
    /// The line between the two groups of tabs.
    Divider,
}

// ---------------------------------------------------------------- app

pub struct App {
    pub store: Store,
    pub vault: Vault,
    clip_clear_at: Option<Instant>,
    pub rows: Vec<Row>,
    pub selected: usize,
    pub offset: usize,
    /// Scroll of the lower half (the bastions) of the SSH view.
    pub offset_bottom: usize,
    pub tabs: Vec<Tab>,
    /// Index in `tabs` of the active tab of the current scope (meaningless while the scope has no tabs).
    pub active: usize,
    pub scope: Scope,
    /// The last active tab (by id) of each scope, to come back to it.
    remembered: HashMap<Scope, u64>,
    /// The scopes whose last tab was closed: each shows the welcome screen instead of another scope's tab.
    emptied: HashSet<Scope>,
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
    /// Which columns of the sidebar are folded away (0: Projects, 1: Servers); `Alt+B` folds or brings back both.
    pub fold: [bool; 2],
    /// The rows of the view that is not the active one, shown in its column.
    pub other_rows: Vec<Row>,
    /// Keyboard focus on the sidebar header instead of the list: an index into `HEADER`.
    pub header: Option<usize>,
    /// The tab bar, as tab ids: every open tab, in the order it was opened.
    bar: Vec<u64>,
    /// Where the line between the two groups is, in thousandths of the width (500 = half and half).
    pub split_pm: u16,
    /// The tab each group last showed (by id), for the group that does not have the focus.
    shown: [Option<u64>; 2],
    /// Servers whose hidden hosts are unfolded in the jump view.
    /// Bastions the user folded in the jump view (everything is unfolded by default).
    pub jump_open: HashSet<u64>,
    /// Where the open/closed state of the sidebar is kept (empty: nowhere, as in tests).
    pub ui_path: PathBuf,
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
            offset_bottom: 0,
            tabs: vec![],
            active: 0,
            scope: Scope::Ssh,
            remembered: HashMap::new(),
            emptied: HashSet::new(),
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
            jump_open: HashSet::new(),
            ui_path: PathBuf::new(),
            header: None,
            bar: Vec::new(),
            fold: [false; 2],
            other_rows: Vec::new(),
            split_pm: 500,
            shown: [None; 2],
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

    /// The rows of one view of the sidebar.
    fn build_rows(&self, view: View) -> Vec<Row> {
        fn walk(store: &Store, parent: Option<u64>, depth: usize, out: &mut Vec<Row>) {
            for f in store.folders.iter().filter(|f| f.parent == parent) {
                out.push(Row { node: NodeId::Folder(f.id), depth, jump: false });
                if f.expanded {
                    walk(store, Some(f.id), depth + 1, out);
                }
            }
            for s in store.servers.iter().filter(|s| s.parent == parent) {
                out.push(Row { node: NodeId::Server(s.id), depth, jump: false });
            }
        }
        fn walk_jump(store: &Store, via: Option<u64>, depth: usize, open: &HashSet<u64>, out: &mut Vec<Row>) {
            if depth > 16 {
                return;
            }
            for s in store.servers.iter().filter(|s| s.jump == via) {
                // At the top level only bastions: servers that others are reached through.
                if via.is_none() && !store.servers.iter().any(|x| x.jump == Some(s.id)) {
                    continue;
                }
                out.push(Row { node: NodeId::Server(s.id), depth, jump: true });
                if open.contains(&s.id) {
                    walk_jump(store, Some(s.id), depth + 1, open, out);
                }
            }
        }
        let mut rows: Vec<Row> = vec![];
        match view {
            View::Folders => {
                walk(&self.store, None, 0, &mut rows);
                // Below the folders, the bastions and what is reached through them (only if there are any).
                let bastions = self
                    .store
                    .servers
                    .iter()
                    .any(|s| s.jump.is_none() && self.store.servers.iter().any(|x| x.jump == Some(s.id)));
                if bastions {
                    rows.push(Row { node: NodeId::BastionsHeader, depth: 0, jump: false });
                    walk_jump(&self.store, None, 1, &self.jump_open, &mut rows);
                }
            }
            View::Spaces => {
                // On top: every project that is not filed, and the filed ones used lately. Below, like the
                // bastions, the folders (small, collapsible) with all their projects.
                let sp = &self.spaces;
                rows.extend(
                    sp.spaces
                        .iter()
                        .filter(|s| sp.on_top(s))
                        .map(|s| Row { node: NodeId::Space(s.id), depth: 0, jump: false }),
                );
                if !sp.folders.is_empty() {
                    rows.push(Row { node: NodeId::BastionsHeader, depth: 0, jump: false });
                    for f in &sp.folders {
                        rows.push(Row { node: NodeId::SpaceFolder(f.id), depth: 0, jump: true });
                        if f.expanded {
                            rows.extend(
                                sp.spaces
                                    .iter()
                                    .filter(|s| s.folder == Some(f.id))
                                    .map(|s| Row { node: NodeId::Space(s.id), depth: 1, jump: true }),
                            );
                        }
                    }
                }
            }
        }
        rows
    }

    pub fn rebuild(&mut self) {
        self.rows = self.build_rows(self.view);
        self.other_rows = self.build_rows(match self.view {
            View::Spaces => View::Folders,
            View::Folders => View::Spaces,
        });
        self.selected = self.selected.min(self.rows.len().saturating_sub(1));
        // The title of the bastions section is a label, not something to stand on.
        if self.rows.get(self.selected).is_some_and(|r| r.node == NodeId::BastionsHeader) {
            self.selected = self.selected.saturating_sub(1);
        }
    }

    pub fn has_children(&self, row: &Row) -> bool {
        match row.node {
            NodeId::Folder(id) => {
                self.store.folders.iter().any(|f| f.parent == Some(id))
                    || self.store.servers.iter().any(|s| s.parent == Some(id))
            }
            NodeId::Server(id) if row.jump => self.store.servers.iter().any(|s| s.jump == Some(id)),
            NodeId::SpaceFolder(id) => self.spaces.spaces.iter().any(|s| s.folder == Some(id)),
            NodeId::BastionsHeader | NodeId::Server(_) | NodeId::Space(_) => false,
        }
    }

    pub fn is_open(&self, row: &Row) -> bool {
        match row.node {
            NodeId::Folder(id) => self.store.folder(id).is_some_and(|f| f.expanded),
            NodeId::Server(id) => self.jump_open.contains(&id),
            NodeId::SpaceFolder(id) => self.spaces.folder(id).is_some_and(|f| f.expanded),
            NodeId::BastionsHeader | NodeId::Space(_) => false,
        }
    }

    fn set_open(&mut self, row: Row, open: bool) {
        match row.node {
            NodeId::Folder(id) => {
                if let Some(f) = self.store.folders.iter_mut().find(|f| f.id == id) {
                    f.expanded = open;
                }
                self.persist();
            }
            NodeId::Server(id) => {
                if open {
                    self.jump_open.insert(id);
                } else {
                    self.jump_open.remove(&id);
                }
                self.save_ui();
            }
            NodeId::SpaceFolder(id) => {
                if let Some(f) = self.spaces.folders.iter_mut().find(|f| f.id == id) {
                    f.expanded = open;
                }
                self.save_spaces();
            }
            NodeId::BastionsHeader | NodeId::Space(_) => {}
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
        self.offset_bottom = 0;
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
        // Going to another part of the sidebar brings back the tab you had there. If it has none, the tab
        // you were looking at stays.
        let back = self
            .remembered
            .get(&scope)
            .and_then(|id| self.tabs.iter().position(|t| t.id == *id))
            .or_else(|| self.tabs.iter().rposition(|t| t.scope == scope));
        if let Some(i) = back {
            self.active = i;
        }
        self.fix_active();
    }

    /// True when the content area has no terminal to show: no tabs at all, or the current scope's last tab
    /// was closed (the welcome screen is shown, rather than jumping to a tab of another scope).
    pub fn blank(&self) -> bool {
        !self.is_split()
            && (self.tabs.is_empty()
            || (self.emptied.contains(&self.scope) && self.tabs.get(self.active).is_none_or(|t| t.scope != self.scope)))
    }

    /// Indices in `tabs` of the tabs of the current scope, in order.
    pub fn scoped(&self) -> Vec<usize> {
        self.tabs.iter().enumerate().filter(|(_, t)| t.scope == self.scope).map(|(i, _)| i).collect()
    }

    /// Keeps `active` pointing at a tab, and the keyboard off an empty terminal.
    fn fix_active(&mut self) {
        if self.tabs.is_empty() {
            self.focus = Focus::Sidebar;
        } else {
            self.active = self.active.min(self.tabs.len() - 1);
        }
    }

    /// What a tab is called in the tab bar, which shows every tab of every part of oso: a terminal of an
    /// agent's project carries the project's name.
    pub fn tab_label(&self, t: &Tab) -> String {
        match t.scope {
            Scope::Space(id) => {
                let place = self.spaces.get(id).map(|s| s.name.as_str()).unwrap_or("?");
                if t.title == "shell" { place.to_string() } else { format!("{place}/{}", t.title) }
            }
            Scope::Ssh => t.title.clone(),
        }
    }

    /// Indices of the tabs where an AI agent is running, in any scope.
    pub fn agent_tabs(&self) -> Vec<usize> {
        self.tabs.iter().enumerate().filter(|(_, t)| t.session.agent().is_some()).map(|(i, _)| i).collect()
    }

    /// Makes tab `idx` the one on screen, wherever it lives: the sidebar moves to its place (the agent's
    /// project, or the SSH section), like a browser jumping between sites.
    pub fn select_tab(&mut self, idx: usize) {
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
    }

    /// Like `select_tab`, and puts the keyboard on the terminal.
    pub fn focus_tab(&mut self, idx: usize) {
        self.select_tab(idx);
        if let Some(t) = self.tabs.get_mut(idx) {
            t.attention = false;
            self.focus = Focus::Terminal;
        }
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

    /// Selects a row of the bastions section (`jump`) or of the folder tree: a server shows up in both.
    fn select_row(&mut self, node: NodeId, jump: bool) {
        match self.rows.iter().position(|r| r.node == node && r.jump == jump) {
            Some(i) => self.selected = i,
            None => self.select_node(node),
        }
    }

    /// Index of the title of the bastions section: where the SSH view splits into its two halves.
    pub fn split_index(&self) -> Option<usize> {
        self.rows.iter().position(|r| r.node == NodeId::BastionsHeader)
    }

    pub fn selected_row(&self) -> Option<Row> {
        self.rows.get(self.selected).copied()
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

    /// Remembers what is open in the sidebar, for the next run.
    fn save_ui(&mut self) {
        if self.ui_path.as_os_str().is_empty() {
            return;
        }
        let mut open_nodes: Vec<u64> = self.jump_open.iter().copied().collect();
        open_nodes.sort_unstable();
        let state = crate::uistate::UiState { open_nodes };
        if let Err(e) = state.save_to(&self.ui_path) {
            self.set_flash(format!("Could not save the sidebar state: {e}"));
        }
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
                right: false,
                session: Session::from_remote(d.attach(info.sid, rows, cols)),
                attention: meta.get("attention").and_then(|v| v.as_bool()).unwrap_or(false),
                meta_sent: String::new(),
            });
        }
        if !list.is_empty() {
            self.select_tab(0);
            self.set_flash(format!("Restored {} session(s) from the background server", list.len()));
        }
    }

    pub fn live_sessions(&self) -> usize {
        self.tabs.iter().filter(|t| t.session.exit_code.is_none()).count()
    }

    /// Called on every pass of the event loop.
    pub fn tick(&mut self) {
        self.sync_bar();
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
            if let Some(t) = self.tabs.get_mut(self.active) {
                t.attention = false;
            }
        }
        self.reap_exited_projects();
        self.reap_clean_ssh_exits();
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
                text: format!("{n} session(s) still open. Quit oso?"),
                action: ConfirmAction::Quit,
            }));
        }
    }

    // ------------------------------------------------------------ sessions and tabs

    pub fn open_server(&mut self, id: u64) {
        self.modal = self.gate(Pending::Open(id));
    }

    /// Enter / double click on a server: go to its tab if it already has a live one; a new tab only when there is
    /// none (more of them are opened on purpose, with `n`).
    fn open_or_focus_server(&mut self, id: u64) {
        let live = self.tabs.iter().position(|t| t.server_id == id && t.scope == Scope::Ssh && t.session.exit_code.is_none());
        match live {
            Some(i) => self.focus_tab(i),
            None => self.open_server(id),
        }
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
            return self.set_flash("Create a project first (press a)");
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
                        right: self.is_split() && self.tabs.get(self.active).is_some_and(|t| t.right),
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
        // The tab to land on: its neighbour in the bar.
        let bar = self.bar_side(self.tabs[idx].right);
        let neighbour = match bar.iter().position(|&i| i == idx) {
            Some(p) if p > 0 => Some(bar[p - 1]),
            Some(p) if p + 1 < bar.len() => Some(bar[p + 1]),
            _ if idx > 0 => Some(idx - 1),
            _ if self.tabs.len() > 1 => Some(1),
            _ => None,
        };
        let was_active = idx == self.active;
        let closed_scope = self.tabs[idx].scope;
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
        let scope_left = self.tabs.iter().any(|t| t.scope == closed_scope);
        if was_active && closed_scope == self.scope && !scope_left {
            // The last terminal of this part of oso: stay here and show the welcome screen.
            self.emptied.insert(closed_scope);
            self.focus = Focus::Sidebar;
        } else if was_active && !self.tabs.is_empty() {
            self.select_tab(self.active); // the sidebar follows the tab we land on
        }
    }

    /// Closes the open session of the selected server (the active tab if it is that server's, else the newest one).
    fn close_selected_tab(&mut self) {
        let id = match self.selected_node() {
            Some(NodeId::Server(id)) => id,
            Some(NodeId::Space(_)) => {
                return match self.scoped().contains(&self.active) {
                    true => self.close_tab(self.active),
                    false => self.set_flash("No open terminal in this project"),
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
    /// The tabs of the bar (indices into `tabs`), one per slot.
    /// The left group first, then the right one: the numbers of the tabs run through both.
    pub fn bar_tabs(&self) -> Vec<usize> {
        let mut all = self.bar_side(false);
        all.extend(self.bar_side(true));
        all
    }

    /// The tabs of one group (indices into `tabs`), in the order they were opened.
    pub fn bar_side(&self, right: bool) -> Vec<usize> {
        self.bar
            .iter()
            .filter_map(|id| self.tabs.iter().position(|t| t.id == *id))
            .filter(|&i| self.tabs[i].right == right)
            .collect()
    }

    /// Two groups of tabs side by side: both have tabs. A group that loses its last tab disappears.
    pub fn is_split(&self) -> bool {
        self.tabs.iter().any(|t| t.right) && self.tabs.iter().any(|t| !t.right)
    }

    /// The tab a group shows: the active one in the group that has the focus, else the one it showed last.
    pub fn pane_tab(&self, right: bool) -> Option<usize> {
        if self.tabs.get(self.active).is_some_and(|t| t.right == right) {
            return Some(self.active);
        }
        let side = self.bar_side(right);
        self.shown[right as usize]
            .and_then(|id| side.iter().copied().find(|&i| self.tabs[i].id == id))
            .or_else(|| side.last().copied())
    }

    /// Sends the active tab to the left or right group (Alt+Shift+←/→). The first tab sent right makes the split.
    fn send_tab(&mut self, right: bool) {
        let Some(t) = self.tabs.get(self.active) else { return };
        if t.right == right {
            return;
        }
        if right && self.tabs.iter().filter(|t| !t.right).count() < 2 {
            return self.set_flash("Open another tab first: the left group needs one");
        }
        self.tabs[self.active].right = right;
    }

    /// An SSH tab whose session ended cleanly (exit code 0) closes by itself, like a browser tab. One that failed
    /// stays, showing the error, until it is reconnected (Enter) or closed.
    fn reap_clean_ssh_exits(&mut self) {
        if self.daemon.as_ref().is_some_and(|d| !d.alive()) {
            return;
        }
        let done: Vec<usize> = (0..self.tabs.len())
            .filter(|&i| self.tabs[i].scope == Scope::Ssh && self.tabs[i].session.exit_code == Some(0))
            .collect();
        for i in done.into_iter().rev() {
            self.close_tab(i);
        }
    }

    /// A project whose terminals have all exited (you typed `exit`) disappears from the list. Only the entry
    /// goes: the directory is never touched. Not when the background server was lost, which ends every session.
    fn reap_exited_projects(&mut self) {
        if self.daemon.as_ref().is_some_and(|d| !d.alive()) {
            return;
        }
        let dead: Vec<usize> = (0..self.tabs.len())
            .filter(|&i| matches!(self.tabs[i].scope, Scope::Space(_)) && self.tabs[i].session.exit_code.is_some())
            .collect();
        if dead.is_empty() {
            return;
        }
        let mut gone: Vec<u64> = vec![];
        for &i in &dead {
            if let Scope::Space(id) = self.tabs[i].scope {
                if !gone.contains(&id) {
                    gone.push(id);
                }
            }
        }
        for i in dead.into_iter().rev() {
            self.close_tab(i);
        }
        for id in gone {
            if !self.tabs.iter().any(|t| t.scope == Scope::Space(id)) {
                self.remembered.remove(&Scope::Space(id));
                self.spaces.remove(id);
            }
        }
        self.save_spaces();
        self.rebuild();
        self.sync_scope();
    }

    /// The project you work in goes to the top of the list (most recently used first). It happens when you press
    /// Enter in one of its terminals (you ran something), not when you merely select, open or type in it, so
    /// moving through the list does not shuffle it.
    fn touch_space(&mut self, id: u64) {
        let promoted = self.spaces.mark_used(id);
        if self.spaces.touch(id) || promoted {
            self.save_spaces();
            // Stay on the row you were on: a filed project also shows on top, and the selection must not jump
            // out of its folder to that copy.
            let keep = self.rows.get(self.selected).map(|r| (r.node, r.jump));
            self.rebuild();
            if let Some((n, jump)) = keep {
                match self.rows.iter().position(|r| r.node == n && r.jump == jump) {
                    Some(i) => self.selected = i,
                    None => self.select_node(n),
                }
            }
        }
    }

    /// Keeps the bar in step with the tabs, like a browser: every open tab has its place, in the order it was
    /// opened, and a closed one loses it. Going to a session from the sidebar never replaces another tab.
    fn sync_bar(&mut self) {
        let tabs = &self.tabs;
        self.bar.retain(|id| tabs.iter().any(|t| t.id == *id));
        for t in &self.tabs {
            if !self.bar.contains(&t.id) {
                self.bar.push(t.id);
            }
        }
        if !self.tabs.iter().any(|t| !t.right) {
            for t in &mut self.tabs {
                t.right = false; // nothing is left of the line: the right group becomes the only one
            }
        }
        if let Some(t) = self.tabs.get(self.active) {
            self.shown[t.right as usize] = Some(t.id);
        }
    }

    /// Alt+←/→: the previous or next tab of the bar.
    fn step_tab(&mut self, d: isize) {
        let right = self.tabs.get(self.active).is_some_and(|t| t.right);
        let tabs = self.bar_side(right);
        if tabs.is_empty() {
            return;
        }
        let at = tabs.iter().position(|&i| i == self.active).unwrap_or(0) as isize;
        let to = (at + d).rem_euclid(tabs.len() as isize) as usize;
        self.select_tab(tabs[to]);
    }

    /// Ctrl+N and the + button: another tab beside the one you are on, both kept in the bar (same server, or a terminal in the same project).
    fn new_tab_here(&mut self) {
        let server = self
            .tabs
            .get(self.active)
            .filter(|t| t.scope == self.scope && self.scope == Scope::Ssh)
            .map(|t| t.server_id)
            .or(self.selected_server_id());
        self.open_server(server.unwrap_or(LOCAL));
    }

    fn selected_server_id(&self) -> Option<u64> {
        match self.selected_node() {
            Some(NodeId::Server(id)) if self.scope == Scope::Ssh => Some(id),
            _ => None,
        }
    }

    fn goto_tab(&mut self, i: usize) {
        if let Some(&t) = self.bar_tabs().get(i) {
            self.focus_tab(t);
        } else {
            self.set_flash(format!("No tab {}", i + 1));
        }
    }

    /// Drag and drop in the bar: tab `from` takes the place of tab `to`, in that tab's group.
    fn move_tab(&mut self, from: usize, to: usize) {
        if from == to || from >= self.tabs.len() || to >= self.tabs.len() {
            return;
        }
        let (fid, tid, right) = (self.tabs[from].id, self.tabs[to].id, self.tabs[to].right);
        if self.tabs[from].right != right {
            self.active = from;
            self.send_tab(right);
            if self.tabs[from].right != right {
                return;
            }
        }
        self.bar.retain(|&b| b != fid);
        let at = self.bar.iter().position(|&b| b == tid).unwrap_or(self.bar.len());
        self.bar.insert(at, fid);
        self.active = from;
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
        self.sync_bar();
    }

    fn on_key_inner(&mut self, key: KeyEvent) {
        self.selection = None;
        self.selecting = false;
        if self.modal.is_some() {
            self.modal_key(key);
            return;
        }
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let n = self.tabs.len();
        match key.code {
            KeyCode::F(6) => return self.cycle_focus(if shift { -1 } else { 1 }),
            KeyCode::Char('q') if alt => {
                self.focus = if self.focus == Focus::Terminal || self.blank() { Focus::Sidebar } else { Focus::Terminal };
                return;
            }
            KeyCode::F(2) if n > 0 => return self.rename_tab_prompt(),
            KeyCode::F(8) => return self.toggle_theme(),
            KeyCode::Char('k') if alt => return self.open_search(),
            KeyCode::Char('p') if alt && self.sudo_ready() => {
                self.modal = self.gate(Pending::FillSudo(self.active));
                return;
            }
            KeyCode::Left if alt && shift && n > 0 => return self.send_tab(false),
            KeyCode::Right if alt && shift && n > 0 => return self.send_tab(true),
            KeyCode::Left if alt && n > 0 => return self.step_tab(-1),
            KeyCode::Right if alt && n > 0 => return self.step_tab(1),
            KeyCode::Char('n') if ctrl => return self.new_tab_here(),
            KeyCode::Char('n') if alt => return self.next_agent(),
            KeyCode::Char('w') if alt && n > 0 => return self.close_tab(self.active),
            KeyCode::Char(c @ '1'..='9') if alt => return self.goto_tab(c as usize - '1' as usize),
            KeyCode::Char('0') if alt => return self.goto_tab(9),
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
            if key.code == KeyCode::Enter {
                if let Scope::Space(id) = tab.scope {
                    self.touch_space(id);
                }
            }
        }
    }

    /// The header item (index into `HEADER`) of the current view.
    fn view_index(&self) -> usize {
        match self.view {
            View::Spaces => 0,
            View::Folders => 1,
        }
    }

    /// Runs a sidebar header button (by mouse or keyboard).
    fn activate(&mut self, hit: Hit) {
        match hit {
            Hit::ViewFolders => self.set_view(View::Folders),
            Hit::ViewSpaces => self.set_view(View::Spaces),
            Hit::NewProject => self.new_space(),
            Hit::NewServer => self.new_server_form(),
            Hit::Term => self.open_server(LOCAL),
            Hit::Vault => self.modal = self.gate(Pending::OpenVault),
            Hit::Fold(i) => self.fold_column(i),
            Hit::Edit => self.edit_selected(),
            Hit::NewTab => self.new_tab_here(),
            Hit::NewTabRight => {
                if let Some(i) = self.pane_tab(true) {
                    self.active = i;
                }
                self.new_tab_here()
            }
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
        match key.code {
            KeyCode::Left | KeyCode::Char('h') => self.move_in_menu(i.saturating_sub(1), false),
            KeyCode::Right | KeyCode::Char('l') => self.move_in_menu((i + 1).min(HEADER.len() - 1), false),
            KeyCode::Down | KeyCode::Char('j') | KeyCode::Esc => self.header = None,
            KeyCode::Up | KeyCode::Char('k') => {}
            // On a column title Enter is just a way to (re)open it.
            KeyCode::Enter | KeyCode::Char(' ') => self.activate(HEADER[i]),
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
            // Past the first row of Servers: the last row of Projects (and the other way round past the last one).
            KeyCode::Up | KeyCode::Char('k') if self.selected == 0 && self.view == View::Folders => {
                self.set_view(View::Spaces);
                self.selected = self.rows.len().saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') if self.selected >= last && self.view == View::Spaces => {
                self.set_view(View::Folders);
                self.selected = 0;
            }
            KeyCode::Up | KeyCode::Char('k') if self.selected == 0 => self.header = Some(self.view_index()),
            KeyCode::Up | KeyCode::Char('k') => self.move_selection(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_selection(1),
            KeyCode::Home | KeyCode::Char('g') => self.selected = 0,
            KeyCode::End | KeyCode::Char('G') => self.selected = last,
            KeyCode::Right | KeyCode::Char('l') if matches!(self.selected_node(), Some(NodeId::Space(_))) => {
                match self.selected_row() {
                    Some(Row { node: NodeId::Space(id), jump: true, .. }) => self.open_filed_space(id),
                    Some(Row { node: NodeId::Space(id), .. }) => self.open_space(id),
                    _ => {}
                }
            }
            KeyCode::Right | KeyCode::Char('l') => self.step_in(),
            KeyCode::Left | KeyCode::Char('h') => self.step_out(),
            KeyCode::Enter | KeyCode::Char(' ') => match self.selected_row() {
                Some(Row { node: NodeId::Space(id), jump: true, .. }) => self.open_filed_space(id),
                Some(Row { node: NodeId::Space(id), .. }) => self.open_space(id),
                Some(Row { node: NodeId::Server(id), .. }) => self.open_or_focus_server(id),
                Some(row @ Row { node: NodeId::Folder(_) | NodeId::SpaceFolder(_), .. }) => self.toggle(row),
                Some(Row { node: NodeId::BastionsHeader, .. }) | None => {}
            },
            KeyCode::Char('v') | KeyCode::Tab => self.set_view(match self.view {
                View::Spaces => View::Folders,
                View::Folders => View::Spaces,
            }),
            KeyCode::Char(c @ '1'..='9') if !ctrl && !alt => self.goto_tab(c as usize - '1' as usize),
            KeyCode::Char('t') => self.open_server(LOCAL),
            KeyCode::Char('n') => {
                if let Some(NodeId::Server(id)) = self.selected_node() {
                    self.open_server(id);
                }
            }
            KeyCode::Char('/') => self.open_search(),
            KeyCode::Char('p') => self.modal = self.gate(Pending::OpenVault),
            KeyCode::Char('a') if self.view == View::Spaces => self.new_space(),
            KeyCode::Char('a') => self.new_server_form(),
            KeyCode::Char('f') => self.new_folder_prompt(),
            KeyCode::Char('m') if self.view == View::Spaces => self.move_space_prompt(),
            KeyCode::Char('e') | KeyCode::F(4) => self.edit_selected(),
            KeyCode::Char('d') | KeyCode::Delete => self.ask_delete(),
            KeyCode::Char('c') if !ctrl => self.close_selected_tab(),
            KeyCode::Char('q') => self.request_quit(),
            KeyCode::Char('c') if ctrl => self.request_quit(),
            _ => {}
        }
    }

    /// One row up or down, stepping over the title of the bastions section.
    fn move_selection(&mut self, delta: i32) {
        let n = self.rows.len() as i32;
        let mut i = self.selected as i32 + delta;
        while i >= 0 && i < n && self.rows[i as usize].node == NodeId::BastionsHeader {
            i += delta;
        }
        if i >= 0 && i < n {
            self.selected = i as usize;
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
            let jump = self.selected_row().is_some_and(|r| r.jump);
            if self.store.shift(n, delta, jump) {
                self.persist();
                self.rebuild();
                self.select_row(n, jump);
            }
        }
    }

    fn toggle(&mut self, row: Row) {
        let open = self.is_open(&row);
        self.set_open(row, !open);
    }

    /// Right arrow: unfold, or move into the first child if already unfolded.
    fn step_in(&mut self) {
        let Some(row) = self.selected_row() else { return };
        if !self.has_children(&row) {
            return;
        }
        if !self.is_open(&row) {
            return self.set_open(row, true);
        }
        if self.rows.get(self.selected + 1).is_some_and(|r| r.depth > row.depth) {
            self.selected += 1;
        }
    }

    /// Left arrow: fold, or go back up to the parent row.
    fn step_out(&mut self) {
        let Some(row) = self.selected_row() else { return };
        if self.has_children(&row) && self.is_open(&row) {
            return self.set_open(row, false);
        }
        if let Some(i) = self.rows[..self.selected].iter().rposition(|r| r.depth < row.depth) {
            self.selected = i;
        }
    }

    fn new_server_form(&mut self) {
        // From a bastion (or something behind one), the new server goes behind it.
        let jump = match self.selected_row() {
            Some(Row { node: NodeId::Server(id), jump: true, .. }) => Some(id),
            _ => None,
        };
        self.modal = Some(Modal::Form(Box::new(ServerForm::new(self.current_parent(), jump))));
    }

    fn new_folder_prompt(&mut self) {
        if self.view == View::Spaces {
            self.modal = Some(Modal::Prompt(Prompt {
                title: "New folder of projects".into(),
                input: Input::default(),
                kind: PromptKind::NewSpaceFolder,
            }));
            return;
        }
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
            self.set_flash(format!("Could not save projects: {e}"));
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
        if self.selected_node() != Some(NodeId::Space(id)) {
            self.select_node(NodeId::Space(id));
        }
        self.sync_scope();
        if self.scoped().is_empty() {
            self.open_server(LOCAL);
        } else {
            self.focus = Focus::Terminal;
        }
    }

    /// Enter on a project filed in a folder: the row is a shortcut to its directory, not a running project. Every
    /// time it is opened, a new project of that directory appears on top of the list; the shortcut stays as it was.
    fn open_filed_space(&mut self, id: u64) {
        let Some(shortcut) = self.spaces.get(id).cloned() else { return };
        let new = self.spaces.add(shortcut.name, shortcut.cwd);
        self.save_spaces();
        self.rebuild();
        self.open_space(new);
    }

    fn edit_selected(&mut self) {
        match self.selected_node() {
            Some(NodeId::Space(id)) => {
                if let Some(sp) = self.spaces.get(id) {
                    self.modal = Some(Modal::Prompt(Prompt {
                        title: "Rename project".into(),
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
            Some(NodeId::SpaceFolder(id)) => {
                if let Some(f) = self.spaces.folder(id) {
                    self.modal = Some(Modal::Prompt(Prompt {
                        title: "Rename folder".into(),
                        input: Input::new(&f.name),
                        kind: PromptKind::RenameSpaceFolder(id),
                    }));
                }
            }
            Some(NodeId::BastionsHeader) | None => {}
        }
    }

    /// `m` on a project: file it in a folder by name (a new name makes the folder; `-` takes it out).
    fn move_space_prompt(&mut self) {
        let Some(NodeId::Space(id)) = self.selected_node() else {
            return self.set_flash("Select a project to move it into a folder");
        };
        let current =
            self.spaces.get(id).and_then(|s| s.folder).and_then(|f| self.spaces.folder(f)).map(|f| f.name.clone());
        self.modal = Some(Modal::Prompt(Prompt {
            title: "Folder (new name creates it, - takes it out)".into(),
            input: Input::new(current.as_deref().unwrap_or("")),
            kind: PromptKind::MoveSpace(id),
        }));
    }

    fn ask_delete(&mut self) {
        let Some(node) = self.selected_node() else { return };
        if node == NodeId::BastionsHeader {
            return;
        }
        if let NodeId::SpaceFolder(id) = node {
            // Only the folder goes: its projects go back to the plain list.
            self.spaces.remove_folder(id);
            self.save_spaces();
            self.rebuild();
            return self.set_flash("Folder removed; its projects are back in the list");
        }
        let text = match node {
            NodeId::BastionsHeader | NodeId::SpaceFolder(_) => return,
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
            Modal::Search(q) => self.search_key(q, key),
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

    /// F6 / Shift+F6: the keyboard goes to the next (or previous) area on screen: the Projects column, the
    /// Servers column and the terminal (both terminals when the screen is split). Folded columns are skipped.
    fn cycle_focus(&mut self, d: isize) {
        #[derive(PartialEq, Clone, Copy)]
        enum Zone {
            Column(View),
            Terminal(bool),
        }
        let mut zones = vec![];
        for (i, view) in [(0, View::Spaces), (1, View::Folders)] {
            if !self.fold[i] {
                zones.push(Zone::Column(view));
            }
        }
        if self.is_split() {
            zones.extend([Zone::Terminal(false), Zone::Terminal(true)]);
        } else if !self.tabs.is_empty() {
            zones.push(Zone::Terminal(false));
        }
        let here = if self.focus == Focus::Terminal {
            Zone::Terminal(self.tabs.get(self.active).is_some_and(|t| t.right))
        } else {
            Zone::Column(self.view)
        };
        let at = zones.iter().position(|&z| z == here).unwrap_or(0) as isize;
        let Some(&to) = zones.get((at + d).rem_euclid(zones.len().max(1) as isize) as usize) else { return };
        self.header = None;
        match to {
            Zone::Column(view) => {
                self.set_view(view);
                self.focus = Focus::Sidebar;
            }
            Zone::Terminal(right) => {
                if let Some(i) = self.pane_tab(right) {
                    self.active = i;
                    self.focus = Focus::Terminal;
                }
            }
        }
    }

    /// F8: switches between the pastel theme (on your terminal's background) and the classic fixed colours, and remembers it.
    fn toggle_theme(&mut self) {
        let terminal = !crate::ui::terminal_theme();
        crate::ui::set_theme(terminal);
        crate::settings::save_theme(if terminal { "terminal" } else { "classic" });
        self.set_flash(if terminal { "Theme: pastel colours on your terminal's background" } else { "Theme: classic colours" });
    }

    /// Folds one column away (0: Projects, 1: Servers) or brings it back. The one left stays the active view.
    fn fold_column(&mut self, i: usize) {
        let view = if i == 0 { View::Spaces } else { View::Folders };
        self.fold[i] = !self.fold[i];
        self.header = None;
        if self.fold[i] {
            if self.view == view && !self.fold[1 - i] {
                self.set_view(if i == 0 { View::Folders } else { View::Spaces });
            }
            if self.fold.iter().all(|&f| f) && !self.blank() {
                self.focus = Focus::Terminal;
            }
        } else {
            self.set_view(view);
            self.focus = Focus::Sidebar;
        }
    }

    /// The column of the sidebar under (x, y), as the view it shows.
    fn column_at(&self, x: u16, y: u16) -> Option<View> {
        let i = (0..2).find(|&i| !self.fold[i] && inside(self.layout.cols[i], x, y))?;
        Some(if i == 0 { View::Spaces } else { View::Folders })
    }

    /// A click or the wheel on the column that is not the active view makes it the active one first (the list
    /// under the pointer keeps the place it was drawn at).
    fn activate_column_at(&mut self, x: u16, y: u16) {
        let Some(view) = self.column_at(x, y) else { return };
        if view == self.view {
            return;
        }
        let i = if view == View::Spaces { 0 } else { 1 };
        let g = self.layout.col_geom[i];
        self.set_view(view);
        self.layout.list = g.top;
        self.layout.list_bottom = g.bottom;
        self.layout.row_h = g.row_h;
    }

    /// Opens the quick open box with every server and project.
    fn open_search(&mut self) {
        let mut items = vec![SearchItem { label: "Local terminal".into(), detail: "this computer".into(), target: SearchTarget::Local }];
        for sp in &self.spaces.spaces {
            items.push(SearchItem { label: sp.name.clone(), detail: self.tilde(&sp.cwd), target: SearchTarget::Space(sp.id) });
        }
        for sv in &self.store.servers {
            let detail = if sv.user.is_empty() { sv.host.clone() } else { format!("{}@{}", sv.user, sv.host) };
            items.push(SearchItem { label: sv.name.clone(), detail, target: SearchTarget::Server(sv.id) });
        }
        self.modal = Some(Modal::Search(Box::new(SearchModal::new(items))));
    }

    fn run_search(&mut self, target: SearchTarget) {
        match target {
            SearchTarget::Server(id) => self.open_or_focus_server(id),
            SearchTarget::Space(id) => self.open_space(id),
            SearchTarget::Local => self.open_server(LOCAL),
        }
    }

    fn search_key(&mut self, mut q: Box<SearchModal>, key: KeyEvent) -> Option<Modal> {
        match key.code {
            KeyCode::Esc => return None,
            KeyCode::Up => q.selected = q.selected.saturating_sub(1),
            KeyCode::Down => q.selected = (q.selected + 1).min(q.hits.len().saturating_sub(1)),
            KeyCode::Enter => {
                let target = q.target()?;
                // Opening may ask for something (a password): whatever it opens is what stays on screen.
                self.run_search(target);
                return self.modal.take();
            }
            _ => {
                q.input.handle(key);
                q.refilter();
            }
        }
        Some(Modal::Search(q))
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
            PromptKind::NewSpaceFolder => {
                let id = self.spaces.add_folder(name);
                self.save_spaces();
                self.rebuild();
                self.select_node(NodeId::SpaceFolder(id));
                self.focus = Focus::Sidebar;
            }
            PromptKind::RenameSpaceFolder(id) => {
                if let Some(f) = self.spaces.folders.iter_mut().find(|f| f.id == id) {
                    f.name = name;
                }
                self.save_spaces();
            }
            PromptKind::MoveSpace(id) => {
                let folder = if name == "-" {
                    None
                } else {
                    let f = self.spaces.folder_named(&name).unwrap_or_else(|| self.spaces.add_folder(name.clone()));
                    if let Some(fo) = self.spaces.folders.iter_mut().find(|x| x.id == f) {
                        fo.expanded = true;
                    }
                    Some(f)
                };
                self.spaces.set_folder(id, folder);
                self.save_spaces();
                self.rebuild();
                self.select_node(NodeId::Space(id));
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
        let rh = self.layout.row_h.max(1);
        let b = self.layout.list_bottom;
        if let (true, Some(header)) = (inside(b, x, y), self.split_index()) {
            let rhb = if self.view == View::Spaces { 1 } else { rh };
            return Some(header + self.offset_bottom + ((y - b.y) / rhb) as usize);
        }
        let l = self.layout.list;
        if inside(l, x, y) { Some(self.offset + ((y - l.y) / rh) as usize) } else { None }
    }

    pub fn on_mouse(&mut self, ev: MouseEvent) {
        self.on_mouse_inner(ev);
        self.sync_scope();
        self.sync_bar();
    }

    fn on_mouse_inner(&mut self, ev: MouseEvent) {
        let (x, y) = (ev.column, ev.row);
        if self.modal.is_some() {
            return self.modal_mouse(ev);
        }
        match ev.kind {
            MouseEventKind::Down(MouseButton::Left) => self.mouse_down(x, y),
            MouseEventKind::Drag(MouseButton::Left) if self.selecting => self.drag_select(x, y),
            MouseEventKind::Drag(MouseButton::Left) if matches!(self.drag, Some(Drag::Divider)) => {
                let a = self.layout.split_area;
                if a.width > 0 {
                    let pm = (x.saturating_sub(a.x) as u32 * 1000 / a.width as u32) as u16;
                    self.split_pm = pm.clamp(150, 850);
                }
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                self.drop_hover = match self.drag {
                    Some(Drag::Node(..)) => self.row_at(x, y).filter(|&i| i < self.rows.len()),
                    _ => None,
                };
            }
            MouseEventKind::Up(MouseButton::Left) => self.mouse_up(x, y),
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                let up = matches!(ev.kind, MouseEventKind::ScrollUp);
                self.activate_column_at(x, y);
                if inside(self.layout.list_bottom, x, y) {
                    let max = self.rows.len().saturating_sub(self.split_index().unwrap_or(0) + 1);
                    self.offset_bottom =
                        if up { self.offset_bottom.saturating_sub(3) } else { (self.offset_bottom + 3).min(max) };
                } else if inside(self.layout.list, x, y) {
                    let max = self.split_index().unwrap_or(self.rows.len()).saturating_sub(1);
                    self.offset = if up { self.offset.saturating_sub(3) } else { (self.offset + 3).min(max) };
                } else if self.pane_at(x, y).is_some() {
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
        if self.is_split() && inside(self.layout.divider, x, y) {
            self.drag = Some(Drag::Divider);
            return;
        }
        self.activate_column_at(x, y);
        if inside(self.layout.sidebar, x, y) {
            self.focus = Focus::Sidebar;
            if let Some(i) = self.row_at(x, y).filter(|&i| i < self.rows.len() && self.rows[i].node != NodeId::BastionsHeader) {
                self.selected = i;
                let row = self.rows[i];
                // A click on the arrow of a folder (or of a bastion) opens or closes it right away.
                let on_arrow = (x.saturating_sub(self.layout.list.x) as usize) < row.depth * 2 + 4;
                if !double && on_arrow && self.has_children(&row) && !matches!(row.node, NodeId::Space(_)) {
                    self.toggle(row);
                    return;
                }
                if double {
                    match row.node {
                        NodeId::Folder(_) | NodeId::SpaceFolder(_) => self.toggle(row),
                        NodeId::BastionsHeader => {}
                        NodeId::Server(id) => self.open_or_focus_server(id),
                        NodeId::Space(id) if row.jump => self.open_filed_space(id),
                        NodeId::Space(id) => self.open_space(id),
                    }
                } else {
                    self.drag = Some(Drag::Node(row.node, row.jump));
                }
            }
        } else if inside(self.layout.tabbar, x, y) {
            let hit = self.layout.tabs.iter().find(|t| inside(t.rect, x, y)).map(|t| (t.idx, t.close));
            if let Some((i, close)) = hit {
                if inside(close, x, y) {
                    return self.close_tab(i);
                }
                self.focus_tab(i);
                if double {
                    self.rename_tab_prompt();
                } else {
                    self.drag = Some(Drag::Tab(i));
                }
            }
        } else if self.pane_at(x, y).is_some() && (self.is_split() || !self.scoped().is_empty()) {
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

    /// The group whose terminal is under (x, y): it takes the focus, and its area becomes `layout.content`.
    fn pane_at(&mut self, x: u16, y: u16) -> Option<bool> {
        let right = if self.is_split() && inside(self.layout.panes[1], x, y) {
            true
        } else if inside(self.layout.panes[0], x, y) || (!self.is_split() && inside(self.layout.content, x, y)) {
            false
        } else {
            return None;
        };
        if self.is_split() {
            if let Some(i) = self.pane_tab(right) {
                self.active = i;
            }
            self.layout.content = self.layout.panes[right as usize];
        }
        Some(right)
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
            Some(Drag::Divider) => {}
            Some(Drag::Tab(from)) => {
                if let Some(to) = self.layout.tabs.iter().find(|t| inside(t.rect, x, y)).map(|t| t.idx) {
                    self.move_tab(from, to);
                } else if from < self.tabs.len() {
                    // dropped on a terminal: the tab goes to that group
                    let right = if inside(self.layout.panes[1], x, y) {
                        Some(true)
                    } else if inside(self.layout.panes[0], x, y) || inside(self.layout.content, x, y) {
                        Some(false)
                    } else {
                        None
                    };
                    if let Some(right) = right {
                        self.active = from;
                        self.send_tab(right);
                    }
                }
            }
            Some(Drag::Node(node, jump)) => self.drop_node(node, jump, x, y),
            None => {}
        }
    }

    /// A project dropped on a folder (or on a project filed in it) goes into it; dropped on the top part it is
    /// taken out of its folder.
    fn drop_space(&mut self, node: NodeId, x: u16, y: u16) {
        let NodeId::Space(id) = node else { return };
        let target = self.row_at(x, y).and_then(|i| self.rows.get(i)).copied();
        let folder = match target {
            Some(Row { node: NodeId::SpaceFolder(f), .. }) => Some(Some(f)),
            Some(Row { node: NodeId::Space(t), jump: true, .. }) => self.spaces.get(t).map(|s| s.folder),
            Some(Row { node: NodeId::BastionsHeader, .. }) => None,
            _ if inside(self.layout.list, x, y) || target.is_some() => Some(None),
            _ => None,
        };
        let Some(folder) = folder else { return };
        if self.spaces.get(id).is_some_and(|s| s.folder == folder) {
            return;
        }
        if let Some(f) = folder.and_then(|f| self.spaces.folders.iter_mut().find(|x| x.id == f)) {
            f.expanded = true;
        }
        self.spaces.set_folder(id, folder);
        self.save_spaces();
        self.rebuild();
        self.select_node(NodeId::Space(id));
    }

    fn drop_node(&mut self, node: NodeId, from_jump: bool, x: u16, y: u16) {
        if !inside(self.layout.sidebar, x, y) {
            return;
        }
        if self.view == View::Spaces {
            return self.drop_space(node, x, y);
        }
        let target = self.row_at(x, y).and_then(|i| self.rows.get(i)).copied();
        // A server dropped on a bastion (or on something behind one) is reached through it from then on; one that
        // was already behind a bastion and is dropped on the section title or on empty space is reached directly.
        if let NodeId::Server(id) = node {
            let on_bastion = target.filter(|r| r.jump).and_then(|r| match r.node {
                NodeId::Server(s) => Some(s),
                _ => None,
            });
            let via = match (on_bastion, target) {
                (Some(s), _) if s == id => return,
                (Some(s), _) => Some(Some(s)),
                (None, Some(r)) if from_jump && r.node == NodeId::BastionsHeader => Some(None),
                (None, None) if from_jump => Some(None),
                _ => None,
            };
            if let Some(via) = via {
                if self.store.move_via(id, via) {
                    if let Some(v) = via {
                        self.jump_open.insert(v);
                        self.save_ui();
                    }
                    self.persist();
                    self.rebuild();
                    self.select_row(node, via.is_some());
                } else {
                    self.set_flash("Invalid move: that would create a loop");
                }
                return;
            }
        }
        if from_jump {
            return; // rows of the bastions section only move within it
        }
        let moved = match target.map(|r| (r.node, r.jump)) {
            Some((t, _)) if t == node => return,
            Some((_, true)) | Some((NodeId::BastionsHeader, _)) | Some((NodeId::Space(_) | NodeId::SpaceFolder(_), _)) => false,
            Some((NodeId::Folder(f), _)) => self.store.move_into(node, Some(f)),
            Some((NodeId::Server(s), _)) => match node {
                NodeId::Server(id) => self.store.move_server_before(id, s),
                NodeId::Folder(_) => {
                    let parent = self.store.server(s).and_then(|s| s.parent);
                    self.store.move_into(node, parent)
                }
                NodeId::Space(_) | NodeId::SpaceFolder(_) | NodeId::BastionsHeader => false,
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
            Modal::Search(mut q) if down => match hit {
                Some(Hit::Field(i)) if i < q.hits.len() => {
                    q.selected = i;
                    match q.target() {
                        Some(t) => {
                            self.run_search(t);
                            self.modal.take()
                        }
                        None => Some(Modal::Search(q)),
                    }
                }
                _ => Some(Modal::Search(q)),
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
    fn arrows_cross_between_servers_and_projects_and_reach_the_titles() {
        let mut app = app_with_servers(2);
        let dir = temp_dir("cross");
        app.spaces.add("proj".into(), dir.display().to_string());
        app.rebuild();
        press(&mut app, KeyCode::Up);
        assert_eq!((app.view, app.selected), (View::Spaces, 0), "Up from the first server: the last project");
        press(&mut app, KeyCode::Down);
        assert_eq!((app.view, app.selected), (View::Folders, 0), "Down from the last project: the first server");
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.view, View::Spaces, "Tab switches too");
        press(&mut app, KeyCode::Up);
        assert_eq!((app.header, app.view), (Some(0), View::Spaces), "Up from the first project: the title");
        press(&mut app, KeyCode::Right);
        assert_eq!((app.header, app.view), (Some(1), View::Folders), "Right on the titles: Servers");
        press(&mut app, KeyCode::Left);
        assert_eq!((app.header, app.view), (Some(0), View::Spaces), "and back to Projects");
        press(&mut app, KeyCode::Down);
        assert_eq!(app.header, None, "Down returns to the list");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn both_lists_are_built_and_the_panel_cannot_be_folded() {
        let mut app = app_with_servers(2);
        assert!(!app.rows.is_empty(), "the active view (Servers) has its rows");
        assert!(app.other_rows.is_empty(), "no projects yet");
        let dir = temp_dir("cols");
        app.spaces.add("proj".into(), dir.display().to_string());
        app.rebuild();
        assert_eq!(app.other_rows.len(), 1, "the other list has its own rows");
        app.on_key(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::ALT));
        assert_eq!(app.fold, [false, false], "Alt+B no longer folds anything");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    #[cfg(unix)]
    fn f6_goes_round_projects_servers_and_the_terminal() {
        let (mut app, _, dir) = app_with_two_tabs();
        app.focus = Focus::Sidebar;
        app.set_view(View::Spaces);
        let f6 = |app: &mut App, shift: bool| {
            app.on_key(KeyEvent::new(KeyCode::F(6), if shift { KeyModifiers::SHIFT } else { KeyModifiers::NONE }))
        };
        f6(&mut app, false);
        assert_eq!((app.view, app.focus), (View::Folders, Focus::Sidebar), "Projects -> Servers");
        f6(&mut app, false);
        assert_eq!(app.focus, Focus::Terminal, "Servers -> the terminal");
        f6(&mut app, false);
        assert_eq!((app.view, app.focus), (View::Spaces, Focus::Sidebar), "and round to Projects");
        f6(&mut app, true);
        assert_eq!(app.focus, Focus::Terminal, "Shift+F6 goes back");
        // A folded column is skipped.
        app.fold_column(0);
        app.focus = Focus::Sidebar;
        f6(&mut app, false);
        assert_eq!(app.focus, Focus::Terminal, "only Servers and the terminal are left");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn quick_open_finds_a_server_by_a_few_letters_and_opens_it() {
        let mut app = app_with_servers(3);
        let name = app.store.servers[1].name.clone();
        app.on_key(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::ALT));
        let Some(Modal::Search(q)) = &app.modal else { panic!("the search box should be open") };
        assert!(q.hits.len() >= 4, "the local terminal and the three servers");
        for c in name.chars() {
            app.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        let Some(Modal::Search(q)) = &app.modal else { panic!("still open while typing") };
        assert_eq!(q.target(), Some(SearchTarget::Server(app.store.servers[1].id)));
        app.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.modal.is_none());
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
        app.sync_bar();
        app.new_tab_here(); // + gives the bar a second slot
        app.sync_bar();
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
        // One directory per call: tests run in parallel and each removes its own when it ends.
        static COUNTER: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let d = std::env::temp_dir().join(format!("oso-app-{name}-{}-{n}", std::process::id()));
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
        assert!(app.scoped().is_empty(), "nothing of the space belongs to the SSH section");
        assert_eq!(app.tabs[app.active].scope, Scope::Space(id), "but its tab stays on screen, like a browser tab");
        app.open_server(LOCAL);
        assert_eq!(app.tabs[1].scope, Scope::Ssh, "a local terminal outside the spaces belongs to the SSH section");
        assert_eq!(app.scoped(), vec![1]);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    #[cfg(unix)]
    fn a_filed_project_is_a_shortcut_that_opens_a_new_project_on_top() {
        let dir = temp_dir("shortcut");
        let mut app = app_with_servers(0);
        let f = app.spaces.add_folder("F".into());
        let id = app.spaces.add("proj".into(), dir.display().to_string());
        app.spaces.set_folder(id, Some(f));
        app.rebuild();
        app.set_view(View::Spaces);
        app.open_filed_space(id);
        assert_eq!(app.spaces.spaces.len(), 2, "a project appears on top");
        let top = app.spaces.spaces.iter().find(|s| s.folder.is_none()).unwrap().id;
        assert_eq!(app.scope, Scope::Space(top));
        assert!(app.tabs.iter().all(|t| t.scope == Scope::Space(top)), "the shortcut has no terminal");
        app.open_filed_space(id);
        assert_eq!(app.spaces.spaces.len(), 3, "every opening makes a new project");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    #[cfg(unix)]
    fn every_open_tab_has_its_place_in_the_bar() {
        let (d1, d2) = (temp_dir("s1"), temp_dir("s2"));
        let mut app = app_with_servers(0);
        let a = app.spaces.add("a".into(), d1.display().to_string());
        let b = app.spaces.add("b".into(), d2.display().to_string());
        app.rebuild();
        app.set_view(View::Spaces);
        app.open_space(a);
        app.sync_bar();
        app.open_space(b);
        app.sync_bar();
        assert_eq!(app.tabs.len(), 2, "each project has its session");
        assert_eq!(app.bar_tabs().len(), 2, "and the bar shows both, like browser tabs");
        app.new_tab_here();
        app.sync_bar();
        assert_eq!(app.bar_tabs().len(), 3);
        app.select_node(NodeId::Space(a));
        app.sync_scope();
        app.open_space(a);
        app.sync_bar();
        assert_eq!(app.bar_tabs().len(), 3, "going back to a project opens nothing new");
        std::fs::remove_dir_all(d1).ok();
        std::fs::remove_dir_all(d2).ok();
    }

    #[test]
    #[cfg(unix)]
    fn the_project_you_use_moves_to_the_top() {
        let (d1, d2) = (temp_dir("m1"), temp_dir("m2"));
        let mut app = app_with_servers(0);
        let a = app.spaces.add("a".into(), d1.display().to_string());
        let b = app.spaces.add("b".into(), d2.display().to_string());
        app.rebuild();
        app.set_view(View::Spaces);
        assert_eq!(app.spaces.spaces[0].id, a);
        app.open_space(b);
        app.sync_bar();
        assert_eq!(app.spaces.spaces[0].id, a, "opening a project does not move it");
        assert_eq!(app.focus, Focus::Terminal);
        app.on_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
        assert_eq!(app.spaces.spaces[0].id, a, "typing without Enter does not either");
        app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(app.spaces.spaces[0].id, b, "Enter in it does: b was used last, so it comes first");
        assert_eq!(app.selected_node(), Some(NodeId::Space(b)), "and the selection follows it");
        std::fs::remove_dir_all(d1).ok();
        std::fs::remove_dir_all(d2).ok();
    }

    #[test]
    #[cfg(unix)]
    fn alt_q_switches_between_the_sidebar_and_the_terminal() {
        let mut app = app_with_servers(1);
        app.open_server(LOCAL);
        assert_eq!(app.focus, Focus::Terminal);
        app.on_key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::ALT));
        assert_eq!(app.focus, Focus::Sidebar);
        app.on_key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::ALT));
        assert_eq!(app.focus, Focus::Terminal);
    }

    #[test]
    #[cfg(unix)]
    fn a_project_whose_shell_exits_leaves_the_list() {
        let (d1, d2) = (temp_dir("x1"), temp_dir("x2"));
        let mut app = app_with_servers(0);
        let a = app.spaces.add("a".into(), d1.display().to_string());
        let b = app.spaces.add("b".into(), d2.display().to_string());
        app.rebuild();
        app.set_view(View::Spaces);
        app.open_space(a);
        app.open_space(b);
        let ti = app.tabs.iter().position(|t| t.scope == Scope::Space(b)).unwrap();
        app.tabs[ti].session.write(b"exit\r");
        for _ in 0..100 {
            app.tick();
            if app.spaces.get(b).is_none() {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(app.spaces.get(b).is_none(), "b is gone after exit");
        assert!(app.spaces.get(a).is_some(), "a stays");
        assert!(d2.exists(), "its directory is untouched");
        std::fs::remove_dir_all(d1).ok();
        std::fs::remove_dir_all(d2).ok();
    }

    #[test]
    fn projects_are_filed_in_folders_below_the_list() {
        let mut app = app_with_servers(0);
        let a = app.spaces.add("a".into(), "/tmp".into());
        let b = app.spaces.add("b".into(), "/tmp".into());
        app.rebuild();
        app.set_view(View::Spaces);
        app.select_node(NodeId::Space(a));
        app.modal = None;
        app.submit_prompt(&Prompt {
            title: String::new(),
            input: Input::new("Work"),
            kind: PromptKind::MoveSpace(a),
        });
        let nodes: Vec<(NodeId, bool)> = app.rows.iter().map(|r| (r.node, r.jump)).collect();
        let f = app.spaces.get(a).unwrap().folder.expect("the folder was created");
        assert_eq!(
            nodes,
            vec![
                (NodeId::Space(b), false),
                (NodeId::BastionsHeader, false),
                (NodeId::SpaceFolder(f), true),
                (NodeId::Space(a), true),
            ],
            "b on top; a only inside its folder"
        );
        // `-` takes it out again.
        app.submit_prompt(&Prompt { title: String::new(), input: Input::new("-"), kind: PromptKind::MoveSpace(a) });
        assert!(app.rows.iter().filter(|r| matches!(r.node, NodeId::Space(_))).all(|r| !r.jump));
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
        app.sync_bar();
        app.new_tab_here(); // second tab in a, in a slot of its own
        app.sync_bar();
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

    fn temp_dir_any(name: &str) -> PathBuf {
        static COUNTER: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let d = std::env::temp_dir().join(format!("oso-app-{name}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
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
            right: false,
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
        app.layout.row_h = 2; // spaces are two-line blocks
        for (y, row) in [(10, 0), (11, 0), (12, 1), (13, 1), (14, 2), (15, 2)] {
            assert_eq!(app.row_at(5, y), Some(row), "line {y}");
        }
        app.offset = 2;
        assert_eq!(app.row_at(5, 10), Some(2), "rows are counted from the scroll offset");
        app.layout.row_h = 1;
        assert_eq!(app.row_at(5, 12), Some(4), "one-line rows as before");
    }

    /// Three servers: `bastion`, `inner` (behind it) and `plain` (neither). Returns their ids.
    fn bastion_setup(app: &mut App) -> (u64, u64, u64) {
        let ids: Vec<u64> = app.store.servers.iter().map(|s| s.id).collect();
        let (b, inner, plain) = (ids[0], ids[1], ids[2]);
        app.store.move_via(inner, Some(b));
        app.rebuild();
        (b, inner, plain)
    }

    #[test]
    fn the_ssh_view_has_a_bastions_section_below_the_folders() {
        let mut app = app_with_servers(3);
        assert!(app.rows.iter().all(|r| !r.jump && r.node != NodeId::BastionsHeader), "no bastions yet: no section");
        let (b, inner, _) = bastion_setup(&mut app);
        assert_eq!(app.rows.len(), 5, "the section is always there: its title and the bastion, itself closed");
        app.jump_open.insert(b);
        app.rebuild();
        let nodes: Vec<(NodeId, bool, usize)> = app.rows.iter().map(|r| (r.node, r.jump, r.depth)).collect();
        let header = nodes.iter().position(|n| n.0 == NodeId::BastionsHeader).expect("the section title");
        assert_eq!(header, 3, "after the three servers of the folder tree");
        assert_eq!(nodes[header + 1], (NodeId::Server(b), true, 1), "the bastion, under the title");
        assert_eq!(nodes[header + 2], (NodeId::Server(inner), true, 2), "and what is behind it, one level deeper");
        assert_eq!(nodes.len(), 6);
    }

    /// The title is a label: only the bastions inside it fold.
    #[test]
    fn the_bastions_fold_and_the_section_title_is_only_a_label() {
        let mut app = app_with_servers(3);
        let (b, _, _) = bastion_setup(&mut app);
        app.selected = 2; // the last server of the folder tree
        press(&mut app, KeyCode::Down);
        assert_eq!(app.rows[app.selected].node, NodeId::Server(b), "Down steps over the title onto the first bastion");
        assert!(app.rows[app.selected].jump);
        press(&mut app, KeyCode::Up);
        assert_eq!(app.selected, 2, "and Up steps back over it");
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Right);
        assert_eq!(app.rows.len(), 6, "→ unfolds the bastion");
        press(&mut app, KeyCode::Left);
        assert_eq!(app.rows.len(), 5, "← folds it");
        app.selected = 3;
        press(&mut app, KeyCode::Enter);
        press(&mut app, KeyCode::Right);
        assert_eq!(app.rows.len(), 5, "nothing happens on the title");
    }

    #[test]
    fn which_bastions_are_open_is_remembered_between_runs() {
        let dir = temp_dir_any("uistate");
        let mut app = app_with_servers(3);
        let (b, _, _) = bastion_setup(&mut app);
        app.ui_path = dir.join("state.json");
        app.selected = app.rows.iter().position(|r| r.node == NodeId::Server(b) && r.jump).unwrap();
        press(&mut app, KeyCode::Right);
        let saved: crate::uistate::UiState = serde_json::from_str(&std::fs::read_to_string(&app.ui_path).unwrap()).unwrap();
        assert_eq!(saved.open_nodes, vec![b]);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn dropping_a_server_on_a_bastion_routes_it_through_that_bastion() {
        let mut app = app_with_servers(3);
        let (b, _, plain) = bastion_setup(&mut app);
        app.rebuild();
        app.layout.sidebar = Rect::new(0, 0, 32, 40);
        app.layout.list = Rect::new(1, 5, 30, 30);
        app.layout.row_h = 1;
        let bastion_row = app.rows.iter().position(|r| r.node == NodeId::Server(b) && r.jump).unwrap();
        app.drop_node(NodeId::Server(plain), false, 5, 5 + bastion_row as u16);
        assert_eq!(app.store.server(plain).unwrap().jump, Some(b));
        // and dropped on the section title (or on empty space) it is reached directly again
        let header_row = app.rows.iter().position(|r| r.node == NodeId::BastionsHeader).unwrap();
        app.drop_node(NodeId::Server(plain), true, 5, 5 + header_row as u16);
        assert_eq!(app.store.server(plain).unwrap().jump, None);
    }

    #[test]
    fn a_new_server_from_a_bastion_row_starts_behind_it() {
        let mut app = app_with_servers(3);
        let (b, _, _) = bastion_setup(&mut app);
        app.rebuild();
        app.selected = app.rows.iter().position(|r| r.node == NodeId::Server(b) && r.jump).unwrap();
        press(&mut app, KeyCode::Char('a'));
        let Some(Modal::Form(f)) = &app.modal else { panic!("the form should be open") };
        assert_eq!(f.jump, Some(b));
    }

    /// Two tabs: an agent project's terminal (index 0) and a local SSH-section terminal (index 1).
    #[cfg(unix)]
    fn app_with_two_tabs() -> (App, u64, PathBuf) {
        let dir = temp_dir("bar");
        let mut app = app_with_servers(1);
        let id = app.spaces.add("proj".into(), dir.display().to_string());
        app.rebuild();
        app.set_view(View::Spaces);
        app.open_space(id);
        app.set_view(View::Folders);
        app.open_server(LOCAL);
        assert_eq!((app.tabs.len(), app.active, app.view), (2, 1, View::Folders), "tabs: {:?}", app.tabs.iter().map(|t| (t.scope, t.session.exit_code)).collect::<Vec<_>>());
        (app, id, dir)
    }

    #[test]
    #[cfg(unix)]
    fn the_tab_bar_travels_between_agents_and_ssh() {
        let (mut app, id, dir) = app_with_two_tabs();
        app.select_tab(0);
        assert_eq!((app.view, app.scope, app.active), (View::Spaces, Scope::Space(id), 0), "the sidebar goes to the agent");
        assert_eq!(app.selected_node(), Some(NodeId::Space(id)));
        app.select_tab(1);
        assert_eq!((app.view, app.scope, app.active), (View::Folders, Scope::Ssh, 1), "and back to SSH");
        // Alt+arrows and the digits stay inside what is selected: here each part has one tab.
        app.on_key(KeyEvent::new(KeyCode::Right, KeyModifiers::ALT));
        assert_eq!((app.active, app.view), (1, View::Folders));
        press(&mut app, KeyCode::Char('2'));
        assert_eq!((app.active, app.view), (1, View::Folders), "there is no second tab here");
        app.select_tab(0);
        assert_eq!((app.active, app.view), (0, View::Spaces));
        // Ctrl+N adds a tab to the project you are on, and only that project shows it.
        app.on_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL));
        assert_eq!(app.tabs.len(), 3);
        assert_eq!(app.bar_tabs().len(), 3, "all three tabs are in the bar");
        app.on_key(KeyEvent::new(KeyCode::Left, KeyModifiers::ALT));
        assert_eq!(app.active, app.bar_tabs()[1]);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    #[cfg(unix)]
    fn closing_the_last_tab_of_a_scope_shows_the_welcome_instead_of_jumping() {
        let (mut app, _, dir) = app_with_two_tabs();
        let view = app.view;
        app.close_tab(1);
        assert_eq!((app.tabs.len(), app.view, app.focus), (1, view, Focus::Sidebar), "the sidebar stays where it was");
        assert!(app.blank(), "the empty scope shows the welcome screen, not the other scope's tab");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    #[cfg(unix)]
    fn every_emptied_scope_keeps_showing_the_welcome() {
        let (mut app, id, dir) = app_with_two_tabs();
        app.close_tab(1); // SSH emptied
        assert!(app.blank());
        app.select_tab(0); // go to the project, then empty it too
        app.close_tab(0);
        assert!(app.blank(), "the project is empty");
        app.set_view(View::Folders);
        app.sync_scope();
        assert!(app.blank(), "SSH was emptied earlier and still shows the welcome");
        let _ = id;
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    #[cfg(unix)]
    fn tabs_can_be_sent_to_a_right_group_and_the_split_goes_away_when_it_empties() {
        let (mut app, _, dir) = app_with_two_tabs();
        assert!(!app.is_split());
        app.on_key(KeyEvent::new(KeyCode::Right, KeyModifiers::ALT | KeyModifiers::SHIFT));
        assert!(app.is_split(), "the second tab went to the right group");
        assert_eq!((app.bar_side(false), app.bar_side(true)), (vec![0], vec![1]));
        assert!(!app.blank());
        // The only tab of the left group cannot go right: that would leave nothing on the left.
        app.select_tab(0);
        app.on_key(KeyEvent::new(KeyCode::Right, KeyModifiers::ALT | KeyModifiers::SHIFT));
        assert_eq!(app.bar_side(false), vec![0]);
        // Back to the left: one group again.
        app.select_tab(1);
        app.on_key(KeyEvent::new(KeyCode::Left, KeyModifiers::ALT | KeyModifiers::SHIFT));
        assert!(!app.is_split());
        // Closing the last tab of a group ends the split too.
        app.on_key(KeyEvent::new(KeyCode::Right, KeyModifiers::ALT | KeyModifiers::SHIFT));
        assert!(app.is_split());
        app.close_tab(1);
        assert!(!app.is_split());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    #[cfg(unix)]
    fn tabs_of_a_project_carry_its_name_in_the_bar() {
        let (mut app, _, dir) = app_with_two_tabs();
        assert_eq!(app.tab_label(&app.tabs[0]), "proj");
        assert_eq!(app.tab_label(&app.tabs[1]), "Local");
        app.tabs[0].title = "build".into();
        assert_eq!(app.tab_label(&app.tabs[0]), "proj/build");
        std::fs::remove_dir_all(dir).ok();
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
