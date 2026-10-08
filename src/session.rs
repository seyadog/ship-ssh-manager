//! Una sesión = un proceso (normalmente `ssh`) dentro de un PTY, con un emulador `vt100`
//! que mantiene la pantalla. Un hilo lector alimenta al emulador.

use anyhow::{Context, Result};
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};

const SCROLLBACK: usize = 5000;

type SharedWriter = Arc<Mutex<Box<dyn Write + Send>>>;

pub struct Session {
    parser: Arc<Mutex<vt100::Parser>>,
    writer: SharedWriter,
    master: Box<dyn MasterPty + Send>,
    child: Box<dyn Child + Send + Sync>,
    size: (u16, u16),
    pub exit_code: Option<u32>,
}

impl Session {
    /// `argv[0]` es el programa. Si `secret` existe, se envía una vez cuando aparezca
    /// un prompt de contraseña o passphrase.
    pub fn spawn(argv: &[String], rows: u16, cols: u16, secret: Option<String>) -> Result<Self> {
        let (rows, cols) = (rows.max(1), cols.max(1));
        let pair = native_pty_system()
            .openpty(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })
            .context("no se pudo abrir el PTY")?;

        let mut cmd = CommandBuilder::new(&argv[0]);
        cmd.args(&argv[1..]);
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        let child = pair
            .slave
            .spawn_command(cmd)
            .with_context(|| format!("no se pudo ejecutar `{}`", argv[0]))?;
        drop(pair.slave);

        let mut reader = pair.master.try_clone_reader()?;
        let writer: SharedWriter = Arc::new(Mutex::new(pair.master.take_writer()?));
        let parser = Arc::new(Mutex::new(vt100::Parser::new(rows, cols, SCROLLBACK)));

        {
            let parser = Arc::clone(&parser);
            let writer = Arc::clone(&writer);
            std::thread::spawn(move || {
                let mut buf = [0u8; 16 * 1024];
                let mut tail = String::new();
                let mut secret = secret;
                while let Ok(n) = reader.read(&mut buf) {
                    if n == 0 {
                        break;
                    }
                    if let Ok(mut p) = parser.lock() {
                        p.process(&buf[..n]);
                    }
                    if secret.is_some() {
                        tail.push_str(&String::from_utf8_lossy(&buf[..n]).to_lowercase());
                        if tail.len() > 256 {
                            tail = tail[tail.len() - 256..].to_string();
                        }
                        if asks_for_secret(&tail) {
                            // Una sola vez: si es incorrecta, no se reintenta en bucle.
                            if let Some(s) = secret.take() {
                                if let Ok(mut w) = writer.lock() {
                                    let _ = w.write_all(format!("{s}\r").as_bytes());
                                    let _ = w.flush();
                                }
                            }
                        }
                    }
                }
            });
        }

        Ok(Session { parser, writer, master: pair.master, child, size: (rows, cols), exit_code: None })
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

    pub fn write(&self, bytes: &[u8]) {
        if let Ok(mut w) = self.writer.lock() {
            let _ = w.write_all(bytes);
            let _ = w.flush();
        }
    }

    /// Actualiza `exit_code` si el proceso terminó.
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

    /// Desplaza la vista por el historial (`delta` > 0 sube).
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

/// ¿El texto (en minúsculas) termina con un prompt de contraseña o passphrase?
fn asks_for_secret(tail: &str) -> bool {
    let t = tail.trim_end();
    t.ends_with("password:") || (t.contains("passphrase for") && t.ends_with(':'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

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
    fn runs_command_and_captures_output() {
        let argv = vec!["sh".into(), "-c".into(), "echo hola-ship; exit 3".into()];
        let mut s = Session::spawn(&argv, 24, 80, None).unwrap();
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
    fn answers_password_prompt_once() {
        let script = "printf 'Password: '; read p; echo \"got:$p\"";
        let argv = vec!["sh".into(), "-c".into(), script.into()];
        let mut s = Session::spawn(&argv, 24, 80, Some("s3cret".into())).unwrap();
        let text = wait_for(&mut s, "got:");
        assert!(text.contains("got:s3cret"), "pantalla: {text:?}");
    }

    #[test]
    fn detects_prompts() {
        assert!(asks_for_secret("user@host's password: "));
        assert!(asks_for_secret("enter passphrase for key '/home/u/.ssh/id':"));
        assert!(!asks_for_secret("welcome to the server"));
    }
}
