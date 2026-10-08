# ship

An SSH connection manager and terminal workspace: a modern TUI with mouse support. Keep your servers in folders, see them by jump host, open every session in a tab, and group local terminals and AI agents into project **spaces**. Sessions keep running when you close it. A lightweight, keyboard- and mouse-friendly alternative to GUI SSH clients.

> Status: servers, folders, jump hosts, the encrypted password vault, agents (project workspaces) with a running panel, and sessions that survive closing the window (Linux and macOS). Architecture: [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md).

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

## Agents: projects with their own terminals

An **agent** is a project with its own terminals (what earlier versions called a *space*). Press `v` (or click `Agents`, the first view) and then `a`: a normal terminal opens, and you move around in it like in any terminal (`cd`, `mkdir`, `git clone`...). **The space stays in the last directory you leave it in**: that directory gives it its name and shows its **git branch**, and new terminals of the space start there. Rename it with `e` and your name is kept from then on.

- Every terminal belongs to where it was opened: the SSH section, or one space. The tab bar only shows the tabs of what you are looking at, so what runs in an agent's project (an AI agent, say) never mixes with your SSH tabs.
- **Agents.** Run `claude`, `opencode`, `codex`, `gemini`, `aider`... in any terminal and it shows up under **running** at the bottom of the sidebar, with the project it runs in. `●` amber is working, `○` is idle, a green `●` means it finished and is waiting for you. Click one (or press `Alt+n`) to jump to its terminal wherever it lives.
- **Sound.** When an agent finishes a stretch of work, ship plays a sound (the system's player, or the terminal bell). Set `{"sound": false}` in `settings.json` (next to `servers.json`) to silence it.
- Detection looks at the foreground process of the terminal and at its output: an agent that keeps drawing is working; when it goes quiet after a while, it is done. Linux and macOS only.

The sidebar is kept clean: the views on top, then the list, then what is running, and one small line of buttons at the bottom; everything also has a key. In a window tall enough, the agents and what is running are bigger blocks (a space shows its branch or directory under its name); in a short window they shrink back to one line. `a` new space (opens a terminal) · `e` rename · `d` delete (closes its terminals, never touches the directory) · `Alt+↑↓` reorder · `Enter` or `t` open a terminal in it · `c` close the current tab.

## Sessions survive closing ship (Linux and macOS)

Terminals live in a small background server (`ship daemon`) that starts by itself the first time. Closing ship, or its window, only disconnects: open it again and every terminal is back, in the same agent, with its screen and its process still running, including agents. If an agent finishes while ship is closed, the server plays the sound and the tab is marked when you return.

- `q` leaves the sessions running (it says how many). `ship kill-server` stops the server and every session in it. The server exits by itself when it holds nothing.
- Closing a tab (`Alt+W`, `✕`, `c`) does end that session.
- After updating ship, run `ship kill-server` once so the new version replaces the old server (it ends the sessions).
- The server listens on a private Unix socket (mode `0600` inside a `0700` directory) under `$XDG_RUNTIME_DIR`. Saved passwords are sent to it once, in memory, when you connect; they are not written anywhere.
- Limits: only the last few MiB of each session's output are kept as history; sessions use the environment of the ship that opened them (an `ssh-agent` socket that later changes is not picked up). Set `SHIP_NO_DAEMON=1` to keep sessions inside the window instead.
- Windows has no background server yet: sessions end with the window.

## Selecting, copying and scrolling in a terminal

- **Select and copy with the mouse.** Drag over the terminal area to select; the text is copied as soon as you let go (the selection stays highlighted until your next click or key). Double-click selects the word under the pointer (a path or a URL counts as one word). Drag past the top or bottom edge to keep going through the history.
- The copy goes to the system clipboard through the terminal (OSC 52, which also works over SSH) and through `wl-copy`, `xclip`, `xsel`, `pbcopy` or `clip`, whichever is there. Paste with your terminal's paste shortcut (`Ctrl+Shift+V` in most), which ship forwards as a paste.
- **History.** The mouse wheel scrolls it; `Shift+PgUp` / `Shift+PgDn` go by half a page, `Shift+Home` to the oldest line, `Shift+End` (or just typing) back to live. An amber bar shows how far up you are. With the background server the history survives closing ship (up to the last few MiB of output of each session).

## Two views of the same servers

The sidebar has three views, left to right: **Agents**, **SSH** and **Bastions**. Press `v` (or click the header) to switch; or press `↑` on the first row and use `←` `→`: the view changes as you move, no `Enter` needed. The last two show the same servers in two ways:

- **SSH** — your servers, organized in folders.
- **Bastions** — how you reach them. Only bastions are roots here (servers that others are reached through), and what sits behind each one is nested under it, so chains like `bastion → web → db` become a tree. Servers that are not part of any jump chain are hidden in this view. To start one, edit a server and set “Jump via”. Opening it runs `ssh` through every hop.

## Keyboard (sidebar)

Navigate like a menu: `↓`/`↑` move, `→` goes into a folder or jump host, `←` folds it or goes back up, `Enter` opens a session.

| Key | Action |
|---|---|
| `↑` `↓` / `j` `k` | move selection |
| `→` / `l` | unfold, or step into the first child |
| `←` / `h` | fold, or go back to the parent |
| `Enter` | open server / toggle folder |
| `↑` on the first row | focus the views at the top (`Agents`, `SSH`, `Bastions`; `←` `→` switch view at once); `↓` from the last row focuses the buttons at the bottom (`+ Server`, `+ Folder`, `Edit`). `←` `→` choose, `Enter` activate, `Esc` (or the arrow back towards the list) leaves |
| `v` / `Tab` | switch view |
| `a` / `f` | new server / new folder (in the jump view, `a` on a server creates a host behind it) |
| `e` / `d` | edit / delete |
| `c` | close the open session of the selected server (in the Agents view: the current terminal) |
| `Alt+↑` `Alt+↓` | reorder among siblings |
| `t` | open a terminal of this computer in a new tab (also the `+` in the tab bar) |
| `p` | open the password vault |
| `q` | quit |

**Global:** `Alt+n` jump to the agent that wants attention (or the next one) · `F6` panel ⇄ terminal · `Alt+←/→` switch tab · `Alt+Shift+←/→` move tab · `Alt+1..9` jump to tab (from the sidebar, plain `1`..`9` also work; some terminals, e.g. Ptyxis, keep `Alt+N` for their own tabs) · `Alt+W` close · `F2` rename · `Shift+PgUp/PgDn` scrollback.

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
- Saved secrets are sent once, and only to the prompt that belongs to them: a jump host's password or key passphrase answers that host's prompt, and the destination's answers its own, so you are only asked for what is not saved. A secret is never retried.

## License

MIT, see [LICENSE](LICENSE).

## Roadmap

1. **Done** — tree, forms, tabs, resize, errors, drag & drop, jump hosts, password vault (with sudo fill), agents (project workspaces) with git branch, a running panel with sound, background server for persistent sessions. ✔
2. **Next:** `ship <alias>` / `list` / `add`, `~/.ssh/config` import (mapping `ProxyJump` to jump hosts), Ctrl+K quick search, auto-reconnect.
3. **Later:** SFTP, saved tunnels, snippets, themes (including per-server colors), remembering the chosen view.
