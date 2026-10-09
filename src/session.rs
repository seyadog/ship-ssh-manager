//! A session is a process (normally `ssh`) inside a PTY, with a `vt100` emulator holding the screen.
//!
//! `PtySession` is the real thing. `Session` is what the interface uses: it either owns a `PtySession`
//! itself, or talks to one living in the background server (`daemon`), which is what lets sessions
//! survive closing the interface.

use anyhow::{Context, Result};
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
use std::io::{Read, Write};
use crate::daemon::{Daemon, RemoteSession};
use std::path::PathBuf;
#[cfg(all(unix, not(target_os = "linux")))]
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;
use std::sync::{Arc, Mutex};
use zeroize::Zeroize;

pub const SCROLLBACK: usize = 5000;
/// How much raw output a session remembers, to give a late-attaching interface its scroll-back.
const HISTORY_BYTES: usize = 4 * 1024 * 1024;

/// The most recent raw output of a session.
#[derive(Default)]
pub struct History(Vec<u8>);

impl History {
    fn push(&mut self, bytes: &[u8]) {
        self.0.extend_from_slice(bytes);
        // Trim in big steps so the cost is amortised, and start again at a line boundary so that
        // replaying it does not begin in the middle of an escape sequence.
        if self.0.len() > HISTORY_BYTES + HISTORY_BYTES / 4 {
            let mut cut = self.0.len() - HISTORY_BYTES;
            if let Some(nl) = self.0[cut..].iter().take(8192).position(|&b| b == b'\n') {
                cut += nl + 1;
            }
            self.0.drain(..cut);
        }
    }

    #[cfg_attr(not(unix), allow(dead_code))]
    pub fn bytes(&self) -> &[u8] {
        &self.0
    }
}

type SharedWriter = Arc<Mutex<Box<dyn Write + Send>>>;

/// A saved secret to type when `ssh` shows the prompt that belongs to it. Each one is sent once, so a wrong
/// secret is never retried in a loop and a hop's password never goes to another host.
pub struct Autofill {
    /// Lowercase text that must appear on the prompt line: `user@host` for a password, the quoted key path for a passphrase.
    pub when: String,
    pub secret: String,
    /// May also answer a bare `Password:` prompt (the destination's keyboard-interactive login).
    pub fallback: bool,
}

/// Called with every chunk of output, while the screen is locked (so nothing can slip between a
/// snapshot of the screen and the output that follows it). The background server uses it to forward output.
pub type Tap = Arc<dyn Fn(&[u8]) + Send + Sync>;

/// Everything needed to start a session.
pub struct SpawnOpts {
    pub argv: Vec<String>,
    pub rows: u16,
    pub cols: u16,
    pub fills: Vec<Autofill>,
    pub login_expected: bool,
    pub cwd: Option<PathBuf>,
    /// If set, the child gets exactly this environment instead of ours (used by the background server).
    pub env: Option<Vec<(String, String)>>,
    pub tap: Option<Tap>,
}

pub struct PtySession {
    parser: Arc<Mutex<vt100::Parser>>,
    writer: SharedWriter,
    master: Box<dyn MasterPty + Send>,
    child: Box<dyn Child + Send + Sync>,
    size: (u16, u16),
    sudo_prompt: Arc<AtomicBool>,
    /// Set when the user types, so an identical prompt line is recognised again.
    typed: Arc<AtomicBool>,
    pub exit_code: Option<u32>,
    #[cfg_attr(not(unix), allow(dead_code))]
    history: Arc<Mutex<History>>,
}

impl PtySession {
    /// `argv[0]` is the program. Each `Autofill` is typed once, when the login or passphrase prompt that
    /// belongs to it appears. `login_expected` says the first generic `Password:` prompt is the login
    /// (password auth) rather than a later `su`.
    pub fn spawn_with(opts: SpawnOpts) -> Result<Self> {
        let SpawnOpts { argv, rows, cols, fills, login_expected, cwd, env, tap } = opts;
        let (rows, cols) = (rows.max(1), cols.max(1));
        let pair = native_pty_system()
            .openpty(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })
            .context("no se pudo abrir el PTY")?;

        let mut cmd = CommandBuilder::new(&argv[0]);
        cmd.args(&argv[1..]);
        if let Some(env) = env {
            cmd.env_clear();
            for (k, v) in env {
                cmd.env(k, v);
            }
        }
        if let Some(dir) = cwd.filter(|d| d.is_dir()) {
            cmd.cwd(dir);
        }
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        let child = pair.slave.spawn_command(cmd).with_context(|| format!("could not run `{}`", argv[0]))?;
        drop(pair.slave);

        let mut reader = pair.master.try_clone_reader()?;
        let writer: SharedWriter = Arc::new(Mutex::new(pair.master.take_writer()?));
        let parser = Arc::new(Mutex::new(vt100::Parser::new(rows, cols, SCROLLBACK)));

        let history = Arc::new(Mutex::new(History::default()));
        let sudo_prompt = Arc::new(AtomicBool::new(false));
        let typed_flag = Arc::new(AtomicBool::new(false));
        {
            let parser = Arc::clone(&parser);
            let writer = Arc::clone(&writer);
            let sudo_flag = Arc::clone(&sudo_prompt);
            let typed = Arc::clone(&typed_flag);
            let history = Arc::clone(&history);
            std::thread::spawn(move || {
                let mut buf = [0u8; 16 * 1024];
                let mut fills: Vec<Option<Autofill>> = fills.into_iter().map(Some).collect();
                let mut login_phase = login_expected;
                // The prompt line last acted on, so the same text is not handled twice.
                let mut last_seen: Option<(u16, String)> = None;
                while let Ok(n) = reader.read(&mut buf) {
                    if n == 0 {
                        break;
                    }
                    // Look at the interpreted screen, not the raw bytes: escape sequences
                    // and line wrapping would otherwise hide the prompt.
                    let line = {
                        let Ok(mut p) = parser.lock() else { break };
                        p.process(&buf[..n]);
                        history.lock().unwrap_or_else(|e| e.into_inner()).push(&buf[..n]);
                        if let Some(tap) = &tap {
                            tap(&buf[..n]);
                        }
                        let (row, col) = p.screen().cursor_position();
                        (row, p.screen().contents_between(row, 0, row, col).to_lowercase())
                    };
                    if typed.swap(false, Ordering::Relaxed) {
                        last_seen = None;
                    }
                    if last_seen.as_ref() == Some(&line) {
                        continue;
                    }
                    match classify_prompt(&line.1, login_phase) {
                        Some(Prompt::Login) => {
                            last_seen = Some(line.clone());
                            let text = &line.1;
                            let generic = !text.contains('@') && !text.contains("passphrase");
                            let pick = fills
                                .iter()
                                .position(|f| f.as_ref().is_some_and(|f| text.contains(&f.when)))
                                .or_else(|| {
                                    if !generic {
                                        return None;
                                    }
                                    fills.iter().position(|f| f.as_ref().is_some_and(|f| f.fallback))
                                });
                            if let Some(mut f) = pick.and_then(|i| fills[i].take()) {
                                if let Ok(mut w) = writer.lock() {
                                    let _ = w.write_all(format!("{}\r", f.secret).as_bytes());
                                    let _ = w.flush();
                                }
                                f.secret.zeroize();
                            }
                            // Stay in the login phase while a later hop or the destination still has a secret to give.
                            if fills.iter().all(|f| f.is_none()) {
                                login_phase = false;
                            }
                        }
                        Some(Prompt::Sudo) => {
                            last_seen = Some(line);
                            sudo_flag.store(true, Ordering::Relaxed);
                        }
                        None => {}
                    }
                }
            });
        }

        Ok(PtySession {
            parser,
            writer,
            master: pair.master,
            child,
            size: (rows, cols),
            sudo_prompt,
            typed: typed_flag,
            exit_code: None,
            history,
        })
    }

    pub fn size(&self) -> (u16, u16) {
        self.size
    }

    pub fn resize(&mut self, rows: u16, cols: u16) {
        let (rows, cols) = (rows.max(1), cols.max(1));
        if (rows, cols) == self.size {
            return;
        }
        self.size = (rows, cols);
        let _ = self.master.resize(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 });
        if let Ok(mut p) = self.parser.lock() {
            p.screen_mut().set_size(rows, cols);
        }
    }

    /// Is the remote asking for a `sudo`/`su` password right now?
    pub fn sudo_prompt(&self) -> bool {
        self.sudo_prompt.load(Ordering::Relaxed)
    }

    pub fn write(&self, bytes: &[u8]) {
        // Any input answers (or dismisses) the prompt.
        self.sudo_prompt.store(false, Ordering::Relaxed);
        self.typed.store(true, Ordering::Relaxed);
        if let Ok(mut w) = self.writer.lock() {
            let _ = w.write_all(bytes);
            let _ = w.flush();
        }
    }

    /// Sets `exit_code` if the process has exited.
    pub fn poll_exit(&mut self) {
        if self.exit_code.is_none() {
            if let Ok(Some(status)) = self.child.try_wait() {
                self.exit_code = Some(status.exit_code());
            }
        }
    }

    pub fn kill(&mut self) {
        let _ = self.child.kill();
    }

    /// The raw output remembered so far. Take it inside `with_screen` to have it match the screen exactly.
    #[cfg_attr(not(unix), allow(dead_code))]
    pub fn history(&self) -> Vec<u8> {
        self.history.lock().unwrap_or_else(|e| e.into_inner()).bytes().to_vec()
    }

    pub fn with_screen<R>(&self, f: impl FnOnce(&vt100::Screen) -> R) -> R {
        let p = self.parser.lock().unwrap_or_else(|e| e.into_inner());
        f(p.screen())
    }

    /// Scrolls the view through the history (`delta` > 0 goes up).
    pub fn scroll(&self, delta: i32) {
        if let Ok(mut p) = self.parser.lock() {
            let cur = p.screen().scrollback() as i32;
            p.screen_mut().set_scrollback((cur + delta).max(0) as usize);
        }
    }

    pub fn reset_scroll(&self) {
        if let Ok(mut p) = self.parser.lock() {
            p.screen_mut().set_scrollback(0);
        }
    }
}

impl Drop for PtySession {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

/// A terminal session as the interface sees it, wherever the process actually lives.
pub struct Session {
    pub exit_code: Option<u32>,
    pub started: Instant,
    backend: Backend,
}

enum Backend {
    Pty(PtySession),
    Remote(RemoteSession),
}

impl Session {
    #[cfg(all(test, unix))]
    pub fn spawn(argv: &[String], rows: u16, cols: u16, fills: Vec<Autofill>, login_expected: bool) -> Result<Self> {
        let opts = SpawnOpts { argv: argv.to_vec(), rows, cols, fills, login_expected, cwd: None, env: None, tap: None };
        Self::spawn_with(opts, None)
    }

    /// Starts a session in the background server if there is one, in this process otherwise.
    pub fn spawn_with(opts: SpawnOpts, daemon: Option<&Daemon>) -> Result<Self> {
        let backend = match daemon {
            Some(d) => Backend::Remote(d.spawn(opts)?),
            None => Backend::Pty(PtySession::spawn_with(opts)?),
        };
        Ok(Session { exit_code: None, started: Instant::now(), backend })
    }

    /// A session that already runs in the background server.
    pub fn from_remote(r: RemoteSession) -> Self {
        Session { exit_code: None, started: Instant::now(), backend: Backend::Remote(r) }
    }

    pub fn size(&self) -> (u16, u16) {
        match &self.backend {
            Backend::Pty(p) => p.size(),
            Backend::Remote(r) => r.size(),
        }
    }

    pub fn resize(&mut self, rows: u16, cols: u16) {
        match &mut self.backend {
            Backend::Pty(p) => p.resize(rows, cols),
            Backend::Remote(r) => r.resize(rows, cols),
        }
    }

    /// Is the remote asking for a `sudo`/`su` password right now?
    pub fn sudo_prompt(&self) -> bool {
        match &self.backend {
            Backend::Pty(p) => p.sudo_prompt(),
            Backend::Remote(r) => r.sudo_prompt(),
        }
    }

    pub fn write(&self, bytes: &[u8]) {
        match &self.backend {
            Backend::Pty(p) => p.write(bytes),
            Backend::Remote(r) => r.write(bytes),
        }
    }

    /// Sets `exit_code` if the process has exited.
    pub fn poll_exit(&mut self) {
        match &mut self.backend {
            Backend::Pty(p) => {
                p.poll_exit();
                self.exit_code = p.exit_code;
            }
            Backend::Remote(r) => self.exit_code = r.exit_code().or(self.exit_code),
        }
    }

    pub fn with_screen<R>(&self, f: impl FnOnce(&vt100::Screen) -> R) -> R {
        match &self.backend {
            Backend::Pty(p) => p.with_screen(f),
            Backend::Remote(r) => r.with_screen(f),
        }
    }

    /// Scrolls the view through the history (`delta` > 0 goes up).
    pub fn scroll(&self, delta: i32) {
        match &self.backend {
            Backend::Pty(p) => p.scroll(delta),
            Backend::Remote(r) => r.scroll(delta),
        }
    }

    pub fn reset_scroll(&self) {
        match &self.backend {
            Backend::Pty(p) => p.reset_scroll(),
            Backend::Remote(r) => r.reset_scroll(),
        }
    }

    /// Ends the session for good (closing its tab). Merely dropping a remote session only detaches.
    pub fn kill(&mut self) {
        match &mut self.backend {
            Backend::Pty(p) => p.kill(),
            Backend::Remote(r) => r.kill(),
        }
    }

    /// Tells the background server what this tab is (title, space, order...) so it can be rebuilt later.
    pub fn set_meta(&self, meta: &serde_json::Value) {
        if let Backend::Remote(r) = &self.backend {
            r.set_meta(meta);
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Prompt {
    /// ssh asking for the login password or a key passphrase.
    Login,
    /// The remote shell asking for a `sudo` / `su` password.
    Sudo,
}

/// Classifies a screen line (lowercased, up to the cursor) as a password prompt, if it is one.
fn classify_prompt(line: &str, login_phase: bool) -> Option<Prompt> {
    let t = line.trim_end();
    if !t.ends_with(':') {
        return None;
    }
    if t.starts_with("[sudo] password for") {
        return Some(Prompt::Sudo);
    }
    if t.contains("passphrase for") || t.ends_with("'s password:") {
        return Some(Prompt::Login);
    }
    if t.ends_with("password:") {
        return Some(if login_phase { Prompt::Login } else { Prompt::Sudo });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::time::{Duration, Instant};

    #[cfg(unix)]
    fn wait_for(s: &mut Session, want: &str) -> String {
        let end = Instant::now() + Duration::from_secs(5);
        loop {
            s.poll_exit();
            let text = s.with_screen(|sc| sc.contents());
            if text.contains(want) || Instant::now() > end {
                return text;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    #[cfg(unix)]
    fn runs_command_and_captures_output() {
        let argv = vec!["sh".into(), "-c".into(), "echo hola-ship; exit 3".into()];
        let mut s = Session::spawn(&argv, 24, 80, vec![], false).unwrap();
        let text = wait_for(&mut s, "hola-ship");
        assert!(text.contains("hola-ship"), "pantalla: {text:?}");
        let end = Instant::now() + Duration::from_secs(5);
        while s.exit_code.is_none() && Instant::now() < end {
            s.poll_exit();
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(s.exit_code, Some(3));
    }

    #[test]
    #[cfg(unix)]
    fn answers_password_prompt_once() {
        let script = "printf 'Password: '; read p; echo \"got:$p\"";
        let argv = vec!["sh".into(), "-c".into(), script.into()];
        let mut s = Session::spawn(&argv, 24, 80, vec![fill("", "s3cret", true)], true).unwrap();
        let text = wait_for(&mut s, "got:");
        assert!(text.contains("got:s3cret"), "pantalla: {text:?}");
    }

    #[cfg(unix)]
    fn fill(when: &str, secret: &str, fallback: bool) -> Autofill {
        Autofill { when: when.into(), secret: secret.into(), fallback }
    }

    /// A jump host asks first, then the destination: each gets its own password.
    #[test]
    #[cfg(unix)]
    fn each_hop_gets_its_own_password() {
        let script = "printf \"ops@bastion's password: \"; read a; printf \"\\napp@dest's password: \"; read b; echo \"got:$a:$b\"";
        let argv = vec!["sh".into(), "-c".into(), script.into()];
        let fills = vec![fill("app@dest", "dest-pw", true), fill("ops@bastion", "bastion-pw", false)];
        let mut s = Session::spawn(&argv, 24, 80, fills, true).unwrap();
        let text = wait_for(&mut s, "got:");
        assert!(text.contains("got:bastion-pw:dest-pw"), "pantalla: {text:?}");
    }

    /// The destination's password must never be typed into the jump host's prompt.
    #[test]
    #[cfg(unix)]
    fn destination_password_is_not_sent_to_a_hop_without_a_saved_secret() {
        let script = "printf \"ops@bastion's password: \"; read -t 2 a; echo \"got:[$a]\"";
        let argv = vec!["sh".into(), "-c".into(), script.into()];
        let mut s = Session::spawn(&argv, 24, 80, vec![fill("app@dest", "dest-pw", true)], true).unwrap();
        let text = wait_for(&mut s, "got:");
        assert!(text.contains("got:[]"), "pantalla: {text:?}");
    }

    #[test]
    #[cfg(unix)]
    fn flags_a_sudo_prompt_until_the_user_types() {
        let script = "printf '[sudo] password for alice: '; sleep 3";
        let argv = vec!["sh".into(), "-c".into(), script.into()];
        let s = Session::spawn(&argv, 24, 80, vec![], false).unwrap();
        let end = Instant::now() + Duration::from_secs(2);
        while !s.sudo_prompt() && Instant::now() < end {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(s.sudo_prompt());
        s.write(b"x");
        assert!(!s.sudo_prompt());
    }

    #[test]
    fn classifies_prompts() {
        assert_eq!(classify_prompt("user@host's password: ", true), Some(Prompt::Login));
        assert_eq!(classify_prompt("enter passphrase for key '/home/u/.ssh/id':", false), Some(Prompt::Login));
        assert_eq!(classify_prompt("(user@host) password: ", true), Some(Prompt::Login), "keyboard-interactive login");
        assert_eq!(classify_prompt("[sudo] password for alice: ", false), Some(Prompt::Sudo));
        assert_eq!(classify_prompt("[sudo] password for alice: ", true), Some(Prompt::Sudo));
        assert_eq!(classify_prompt("password: ", false), Some(Prompt::Sudo), "su after login");
        assert_eq!(classify_prompt("welcome to the server", true), None);
        assert_eq!(classify_prompt("alice@host:~$ ", false), None);
    }

    #[test]
    #[cfg(unix)]
    fn sudo_prompt_is_found_even_after_a_screen_clear() {
        let script = "printf '\\033[H\\033[2J[sudo] password for alice: '; sleep 3";
        let argv = vec!["sh".into(), "-c".into(), script.into()];
        let s = Session::spawn(&argv, 24, 80, vec![], false).unwrap();
        let end = Instant::now() + Duration::from_secs(2);
        while !s.sudo_prompt() && Instant::now() < end {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(s.sudo_prompt());
    }
}
