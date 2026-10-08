//! Application state and keyboard/mouse handling. Drawing lives in `ui.rs`.

use crate::keys;
use crate::secrets;
use crate::session::Session;
use crate::store::{Auth, NodeId, Server, Store};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

// ---------------------------------------------------------------- text input

#[derive(Default, Clone)]
pub struct Input {
    pub value: String,
    /// Cursor position, in characters.
    pub cursor: usize,
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
        match key.code {
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

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Sidebar,
    Terminal,
}

pub struct Tab {
    pub title: String,
    pub server_id: u64,
    pub session: Session,
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

pub struct ServerForm {
    pub editing: Option<u64>,
    pub parent: Option<u64>,
    pub name: Input,
    pub host: Input,
    pub port: Input,
    pub user: Input,
    pub key: Input,
    pub secret: Input,
    pub auth: Auth,
    pub jump: Option<u64>,
    pub focus: usize,
    pub error: Option<String>,
    pub secret_saved: bool,
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
            auth: Auth::Agent,
            focus: F_NAME,
            error: None,
            secret_saved: false,
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
            auth: s.auth,
            focus: F_NAME,
            error: None,
            secret_saved: secrets::get(s.id).is_some(),
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
    Delete(NodeId),
    Quit,
}

pub struct Confirm {
    pub text: String,
    pub action: ConfirmAction,
}

pub enum Modal {
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
    Field(usize),
    Browse,
    Save,
    Cancel,
    Yes,
    No,
}

#[derive(Clone, Copy)]
pub struct TabHit {
    pub rect: Rect,
    pub close: Rect,
}

#[derive(Default)]
pub struct Layout {
    pub sidebar: Rect,
    pub list: Rect,
    pub tabbar: Rect,
    pub content: Rect,
    pub tabs: Vec<TabHit>,
    pub toolbar: Vec<(Rect, Hit)>,
    pub modal: Vec<(Rect, Hit)>,
    pub picker_list: Rect,
}

fn inside(r: Rect, x: u16, y: u16) -> bool {
    x >= r.x && x < r.x + r.width && y >= r.y && y < r.y + r.height
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum View {
    Folders,
    Jump,
}

enum Drag {
    Node(NodeId),
    Tab(usize),
}

// ---------------------------------------------------------------- app

pub struct App {
    pub store: Store,
    pub rows: Vec<Row>,
    pub selected: usize,
    pub offset: usize,
    pub tabs: Vec<Tab>,
    pub active: usize,
    pub focus: Focus,
    pub modal: Option<Modal>,
    pub layout: Layout,
    pub quit: bool,
    pub flash: Option<(String, Instant)>,
    /// Row under the pointer while dragging (highlighted as the drop target).
    pub drop_hover: Option<usize>,
    pub view: View,
    /// Servers whose hidden hosts are unfolded in the jump view.
    jump_open: HashSet<u64>,
    drag: Option<Drag>,
    last_click: Option<(Instant, u16, u16)>,
}

impl App {
    pub fn new(store: Store) -> Self {
        let mut app = App {
            store,
            rows: vec![],
            selected: 0,
            offset: 0,
            tabs: vec![],
            active: 0,
            focus: Focus::Sidebar,
            modal: None,
            layout: Layout::default(),
            quit: false,
            flash: None,
            drop_hover: None,
            view: View::Folders,
            jump_open: HashSet::new(),
            drag: None,
            last_click: None,
        };
        app.rebuild();
        app
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
        fn walk_jump(store: &Store, via: Option<u64>, depth: usize, open: &HashSet<u64>, out: &mut Vec<Row>) {
            if depth > 16 {
                return;
            }
            for s in store.servers.iter().filter(|s| s.jump == via) {
                out.push(Row { node: NodeId::Server(s.id), depth });
                if open.contains(&s.id) {
                    walk_jump(store, Some(s.id), depth + 1, open, out);
                }
            }
        }
        self.rows.clear();
        match self.view {
            View::Folders => walk(&self.store, None, 0, &mut self.rows),
            View::Jump => walk_jump(&self.store, None, 0, &self.jump_open, &mut self.rows),
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
            (NodeId::Server(_), View::Folders) => false,
        }
    }

    pub fn is_open(&self, node: NodeId) -> bool {
        match node {
            NodeId::Folder(id) => self.store.folder(id).is_some_and(|f| f.expanded),
            NodeId::Server(id) => self.jump_open.contains(&id),
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
                    self.jump_open.insert(id);
                } else {
                    self.jump_open.remove(&id);
                }
            }
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
        if let Some(n) = keep {
            self.select_node(n);
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

    fn persist(&mut self) {
        if let Err(e) = self.store.save() {
            self.set_flash(format!("Could not save: {e}"));
        }
    }

    pub fn live_sessions(&self) -> usize {
        self.tabs.iter().filter(|t| t.session.exit_code.is_none()).count()
    }

    /// Called on every pass of the event loop.
    pub fn tick(&mut self) {
        for t in &mut self.tabs {
            t.session.poll_exit();
        }
        if self.flash.as_ref().is_some_and(|(_, t)| t.elapsed() > Duration::from_secs(5)) {
            self.flash = None;
        }
    }

    fn request_quit(&mut self) {
        let n = self.live_sessions();
        if n == 0 {
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
        let Some(server) = self.store.server(id).cloned() else { return };
        let c = self.layout.content;
        let (rows, cols) = if c.width > 0 && c.height > 0 { (c.height, c.width) } else { (24, 80) };
        match Session::spawn(&ssh_argv(&self.store, &server), rows, cols, secret_for(&server)) {
            Ok(session) => {
                self.tabs.push(Tab { title: server.name.clone(), server_id: id, session });
                self.active = self.tabs.len() - 1;
                self.focus = Focus::Terminal;
            }
            Err(e) => self.set_flash(format!("Could not start ssh: {e:#}")),
        }
    }

    fn reconnect(&mut self, idx: usize) {
        let Some(server) = self.tabs.get(idx).and_then(|t| self.store.server(t.server_id)).cloned() else {
            self.set_flash("This tab’s server no longer exists");
            return;
        };
        let (rows, cols) = self.tabs[idx].session.size();
        match Session::spawn(&ssh_argv(&self.store, &server), rows, cols, secret_for(&server)) {
            Ok(session) => self.tabs[idx].session = session,
            Err(e) => self.set_flash(format!("Could not reconnect: {e:#}")),
        }
    }

    fn close_tab(&mut self, idx: usize) {
        if idx >= self.tabs.len() {
            return;
        }
        self.tabs.remove(idx);
        if self.active > idx || self.active >= self.tabs.len() {
            self.active = self.active.saturating_sub(1);
        }
        if self.tabs.is_empty() {
            self.focus = Focus::Sidebar;
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
        if self.modal.is_some() {
            self.modal_key(key);
            return;
        }
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let n = self.tabs.len();
        match key.code {
            KeyCode::F(6) => {
                self.focus = if self.focus == Focus::Terminal || n == 0 { Focus::Sidebar } else { Focus::Terminal };
                return;
            }
            KeyCode::F(2) if n > 0 => return self.rename_tab_prompt(),
            KeyCode::Left if alt && shift => return self.move_tab(self.active, self.active.saturating_sub(1)),
            KeyCode::Right if alt && shift && n > 0 => return self.move_tab(self.active, (self.active + 1).min(n - 1)),
            KeyCode::Left if alt && n > 0 => return self.active = (self.active + n - 1) % n,
            KeyCode::Right if alt && n > 0 => return self.active = (self.active + 1) % n,
            KeyCode::Char('w') if alt && n > 0 => return self.close_tab(self.active),
            KeyCode::Char(c @ '1'..='9') if alt => {
                let i = c as usize - '1' as usize;
                if i < n {
                    self.active = i;
                }
                return;
            }
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
                _ => {}
            }
        }
        let app_cursor = tab.session.with_screen(|s| s.application_cursor());
        if let Some(bytes) = keys::encode(key, app_cursor) {
            tab.session.reset_scroll();
            tab.session.write(&bytes);
        }
    }

    fn sidebar_key(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let last = self.rows.len().saturating_sub(1);
        match key.code {
            KeyCode::Up | KeyCode::Char('k') if alt => self.shift_selected(-1),
            KeyCode::Down | KeyCode::Char('j') if alt => self.shift_selected(1),
            KeyCode::Up | KeyCode::Char('k') => self.selected = self.selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => self.selected = (self.selected + 1).min(last),
            KeyCode::Home | KeyCode::Char('g') => self.selected = 0,
            KeyCode::End | KeyCode::Char('G') => self.selected = last,
            KeyCode::Right | KeyCode::Char('l') => self.step_in(),
            KeyCode::Left | KeyCode::Char('h') => self.step_out(),
            KeyCode::Enter | KeyCode::Char(' ') => match self.selected_node() {
                Some(NodeId::Server(id)) => self.open_server(id),
                Some(n @ NodeId::Folder(_)) => self.toggle(n),
                None => {}
            },
            KeyCode::Char('v') | KeyCode::Tab => {
                self.set_view(if self.view == View::Folders { View::Jump } else { View::Folders })
            }
            KeyCode::Char('a') => self.new_server_form(),
            KeyCode::Char('f') => self.new_folder_prompt(),
            KeyCode::Char('e') | KeyCode::F(4) => self.edit_selected(),
            KeyCode::Char('d') | KeyCode::Delete => self.ask_delete(),
            KeyCode::Char('q') => self.request_quit(),
            KeyCode::Char('c') if ctrl => self.request_quit(),
            _ => {}
        }
    }

    fn shift_selected(&mut self, delta: i32) {
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
        if self.view == View::Jump {
            return self.set_flash("Folders are created in the Folders view (press v)");
        }
        self.modal = Some(Modal::Prompt(Prompt {
            title: "New folder".into(),
            input: Input::default(),
            kind: PromptKind::NewFolder(self.current_parent()),
        }));
    }

    fn edit_selected(&mut self) {
        match self.selected_node() {
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
            NodeId::Server(id) => {
                format!("Delete server “{}”?", self.store.server(id).map(|s| s.name.as_str()).unwrap_or("?"))
            }
            NodeId::Folder(id) => format!(
                "Delete folder “{}” and everything in it?",
                self.store.folder(id).map(|f| f.name.as_str()).unwrap_or("?")
            ),
        };
        self.modal = Some(Modal::Confirm(Confirm { text, action: ConfirmAction::Delete(node) }));
    }

    // ------------------------------------------------------------ modales (teclado)

    fn modal_key(&mut self, key: KeyEvent) {
        let Some(modal) = self.modal.take() else { return };
        self.modal = match modal {
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
            KeyCode::Char('y') | KeyCode::Char('s') | KeyCode::Enter => {
                self.run_confirmed(c.action);
                None
            }
            KeyCode::Char('n') | KeyCode::Esc => None,
            _ => Some(Modal::Confirm(c)),
        }
    }

    fn run_confirmed(&mut self, action: ConfirmAction) {
        match action {
            ConfirmAction::Quit => self.quit = true,
            ConfirmAction::Delete(node) => {
                for id in self.store.delete(node) {
                    secrets::delete(id);
                }
                self.persist();
                self.rebuild();
            }
        }
    }

    fn save_form(&mut self, mut f: Box<ServerForm>) -> Option<Modal> {
        let host = f.host.value.trim().to_string();
        let port: u16 = match f.port.value.trim().parse() {
            Ok(p) if p > 0 => p,
            _ => {
                f.error = Some("Port must be a number between 1 and 65535".into());
                f.focus = F_PORT;
                return Some(Modal::Form(f));
            }
        };
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
        let name = match f.name.value.trim() {
            "" => host.clone(),
            n => n.to_string(),
        };
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
        };
        let id = match f.editing {
            Some(id) => {
                self.store.update_server(server);
                id
            }
            None => self.store.add_server(server),
        };
        if f.auth == Auth::Agent {
            secrets::delete(id);
        } else if !f.secret.value.is_empty() {
            if let Err(e) = secrets::set(id, &f.secret.value) {
                self.set_flash(format!("Could not save to the keyring: {e:#}"));
            }
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
        if inside(l, x, y) { Some(self.offset + (y - l.y) as usize) } else { None }
    }

    pub fn on_mouse(&mut self, ev: MouseEvent) {
        let (x, y) = (ev.column, ev.row);
        if self.modal.is_some() {
            return self.modal_mouse(ev);
        }
        match ev.kind {
            MouseEventKind::Down(MouseButton::Left) => self.mouse_down(x, y),
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
        if let Some(&(_, hit)) = self.layout.toolbar.iter().find(|(r, _)| inside(*r, x, y)) {
            match hit {
                Hit::ViewFolders => self.set_view(View::Folders),
                Hit::ViewJump => self.set_view(View::Jump),
                Hit::AddServer => self.new_server_form(),
                Hit::AddFolder => self.new_folder_prompt(),
                _ => {}
            }
            return;
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
                    }
                } else {
                    self.drag = Some(Drag::Node(node));
                }
            }
        } else if inside(self.layout.tabbar, x, y) {
            let hit = self.layout.tabs.iter().position(|t| inside(t.rect, x, y));
            if let Some(i) = hit {
                if inside(self.layout.tabs[i].close, x, y) {
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
        } else if inside(self.layout.content, x, y) && !self.tabs.is_empty() {
            self.focus = Focus::Terminal;
        }
    }

    fn mouse_up(&mut self, x: u16, y: u16) {
        self.drop_hover = None;
        match self.drag.take() {
            Some(Drag::Tab(from)) => {
                if let Some(to) = self.layout.tabs.iter().position(|t| inside(t.rect, x, y)) {
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
                    self.jump_open.insert(v);
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
            Some(NodeId::Folder(f)) => self.store.move_into(node, Some(f)),
            Some(NodeId::Server(s)) => match node {
                NodeId::Server(id) => self.store.move_server_before(id, s),
                NodeId::Folder(_) => {
                    let parent = self.store.server(s).and_then(|s| s.parent);
                    self.store.move_into(node, parent)
                }
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
                Some(Hit::Yes) => {
                    self.run_confirmed(c.action);
                    None
                }
                Some(Hit::No) => None,
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

fn secret_for(s: &Server) -> Option<String> {
    match s.auth {
        Auth::Agent => None,
        Auth::Key | Auth::Password => secrets::get(s.id),
    }
}

fn target(s: &Server) -> String {
    if s.user.is_empty() { s.host.clone() } else { format!("{}@{}", s.user, s.host) }
}

fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
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
    parts.extend(auth_args(hop));
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
    a.extend(["-o".into(), "ServerAliveInterval=30".into()]);
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
    fn argv_uses_proxy_command_when_a_hop_has_a_key() {
        let (st, t) = chain_store(Auth::Key);
        let a = ssh_argv(&st, &t);
        let pc = a.iter().find(|x| x.starts_with("ProxyCommand=")).expect("ProxyCommand");
        assert!(pc.contains("-i /k/bast") && pc.contains("-W %h:%p ops@bastion.example"), "{pc}");
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
