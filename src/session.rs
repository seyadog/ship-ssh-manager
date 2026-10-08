//! A session is a process (normally `ssh`) inside a PTY, with a `vt100` emulator
//! holding the screen. A reader thread feeds the emulator.

use anyhow::{Context, Result};
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

const SCROLLBACK: usize = 5000;

type SharedWriter = Arc<Mutex<Box<dyn Write + Send>>>;

pub struct Session {
    parser: Arc<Mutex<vt100::Parser>>,
    writer: SharedWriter,
    master: Box<dyn MasterPty + Send>,
    child: Box<dyn Child + Send + Sync>,
    size: (u16, u16),
    sudo_prompt: Arc<AtomicBool>,
    /// Set when the user types, so an identical prompt line is recognised again.
    typed: Arc<AtomicBool>,
    pub exit_code: Option<u32>,
    pub started: std::time::Instant,
}

impl Session {
    /// `argv[0]` is the program. If `secret` is set, it is sent once when a login password or
    /// passphrase prompt appears. `login_expected` says the first generic `Password:` prompt is
    /// the login (password auth) rather than a later `su`.
    pub fn spawn(argv: &[String], rows: u16, cols: u16, secret: Option<String>, login_expected: bool) -> Result<Self> {
        let (rows, cols) = (rows.max(1), cols.max(1));
        let pair = native_pty_system()
            .openpty(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })
            .context("no se pudo abrir el PTY")?;

        let mut cmd = CommandBuilder::new(&argv[0]);
        cmd.args(&argv[1..]);
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        let child = pair.slave.spawn_command(cmd).with_context(|| format!("could not run `{}`", argv[0]))?;
        drop(pair.slave);

        let mut reader = pair.master.try_clone_reader()?;
        let writer: SharedWriter = Arc::new(Mutex::new(pair.master.take_writer()?));
        let parser = Arc::new(Mutex::new(vt100::Parser::new(rows, cols, SCROLLBACK)));

        let sudo_prompt = Arc::new(AtomicBool::new(false));
        let typed_flag = Arc::new(AtomicBool::new(false));
        {
            let parser = Arc::clone(&parser);
            let writer = Arc::clone(&writer);
            let sudo_flag = Arc::clone(&sudo_prompt);
            let typed = Arc::clone(&typed_flag);
            std::thread::spawn(move || {
                let mut buf = [0u8; 16 * 1024];
                let mut secret = secret;
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
                            last_seen = Some(line);
                            login_phase = false;
                            // Only once: a wrong secret must not be retried in a loop.
                            if let Some(s) = secret.take() {
                                if let Ok(mut w) = writer.lock() {
                                    let _ = w.write_all(format!("{s}\r").as_bytes());
                                    let _ = w.flush();
                                }
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

        Ok(Session {
            parser,
            writer,
            master: pair.master,
            child,
            size: (rows, cols),
            sudo_prompt,
            typed: typed_flag,
            exit_code: None,
            started: std::time::Instant::now(),
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

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.child.kill();
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
        let mut s = Session::spawn(&argv, 24, 80, None, false).unwrap();
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
        let mut s = Session::spawn(&argv, 24, 80, Some("s3cret".into()), true).unwrap();
        let text = wait_for(&mut s, "got:");
        assert!(text.contains("got:s3cret"), "pantalla: {text:?}");
    }

    #[test]
    #[cfg(unix)]
    fn flags_a_sudo_prompt_until_the_user_types() {
        let script = "printf '[sudo] password for alice: '; sleep 3";
        let argv = vec!["sh".into(), "-c".into(), script.into()];
        let s = Session::spawn(&argv, 24, 80, None, false).unwrap();
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
        let s = Session::spawn(&argv, 24, 80, None, false).unwrap();
        let end = Instant::now() + Duration::from_secs(2);
        while !s.sudo_prompt() && Instant::now() < end {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(s.sudo_prompt());
    }
}
