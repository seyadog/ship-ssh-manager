# oso architecture

A Rust TUI (ratatui + crossterm) for managing SSH connections. Mouse support; runs on Linux, macOS and Windows.

## Core idea

Each tab runs the system `ssh` inside a PTY (`portable-pty`). Its output goes through a terminal emulator (`vt100`) and is drawn into a ratatui widget. `oso` therefore inherits for free:

- the SSH agent, keys, `~/.ssh/config`, ProxyJump
- `known_hosts`: ssh asks about new or changed host keys **inside the tab** and records them itself
- the real client error messages (unreachable host, permission denied, ...)

`oso` adds the server tree, tabs, forms, jump-host routing and a clear summary when a session ends.

## Modules

| Module | Responsibility |
|---|---|
| `main.rs` | Terminal setup (raw mode, mouse, paste), event loop |
| `store.rs` | Model (folders, servers, jump chains) and JSON persistence in `~/.config/oso/servers.json` |
| `session.rs` | `PtySession`: a PTY + emulator (spawn, resize, input, scrollback, exit, per-prompt secret autofill, agent probe). `Session`: what the interface uses; it owns a `PtySession` or talks to one in the server |
| `vault.rs` | Encrypted password vault (Argon2id + XChaCha20-Poly1305), master-password lock |
| `spaces.rs` | Spaces (a name and the directory their terminal was left in, saved in `spaces.json`) and the git branch of a directory |
| `agent.rs` | Recognising an AI agent from the foreground process of a PTY, and the working/finished state machine |
| `clipboard.rs` | Copying to the system clipboard: OSC 52 plus the platform's clipboard tool |
| `notify.rs` | The sound played when an agent finishes |
| `daemon.rs` | The background server (Unix): protocol, server, client, `RemoteSession`. `daemon_stub.rs` stands in on other platforms |
| `settings.rs` | `settings.json` |
| `keys.rs` | Maps crossterm key events to terminal bytes |
| `app.rs` | App state, focus, modals, keyboard and mouse handling, `ssh` argv building |
| `ui.rs` | Drawing: sidebar, tabs, terminal, forms |

Everything runs in one process; there is no IPC and no separate renderer.

## Folders vs. jump hosts

A server has two independent attributes: `parent` (its folder, for organizing) and `jump` (the server it is reached through, for routing). The SSH view shows both over the same data, so nothing is duplicated:

- **Folder tree** — roots are top-level folders; servers reached through a bastion show a dim `↪ bastion` hint.
- **Bastions section** (the lower half of the list, starting at the middle: the upper half is the folder tree, each half scrolls on its own; with a fixed title row that is only a label and is skipped by the selection; only when there are bastions) — roots are bastions only (servers with no `jump` that other servers are reached through); children are the servers behind them, to any depth. Its rows are flagged `jump` so that fold, reorder, drag and "new server" act on the right tree.

`ssh_argv` resolves the chain. If every hop uses the agent/default keys it emits `-J a,b`. If a hop has its own key it builds a nested `ProxyCommand`, because `-J` cannot give each hop an identity. Loops are rejected (`Store::would_cycle`), and deleting a bastion detaches the servers behind it.

## Tabs, scopes and spaces (the Projects view)

Every tab has a `Scope`: the SSH section, or one space. The tab bar shows every tab, like a browser. The scope is where the sidebar is: the SSH view is the SSH scope, and in the Projects view (the spaces) it is the selected project. Choosing a tab (`select_tab`) moves the sidebar to the tab's scope; moving the sidebar to a scope brings back the tab last used there. A terminal opened in a space starts in that space's directory, and the space follows its terminal: the directory of the shell (the PTY's child, read from `/proc` or `lsof` about once a second, by the process that owns the PTY) becomes the space's directory, and its name too until the user renames it. Keeping tabs per scope is what keeps an AI agent where it was launched and out of the SSH tabs.

## The background server

The PTYs and their `vt100` screens live in `oso daemon`, a process started on demand (`setsid`, detached from the terminal). The interface talks to it over a Unix socket with one JSON object per line (terminal bytes in base64).

- Each session also keeps its last few MiB of raw output. `Attach` replays it into the client's emulator before the snapshot, which is what rebuilds the scroll-back; the snapshot (which starts by clearing the screen) then sets the exact screen and modes.
- `Spawn` creates a session (with the client's environment, working directory and the secrets to autofill); `Attach` returns a **snapshot** of the screen (`state_formatted`) and then streams `Output`. Snapshot and subscription happen while the screen is locked, so no output is lost or repeated.
- The server watches its sessions twice a second: exit codes, sudo prompts, and the agent in the foreground (and whether it is working or just finished). If an agent finishes with no interface attached, it plays the sound itself and remembers it for the next attach.
- Each tab sends a small description of itself (`SetMeta`: title, space, order) whenever it changes; that is how the next interface rebuilds the tabs in the right scope.
- A client that cannot keep up is disconnected rather than allowed to miss output, and reconnects to a fresh snapshot. The server exits after a few idle seconds with no sessions and no clients, or on `Shutdown` (`oso kill-server`).
- Closing a tab sends `Kill`; just dropping the connection only detaches. On platforms without Unix sockets, `daemon_stub.rs` makes the interface keep sessions in its own process.

## Security

- No password is written to disk in the clear. Secrets live in `vault.json`, sealed with XChaCha20-Poly1305 under a key derived from the master password (Argon2id). The key exists only in memory while the vault is unlocked, and it relocks after 5 idle minutes. `servers.json` only stores booleans (`has_secret`, `has_sudo`).
- A login secret is written to the PTY once when `ssh` prompts. Prompts are recognised on the interpreted `vt100` screen line, so escape sequences and wrapping cannot hide them.
- A `sudo`/`su` password is only offered (banner) and sent when the user presses `Alt+P`.
- Keys and the agent are preferred; the data file only stores the *path* of a key.

## Phases

1. **MVP:** tree, form, tabs, resize, clear errors, drag & drop, jump hosts.
2. **CLI and search:** `oso <alias>`, `oso list`, `oso add`, `~/.ssh/config` import, Ctrl+K, auto-reconnect.
3. **Extras:** SFTP, saved tunnels, snippets, themes.
