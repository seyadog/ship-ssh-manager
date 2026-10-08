//! Detecting an AI agent running in a terminal, and when it finishes.
//!
//! The agent is recognised by the foreground process of the PTY (`claude`, `opencode`, ...).
//! "Working" is inferred from output: an agent that keeps drawing is busy; when it goes quiet
//! after a stretch of work, it is done.

/// Programs treated as agents. Matched against the file name of the command or of its script.
const AGENTS: &[&str] =
    &["claude", "opencode", "codex", "gemini", "aider", "amp", "crush", "goose", "cursor-agent", "qwen"];

/// Programs that run an agent that is a script.
const INTERPRETERS: &[&str] = &["node", "nodejs", "bun", "deno", "python", "python3", "npx"];

/// Output newer than this means the agent is working.
const QUIET_AFTER_MS: u64 = 1500;
/// Output this soon after a keypress is just the echo of typing, not work.
const ECHO_MS: u64 = 1200;
/// Work shorter than this ends silently (no sound).
const MIN_WORK_MS: u64 = 3000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentInfo {
    pub name: String,
    pub working: bool,
}

/// The agent a command line belongs to, if any: `claude`, `node /x/bin/claude`, `/usr/bin/opencode run`.
pub fn agent_from_args(args: &[String]) -> Option<&'static str> {
    let base = |a: &str| {
        let b = a.rsplit(['/', '\\']).next().unwrap_or(a).to_string();
        for ext in [".js", ".mjs", ".exe"] {
            if let Some(stripped) = b.strip_suffix(ext) {
                return stripped.to_string();
            }
        }
        b
    };
    let first = base(args.first()?);
    if let Some(n) = AGENTS.iter().copied().find(|n| *n == first) {
        return Some(n);
    }
    // `node /path/to/claude`: the script is what counts, but only behind a known interpreter, so that
    // `grep claude notes` or `vim claude.md` are not mistaken for an agent.
    if INTERPRETERS.contains(&first.as_str()) {
        let script = base(args.get(1)?);
        return AGENTS.iter().copied().find(|n| *n == script);
    }
    None
}

/// Command line of a process (empty if it cannot be read).
#[cfg(target_os = "linux")]
fn process_args(pid: i32) -> Vec<String> {
    std::fs::read(format!("/proc/{pid}/cmdline"))
        .map(|b| b.split(|&c| c == 0).filter(|s| !s.is_empty()).map(|s| String::from_utf8_lossy(s).into_owned()).collect())
        .unwrap_or_default()
}

#[cfg(all(unix, not(target_os = "linux")))]
fn process_args(pid: i32) -> Vec<String> {
    std::process::Command::new("ps")
        .args(["-o", "args=", "-p", &pid.to_string()])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).split_whitespace().map(String::from).collect())
        .unwrap_or_default()
}

/// The agent in the foreground of a PTY, if any. Unix only.
#[cfg(unix)]
pub fn foreground_agent(master: &dyn portable_pty::MasterPty) -> Option<&'static str> {
    let pid = master.process_group_leader()?;
    agent_from_args(&process_args(pid))
}

#[cfg(not(unix))]
pub fn foreground_agent(_master: &dyn portable_pty::MasterPty) -> Option<&'static str> {
    None
}

/// Working/finished state machine, fed with the age of the last output and keypress.
#[derive(Default)]
pub struct Tracker {
    pub working: bool,
    since: Option<u64>,
}

impl Tracker {
    /// Advances the state. Returns true when the agent has just finished a stretch of real work.
    pub fn step(&mut self, now: u64, last_output: u64, last_input: u64, present: bool) -> bool {
        if !present {
            self.working = false;
            self.since = None;
            return false;
        }
        let recent_output = now.saturating_sub(last_output) < QUIET_AFTER_MS;
        let typing = now.saturating_sub(last_input) < ECHO_MS;
        if recent_output && !typing {
            if !self.working {
                self.working = true;
                self.since = Some(now);
            }
            false
        } else if !recent_output && self.working {
            self.working = false;
            self.since.take().is_some_and(|s| now.saturating_sub(s) >= MIN_WORK_MS)
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn recognises_agents_by_command_or_script() {
        assert_eq!(agent_from_args(&args(&["claude"])), Some("claude"));
        assert_eq!(agent_from_args(&args(&["/home/u/.local/bin/claude", "--resume"])), Some("claude"));
        assert_eq!(agent_from_args(&args(&["node", "/usr/lib/node_modules/opencode/bin/opencode.js"])), Some("opencode"));
        assert_eq!(agent_from_args(&args(&["C:\\bin\\codex.exe"])), Some("codex"));
    }

    #[test]
    fn ignores_things_that_only_mention_an_agent() {
        assert_eq!(agent_from_args(&args(&["vim", "claude.md"])), None);
        assert_eq!(agent_from_args(&args(&["bash"])), None);
        assert_eq!(agent_from_args(&args(&["grep", "claude", "notes"])), None);
        assert_eq!(agent_from_args(&args(&["node", "server.js"])), None);
        assert_eq!(agent_from_args(&[]), None);
    }

    #[test]
    fn done_after_a_long_stretch_of_output_goes_quiet() {
        let mut t = Tracker::default();
        // The agent draws for 5 s, then stops.
        for now in (0..=5000).step_by(100) {
            assert!(!t.step(now, now, 0, true));
        }
        assert!(t.working);
        assert!(!t.step(6000, 5000, 0, true), "still within the quiet window");
        assert!(t.step(6600, 5000, 0, true), "quiet for 1.6 s: done");
        assert!(!t.working);
        assert!(!t.step(7000, 5000, 0, true), "reported once");
    }

    #[test]
    fn short_bursts_and_typing_do_not_ring() {
        let mut t = Tracker::default();
        // A 1 s burst of output is not real work.
        for now in (0..=1000).step_by(100) {
            t.step(now, now, 0, true);
        }
        assert!(!t.step(3000, 1000, 0, true));
        // Echo of the user's own typing is not work either.
        let mut t = Tracker::default();
        for now in (10_000..=10_900).step_by(100) {
            assert!(!t.step(now, now, now, true));
        }
        assert!(!t.working);
    }

    #[test]
    fn no_agent_no_state() {
        let mut t = Tracker::default();
        t.step(10_000, 10_000, 0, true);
        t.step(10_100, 10_100, 0, true);
        assert!(t.working);
        assert!(!t.step(20_000, 10_100, 0, false));
        assert!(!t.working);
    }
}
