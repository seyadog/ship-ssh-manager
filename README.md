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

Data lives in `~/.config/ship/servers.json` (or `$SHIP_CONFIG_DIR`). Passwords never go in that file: they live in the encrypted vault (`vault.json`, next to it).

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
| `p` | open the password vault |
| `q` | quit |

**Global:** `F6` panel ⇄ terminal · `Alt+←/→` switch tab · `Alt+Shift+←/→` move tab · `Alt+1..9` jump to tab · `Alt+W` close · `F2` rename · `Shift+PgUp/PgDn` scrollback.

**Mouse:** double-click opens a server or toggles a folder · drag to move (onto a folder: inside it; onto a server: right before it; in the jump view: behind that server, or drop on empty space to reach it directly) · drag tabs to reorder · ✕ closes a tab · double-click a tab renames it · wheel scrolls the history.

In the server form, `Ctrl+O` (or “Browse…”) opens a file browser that starts in `~/.ssh` and highlights private keys.

## Password vault

Passwords you type in the server form (the login password or key passphrase, and an optional **sudo password**) are stored in an encrypted vault, protected by a master password.

- **First use** asks you to create the master password. Pick one you will not forget: **there is no recovery**.
- It is asked only when needed (connecting to a server with a saved password, opening the vault, saving a secret) and the vault locks itself after 5 idle minutes.
- `p` opens the vault. `↑↓` pick a server and `←→` pick the column (login / sudo); then `r` reveals it for 8 seconds, `c` copies it (cleared from the clipboard after 30 s), `e` changes it (`Ctrl+T` shows what you type) and `d` removes it after a confirmation. `m` changes the master password.
- In the server form, type a new value over a saved one to replace it, or press `Ctrl+X` on the field to remove it when you save. Deleting a server removes its secrets too.
- **sudo:** when a remote `sudo` or `su` asks for a password and one is saved for that server, a bar offers it. `Alt+P` fills it from the vault, and typing anything dismisses it. It is never sent without that keypress.
- Passwords saved by older versions in the system keyring are moved into the vault the first time you unlock it.
- Clipboard copy uses the terminal (OSC 52); inside tmux enable `set -g set-clipboard on`. Some terminals do not support it.

## Security

- A session is the system `ssh` inside a PTY, so `known_hosts`, the agent and your keys work as usual. New or changed host keys are asked inside the tab.
- The vault is a single file sealed with XChaCha20-Poly1305; the key comes from the master password via Argon2id (64 MiB, 3 passes). The master password and key exist only in memory while unlocked. The file is created with `0600` permissions, and `servers.json` only records *that* a secret exists.
- A saved login secret is sent once when `ssh` asks for it. Jump hosts should use keys or the agent.

## Roadmap

1. **MVP** — tree, forms, tabs, resize, errors, drag & drop, jump hosts. ✔
2. `ship <alias>` / `list` / `add`, `~/.ssh/config` import, Ctrl+K, auto-reconnect.
3. SFTP, tunnels, snippets, themes.
