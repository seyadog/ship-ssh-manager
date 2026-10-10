# oso

> oso used to be called ship. Updating moves your data folder to the new name by itself, and the installer leaves a `ship` command that points to `oso`.

An SSH connection manager for the terminal: a modern TUI with mouse support. Keep your servers in folders, see them by jump host, and open every session in a tab. Sessions keep running when you close it. A lightweight, keyboard- and mouse-friendly alternative to GUI SSH clients.

> Status: servers, folders, jump hosts, the encrypted password vault, and sessions that survive closing the window (Linux and macOS). oso is only for SSH (and a local terminal): earlier versions also had AI-agent projects, which were removed in 0.11. Architecture: [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md).

## Install

Needs an `ssh` client in the PATH (preinstalled on macOS and most Linux distributions).

**Prebuilt binary** (Linux x86_64 / arm64, macOS Apple Silicon and Intel, Windows 11 x86_64 — experimental). Download the `.tar.gz` for your platform from the
[latest release](https://github.com/seyadog/oso/releases/latest), or:

```sh
curl -fsSL https://raw.githubusercontent.com/seyadog/oso/main/install.sh | sh
```

It installs `oso` into `~/.local/bin` (set `INSTALL_DIR` to change it). On macOS, if Gatekeeper blocks the binary:
`xattr -d com.apple.quarantine ~/.local/bin/oso`.

**Windows 11** (experimental, PowerShell; use Windows Terminal):

```powershell
irm https://raw.githubusercontent.com/seyadog/oso/main/install.ps1 | iex
```

It installs `oso.exe` into `%LOCALAPPDATA%\oso\bin`. Needs the OpenSSH Client feature (on by default in Windows 11).
See [Windows notes](#windows-notes) for the limitations.

**From source** (stable [Rust](https://rustup.rs)):

```sh
cargo install --git https://github.com/seyadog/oso --locked
```

Data lives in `~/.config/oso/servers.json` (`~/Library/Application Support/oso` on macOS, or `$OSO_CONFIG_DIR`).
Passwords never go in that file: they live in the encrypted vault (`vault.json`, next to it).

## Launcher on Linux

`install.sh` also adds an app entry (and the bear icon) so oso opens from the desktop launcher or the dock like any other app. It is still a terminal window with oso inside: `oso-launch` opens it in `kitty`, `foot`, `alacritty`, Ptyxis or GNOME Terminal (the first one found), with its own window class where the terminal allows it, so the desktop groups it as its own app. From the source tree, copy `assets/oso.svg` to `~/.local/share/icons/hicolor/scalable/apps/`, `assets/oso-launch` somewhere in your `PATH`, and `assets/oso.desktop` to `~/.local/share/applications/` after replacing `@LAUNCH@` and `@OSO@` with the full paths.

## Windows notes

Windows support is **experimental**: it builds and passes the unit tests in CI, but has had little real-world testing.

- Use Windows Terminal. It uses the system `ssh.exe` (Settings → System → Optional features → OpenSSH Client).
- Jump hosts through the agent or default keys (`-J`) should work. Chains where a hop has its own key use a nested `ProxyCommand` run by `cmd.exe`, which is untested; deep chains may fail.
- Some `Alt` key combinations may be taken by Windows or the terminal; the mouse and arrow keys always work.
- The vault has no extra file permissions on Windows (no `0600`); it is still encrypted with your master password.
- The binary is not code-signed, so SmartScreen may warn on first run. Windows on ARM uses the x86_64 build via emulation.
- Data lives in `%APPDATA%\oso\config` (or `$env:OSO_CONFIG_DIR`).

## Develop

```sh
cargo run
cargo test
```

## Sessions survive closing oso (Linux and macOS)

Terminals live in a small background server (`oso daemon`) that starts by itself the first time. Closing oso, or its window, only disconnects: open it again and every terminal is back, with its screen and its process still running.

- `q` leaves the sessions running (it says how many). `oso kill-server` stops the server and every session in it. The server exits by itself when it holds nothing.
- Closing a tab (`Alt+W`, `✕`, `c`) does end that session.
- After updating oso, run `oso kill-server` once so the new version replaces the old server (it ends the sessions).
- The server listens on a private Unix socket (mode `0600` inside a `0700` directory) under `$XDG_RUNTIME_DIR`. Saved passwords are sent to it once, in memory, when you connect; they are not written anywhere.
- Limits: only the last few MiB of each session's output are kept as history; sessions use the environment of the oso that opened them (an `ssh-agent` socket that later changes is not picked up). Set `OSO_NO_DAEMON=1` to keep sessions inside the window instead.
- Windows has no background server yet: sessions end with the window.

## Selecting, copying and scrolling in a terminal

- **Select and copy with the mouse.** Drag over the terminal area to select; the text is copied as soon as you let go (the selection stays highlighted until your next click or key). Double-click selects the word under the pointer (a path or a URL counts as one word). Drag past the top or bottom edge to keep going through the history.
- The copy goes to the system clipboard through the terminal (OSC 52, which also works over SSH) and through `wl-copy`, `xclip`, `xsel`, `pbcopy` or `clip`, whichever is there. Paste with your terminal's paste shortcut (`Ctrl+Shift+V` in most), which oso forwards as a paste.
- **History.** The mouse wheel scrolls it; `Shift+PgUp` / `Shift+PgDn` go by half a page, `Shift+Home` to the oldest line, `Shift+End` (or just typing) back to live. An amber bar shows how far up you are. With the background server the history survives closing oso (up to the last few MiB of output of each session).

## The sidebar

One column, always open: your servers, organized in folders, in the upper half. The lower half, always starting at the middle of the sidebar, is the **bastions** section (it shows up once a server is reached through another; each half scrolls on its own): the bastions are its roots, and what sits behind each one is nested under it, so chains like `bastion → web → db` become a tree. The title is just a label, always there; what folds are the bastions in it (`→` / `←`). To start one, edit a server and set “Jump via”, or drag a server onto a bastion. Opening a server runs `ssh` through every hop.

Next to the title there is a coloured dot for every server with an open session. Folders are light green. At the bottom: `+ new`, `edit`, `term` (a terminal of this computer) and `vault`.

Everything starts **closed**: folders and each bastion. What you open stays open, also after you close oso (folders are remembered in `servers.json`, the bastions in `state.json`).

The **tab bar** at the top is like a browser's: it shows every tab. Pick one with the click or `Alt+←/→` (the bar shows the tabs of the screen you are on).

**Two groups side by side:** `Alt+Shift+→` sends the active tab to a right group (the screen splits in half, each group with its own bar), `Alt+Shift+←` sends it back. Drag the line between them to resize; drag a tab onto the other group to move it. A group that loses its last tab disappears. The numbers run through the left group and on into the right one.

## Mosaic and broadcast

Like a tiling terminal. **Screens, like Hyprland's workspaces:** `Alt+1` … `Alt+9` go to screen 1 … 9 (the numbers under the sidebar title show the ones in use; click one too). Each screen has its own terminals and its own mode, so screen 1 can be a mosaic and screen 2 a set of tabs. `Alt+S` then `1..9` (or `Alt+Shift+1..9`, on terminals that deliver it) sends the active terminal to another screen, or drag a tab onto a screen number. `Alt+,` and `Alt+.` go to the previous and next screen when your terminal keeps `Alt+number` for itself. At the bottom of the sidebar, two buttons choose the mode of the screen you are on (the lit one is in use; `Alt+M` switches): `▤ tabs` (each new terminal is its own tab) or `▦ mosaic` (every new terminal tiles into the mosaic by itself, when you open it with a click or the keyboard). The modes are remembered between runs. The mosaic shows your open sessions at once, each in its own bordered box with its title (the active one has a thick, coloured border). The screen is a tree of divisions: every division is side by side or stacked and can be split again, so **any arrangement** works, with as many terminals as fit. Build it your way:

- `Alt+V` opens a new terminal to the **right** of the active one, `Alt+H` **below** it (from a single terminal too: that starts the mosaic). Do it again on any box to keep splitting.
- **Drag a tab** from the bar (or a box by its title) onto a terminal: near its left, right, top or bottom edge it docks there and takes half of it (the zone is outlined while you drag); in the middle it trades places with that box. Dragging a tab onto the screen when there is no mosaic yet starts one with the tab you were on.
- **Drag the line** between two terminals to resize them.
- `Alt+arrows` move the keyboard to the terminal on that side; `Alt+Shift+arrows` swap the active terminal with the one there.
- A new tab joins the mosaic by splitting the terminal you were on; a tab picked from the bar takes its place; closing a terminal gives its room to its neighbour. `Alt+M` again goes back to one terminal.

It arranges itself like Hyprland's default: each new terminal splits the one you were on, beside it when that box is wide and below it when it is tall (never leaving a box too small to use); `Alt+M` starts with up to six terminals that way. Then drag them wherever you want. The **×** next to a terminal's name closes it.

`⇉ broadcast` (or `Alt+B`) sends **everything you type or paste to all the terminals of the mosaic** at once, so you can run the same command on several machines. While it is on, every title says `BROADCAST` in red and so does the status bar. It stops by itself when you leave the mosaic or fewer than two sessions are left. Only the terminals you can see receive it; sessions that ended are skipped.

## Keyboard (sidebar)

Navigate like a menu: `↓`/`↑` move, `→` goes into a folder or a bastion, `←` folds it or goes back up, `Enter` opens a session.

| Key | Action |
|---|---|
| `↑` `↓` / `j` `k` | move selection |
| `→` / `l` | unfold, or step into the first child |
| `←` / `h` | fold, or go back to the parent |
| `Enter` | open server / toggle folder |
| `a` / `f` | new server / new folder (on a server of the bastions section, `a` creates a host behind it) |
| `e` / `d` | edit / delete |
| `c` | close the open session of the selected server |
| `Alt+↑` `Alt+↓` | reorder among siblings |
| `t` | open a terminal of this computer in a new tab (also the `+` in the tab bar) |
| `p` | open the password vault |
| `q` | quit |

**Global:** `F6` / `Shift+F6` next / previous area (servers, terminal; both terminals when split) · `Alt+M` mosaic · `Alt+V` / `Alt+H` split right / below · `Alt+B` broadcast · `Alt+Q` panel ⇄ terminal · `Alt+K` (or `/` in the panel) quick open: type a few letters of a server · `F8` switch the colours between the default pastel theme (it keeps your terminal's background and text colour) and the classic fixed colours · `Alt+←/→` switch tab · `Alt+Shift+←/→` send tab to the left/right group (in the mosaic, `Alt+Shift+←/→/↑/↓` moves the terminal around the grid) · `Alt+1..9` go to that screen · `Alt+S` then `1..9` send the tab to that screen · `Alt+,` / `Alt+.` previous / next screen (in the mosaic, `Alt+arrows` move the keyboard between the terminals; from the sidebar, plain `1`..`9` also work; some terminals, e.g. Ptyxis, keep `Alt+N` for their own tabs) · `Alt+W` close · `F2` rename · `Shift+PgUp/PgDn` scrollback.

**Mouse:** double-click opens a server or toggles a folder · drag to move (onto a folder: inside it; onto a server: right before it; onto a bastion: it is reached through it from then on; from the bastions section onto its title or empty space: directly again) · drag tabs to reorder · ✕ closes a tab · double-click a tab renames it · wheel scrolls the history.

In the server form, `Ctrl+O` (or “Browse…”) opens a file browser that starts in `~/.ssh` and highlights private keys.

## Import from Tabby

`oso import-tabby [path/to/config.yaml]` (default `~/.config/tabby/config.yaml`) copies SSH profiles and groups into oso:
host, port, user, private key path, folders and jump hosts. Re-running it skips servers already present. Passwords are
not imported (Tabby keeps them in its own vault); save them in oso's vault afterwards.

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

1. **Done** — tree, forms, tabs, resize, errors, drag & drop, jump hosts, password vault (with sudo fill), background server for persistent sessions, one-column sidebar. ✔
2. **Next:** folders that close by themselves when the list runs out of room, `oso <alias>` / `list` / `add`, `~/.ssh/config` import (mapping `ProxyJump` to jump hosts), Ctrl+K quick search, auto-reconnect.
3. **Later:** SFTP, saved tunnels, snippets, themes (including per-server colors), remembering the chosen view.
