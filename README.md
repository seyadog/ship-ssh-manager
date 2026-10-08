# ship

An SSH connection manager for the terminal: a modern TUI with mouse support. Keep your servers in folders, see them by jump host, and open every session in a tab. A lightweight, keyboard- and mouse-friendly alternative to GUI SSH clients.

> Status: Phase 1 (MVP) done, with jump hosts and the encrypted password vault. Architecture: [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md).

## Install

Needs an `ssh` client in the PATH (preinstalled on macOS and most Linux distributions).

**Prebuilt binary** (Linux x86_64 / arm64, macOS Apple Silicon and Intel, Windows 11 x86_64 — experimental). Download the `.tar.gz` for your platform from the
[latest release](https://github.com/seyadog/ship-ssh-manager/releases/latest), or:

```sh
curl -fsSL https://raw.githubusercontent.com/seyadog/ship-ssh-manager/main/install.sh | sh
```

It installs `ship` into `~/.local/bin` (set `INSTALL_DIR` to change it). On macOS, if Gatekeeper blocks the binary:
`xattr -d com.apple.quarantine ~/.local/bin/ship`.

**Windows 11** (experimental, PowerShell; use Windows Terminal):

```powershell
irm https://raw.githubusercontent.com/seyadog/ship-ssh-manager/main/install.ps1 | iex
```

It installs `ship.exe` into `%LOCALAPPDATA%\ship\bin`. Needs the OpenSSH Client feature (on by default in Windows 11).
See [Windows notes](#windows-notes) for the limitations.

**From source** (stable [Rust](https://rustup.rs)):

```sh
cargo install --git https://github.com/seyadog/ship-ssh-manager --locked
```

Data lives in `~/.config/ship/servers.json` (`~/Library/Application Support/ship` on macOS, or `$SHIP_CONFIG_DIR`).
Passwords never go in that file: they live in the encrypted vault (`vault.json`, next to it).

## Windows notes

Windows support is **experimental**: it builds and passes the unit tests in CI, but has had little real-world testing.

- Use Windows Terminal. It uses the system `ssh.exe` (Settings → System → Optional features → OpenSSH Client).
- Jump hosts through the agent or default keys (`-J`) should work. Chains where a hop has its own key use a nested `ProxyCommand` run by `cmd.exe`, which is untested; deep chains may fail.
- Some `Alt` key combinations may be taken by Windows or the terminal; the mouse and arrow keys always work.
- The vault has no extra file permissions on Windows (no `0600`); it is still encrypted with your master password.
- The binary is not code-signed, so SmartScreen may warn on first run. Windows on ARM uses the x86_64 build via emulation.
- Data lives in `%APPDATA%\ship\config` (or `$env:SHIP_CONFIG_DIR`).

## Develop

```sh
cargo run
cargo test
```

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
| `c` | close the open session of the selected server |
| `Alt+↑` `Alt+↓` | reorder among siblings |
| `p` | open the password vault |
| `q` | quit |

**Global:** `F6` panel ⇄ terminal · `Alt+←/→` switch tab · `Alt+Shift+←/→` move tab · `Alt+1..9` jump to tab · `Alt+W` close · `F2` rename · `Shift+PgUp/PgDn` scrollback.

**Mouse:** double-click opens a server or toggles a folder · drag to move (onto a folder: inside it; onto a server: right before it; in the jump view: behind that server, or drop on empty space to reach it directly) · drag tabs to reorder · ✕ closes a tab · double-click a tab renames it · wheel scrolls the history.

In the server form, `Ctrl+O` (or “Browse…”) opens a file browser that starts in `~/.ssh` and highlights private keys.

## Import from Tabby

`ship import-tabby [path/to/config.yaml]` (default `~/.config/tabby/config.yaml`) copies SSH profiles and groups into ship:
host, port, user, private key path, folders and jump hosts. Re-running it skips servers already present. Passwords are
not imported (Tabby keeps them in its own vault); save them in ship's vault afterwards.

## Password vault

Passwords you type in the server form (the login password or key passphrase, and an optional **sudo password**) are stored in an encrypted vault, protected by a master password.

- **First use** asks you to create the master password. Pick one you will not forget: **there is no recovery**.
- It is asked only when needed (connecting to a server with a saved password, opening the vault, saving a secret) and the vault locks itself after 5 idle minutes.
- `p` opens the vault. `↑↓` pick a server and `←→` pick the column (login / sudo); then `r` reveals it for 8 seconds, `c` copies it (cleared from the clipboard after 30 s), `e` changes it (`Ctrl+T` shows what you type) and `d` removes it after a confirmation. `m` changes the master password.
- In the server form, type a new value over a saved one to replace it, or press `Ctrl+X` on the field to remove it when you save. Deleting a server removes its secrets too.
- **sudo:** when a remote `sudo` or `su` asks for a password and one is saved for that server, a bar offers it. `Alt+P` fills it from the vault, and typing anything dismisses it. It is never sent without that keypress.
- Clipboard copy uses the terminal (OSC 52); inside tmux enable `set -g set-clipboard on`. Some terminals do not support it.

## Security

- A session is the system `ssh` inside a PTY, so `known_hosts`, the agent and your keys work as usual. New or changed host keys are asked inside the tab.
- The vault is a single file sealed with XChaCha20-Poly1305; the key comes from the master password via Argon2id (64 MiB, 3 passes). The master password and key exist only in memory while unlocked. The file is created with `0600` permissions, and `servers.json` only records *that* a secret exists.
- A saved login secret is sent once when `ssh` asks for it. Jump hosts should use keys or the agent.

## License

MIT, see [LICENSE](LICENSE).

## Roadmap

1. **MVP** — tree, forms, tabs, resize, errors, drag & drop, jump hosts, password vault (with sudo fill). ✔
2. **Next:** `ship <alias>` / `list` / `add`, `~/.ssh/config` import (mapping `ProxyJump` to jump hosts), Ctrl+K quick search, auto-reconnect.
3. **Later:** SFTP, saved tunnels, snippets, themes (including per-server colors), remembering the chosen view.
