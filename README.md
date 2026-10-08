# ship

An SSH connection manager for the terminal: a modern TUI with mouse support. Keep your servers in folders, see them by jump host, and open every session in a tab. A lightweight alternative to Tabby.

> Status: Phase 1 (MVP) done, jump hosts included. Architecture: [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md).

## Install & develop

Requires a stable [Rust](https://rustup.rs) toolchain and an `ssh` client in the PATH.

```sh
cargo run                                   # run from the source tree
cargo test
cargo install --path . --root ~/.local      # installs `ship` into ~/.local/bin
```

Data lives in `~/.config/ship/servers.json` (or `$SHIP_CONFIG_DIR`). Passwords and passphrases go to the system keyring, never to that file.

## Two views of the same servers

Press `v` (or click the header) to switch:

- **Folders** — how you organize things.
- **Jump hosts** — how you reach them. A server with “Jump via” set to another server is nested under it, so chains like `bastion → web → db` become a tree. Opening it runs `ssh` through every hop.

## Keyboard (sidebar)

Navigate like a menu: `↓`/`↑` move, `→` goes into a folder or jump host, `←` folds it or goes back up, `Enter` opens a session.

| Key | Action |
|---|---|
| `↑` `↓` / `j` `k` | move selection |
| `→` / `l` | unfold, or step into the first child |
| `←` / `h` | fold, or go back to the parent |
| `Enter` | open server / toggle folder |
| `v` / `Tab` | switch view |
| `a` / `f` | new server / new folder (in the jump view, `a` on a server creates a host behind it) |
| `e` / `d` | edit / delete |
| `Alt+↑` `Alt+↓` | reorder among siblings |
| `q` | quit |

**Global:** `F6` panel ⇄ terminal · `Alt+←/→` switch tab · `Alt+Shift+←/→` move tab · `Alt+1..9` jump to tab · `Alt+W` close · `F2` rename · `Shift+PgUp/PgDn` scrollback.

**Mouse:** double-click opens a server or toggles a folder · drag to move (onto a folder: inside it; onto a server: right before it; in the jump view: behind that server, or drop on empty space to reach it directly) · drag tabs to reorder · ✕ closes a tab · double-click a tab renames it · wheel scrolls the history.

In the server form, `Ctrl+O` (or “Browse…”) opens a file browser that starts in `~/.ssh` and highlights private keys.

## Security

- A session is the system `ssh` inside a PTY, so `known_hosts`, the agent and your keys work as usual. New or changed host keys are asked inside the tab.
- A saved password/passphrase lives in the keyring and is sent once when `ssh` asks for it. Jump hosts should use keys or the agent.

## Roadmap

1. **MVP** — tree, forms, tabs, resize, errors, drag & drop, jump hosts. ✔
2. `ship <alias>` / `list` / `add`, `~/.ssh/config` import, Ctrl+K, auto-reconnect.
3. SFTP, tunnels, snippets, themes.
