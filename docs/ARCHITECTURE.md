# ship architecture

A Rust TUI (ratatui + crossterm) for managing SSH connections. Mouse support; runs on Linux, macOS and Windows.

## Core idea

Each tab runs the system `ssh` inside a PTY (`portable-pty`). Its output goes through a terminal emulator (`vt100`) and is drawn into a ratatui widget. `ship` therefore inherits for free:

- the SSH agent, keys, `~/.ssh/config`, ProxyJump
- `known_hosts`: ssh asks about new or changed host keys **inside the tab** and records them itself
- the real client error messages (unreachable host, permission denied, ...)

`ship` adds the server tree, tabs, forms, jump-host routing and a clear summary when a session ends.

## Modules

| Module | Responsibility |
|---|---|
| `main.rs` | Terminal setup (raw mode, mouse, paste), event loop |
| `store.rs` | Model (folders, servers, jump chains) and JSON persistence in `~/.config/ship/servers.json` |
| `session.rs` | PTY + emulator: spawn, resize, input, scrollback, exit detection, one-shot secret autofill |
| `vault.rs` | Encrypted password vault (Argon2id + XChaCha20-Poly1305), master-password lock |
| `keys.rs` | Maps crossterm key events to terminal bytes |
| `app.rs` | App state, focus, modals, keyboard and mouse handling, `ssh` argv building |
| `ui.rs` | Drawing: sidebar, tabs, terminal, forms |

Everything runs in one process; there is no IPC and no separate renderer.

## Folders vs. jump hosts

A server has two independent attributes: `parent` (its folder, for organizing) and `jump` (the server it is reached through, for routing). The sidebar is one tree with two lenses over the same data, so nothing is duplicated:

- **Folders view** — roots are top-level folders; jump servers show a dim `↪ bastion` hint.
- **Jump hosts view** — roots are servers with no `jump`; children are the servers behind them, to any depth.

`ssh_argv` resolves the chain. If every hop uses the agent/default keys it emits `-J a,b`. If a hop has its own key it builds a nested `ProxyCommand`, because `-J` cannot give each hop an identity. Loops are rejected (`Store::would_cycle`), and deleting a bastion detaches the servers behind it.

## Security

- No password is written to disk in the clear. Secrets live in `vault.json`, sealed with XChaCha20-Poly1305 under a key derived from the master password (Argon2id). The key exists only in memory while the vault is unlocked, and it relocks after 5 idle minutes. `servers.json` only stores booleans (`has_secret`, `has_sudo`).
- A login secret is written to the PTY once when `ssh` prompts. Prompts are recognised on the interpreted `vt100` screen line, so escape sequences and wrapping cannot hide them.
- A `sudo`/`su` password is only offered (banner) and sent when the user presses `Alt+P`.
- Keys and the agent are preferred; the data file only stores the *path* of a key.

## Phases

1. **MVP:** tree, form, tabs, resize, clear errors, drag & drop, jump hosts.
2. **CLI and search:** `ship <alias>`, `ship list`, `ship add`, `~/.ssh/config` import, Ctrl+K, auto-reconnect.
3. **Extras:** SFTP, saved tunnels, snippets, themes.
