//! Copying text to the system clipboard.

/// Copies `text` in two ways, because neither works everywhere: through the terminal (OSC 52, which
/// even works over SSH) and through the platform's clipboard tool.
pub fn copy(text: &str) {
    #[cfg(test)]
    LAST.with(|l| *l.borrow_mut() = text.to_string()); // tests must not touch the real clipboard
    #[cfg(not(test))]
    real::copy(text);
}

// What the last `copy` of this thread was given (tests only).
#[cfg(test)]
thread_local! {
    pub static LAST: std::cell::RefCell<String> = const { std::cell::RefCell::new(String::new()) };
}

#[cfg(not(test))]
mod real {
    use base64::{Engine, engine::general_purpose::STANDARD as B64};
    use std::io::Write;
    use std::process::{Command, Stdio};

    pub fn copy(text: &str) {
        let mut out = std::io::stdout();
        let _ = write!(out, "\x1b]52;c;{}\x07", B64.encode(text));
        let _ = out.flush();
        let text = text.to_string();
        std::thread::spawn(move || {
            for (cmd, args) in tools() {
                if pipe(cmd, args, &text) {
                    return;
                }
            }
        });
    }

    #[cfg(target_os = "macos")]
    fn tools() -> Vec<(&'static str, &'static [&'static str])> {
        vec![("pbcopy", &[])]
    }

    #[cfg(windows)]
    fn tools() -> Vec<(&'static str, &'static [&'static str])> {
        vec![("clip", &[])]
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    fn tools() -> Vec<(&'static str, &'static [&'static str])> {
        let mut v: Vec<(&'static str, &'static [&'static str])> = vec![];
        if std::env::var_os("WAYLAND_DISPLAY").is_some() {
            v.push(("wl-copy", &[]));
        }
        v.push(("xclip", &["-selection", "clipboard"]));
        v.push(("xsel", &["--clipboard", "--input"]));
        v
    }

    /// Feeds `text` to a clipboard tool. False if the tool is not there.
    fn pipe(cmd: &str, args: &[&str], text: &str) -> bool {
        let Ok(mut child) =
            Command::new(cmd).args(args).stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null()).spawn()
        else {
            return false;
        };
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(text.as_bytes());
        }
        let _ = child.wait();
        true
    }
}
