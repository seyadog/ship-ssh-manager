//! The sound played when an AI agent finishes. Uses the system's player where there is one,
//! and the terminal bell otherwise.

use std::io::Write;
use std::process::{Command, Stdio};

pub fn ring() {
    std::thread::spawn(|| {
        if !play_sound() {
            let mut out = std::io::stdout();
            let _ = out.write_all(b"\x07");
            let _ = out.flush();
        }
    });
}

fn spawn(cmd: &str, args: &[&str]) -> bool {
    Command::new(cmd).args(args).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn().is_ok()
}

#[cfg(target_os = "macos")]
fn play_sound() -> bool {
    spawn("afplay", &["/System/Library/Sounds/Glass.aiff"])
}

#[cfg(windows)]
fn play_sound() -> bool {
    spawn("powershell", &["-NoProfile", "-Command", "[console]::beep(880,180)"])
}

#[cfg(all(unix, not(target_os = "macos")))]
fn play_sound() -> bool {
    let sounds = [
        "/usr/share/sounds/freedesktop/stereo/complete.oga",
        "/usr/share/sounds/freedesktop/stereo/message.oga",
        "/usr/share/sounds/freedesktop/stereo/bell.oga",
    ];
    if let Some(file) = sounds.iter().find(|p| std::path::Path::new(p).exists()) {
        if spawn("paplay", &[file]) || spawn("pw-play", &[file]) {
            return true;
        }
    }
    spawn("canberra-gtk-play", &["-i", "complete"])
}
