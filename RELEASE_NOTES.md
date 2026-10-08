**Install:** see the [README](https://github.com/seyadog/ship-ssh-manager#install). Linux and macOS: `install.sh`. Windows: `install.ps1`, or unzip `ship-*-windows-x86_64.zip` and run `ship.exe` from Windows Terminal.

> **Windows 11 support is experimental.** It builds and passes the unit tests in CI, but it has had far less real-world testing than Linux and macOS. Known limitations:
>
> - Use **Windows Terminal** (the default on Windows 11). The old console host and some third-party terminals may misrender or mishandle keys.
> - It uses the OpenSSH client that ships with Windows 11 (`ssh.exe`). If `ssh` is not found, enable *Settings → System → Optional features → OpenSSH Client*.
> - **Jump hosts** through plain agent/default keys work (`-J`). Chains where a hop has its own key use a nested `ProxyCommand` through `cmd.exe`, which is untested on Windows; deep chains may fail.
> - Some `Alt` key combinations may be intercepted by Windows or the terminal. Use the mouse or the arrow keys if one does not work.
> - The vault file gets no extra permission restrictions (Windows has no `0600`); it relies on your user profile folder being private. It stays encrypted with your master password.
> - The binary is **not code-signed**: Windows SmartScreen may warn on first run ("More info" → "Run anyway").
> - Windows on ARM runs the x86_64 build through emulation; there is no native ARM build yet.
>
> Please report problems at https://github.com/seyadog/ship-ssh-manager/issues.

macOS binaries are not notarized (see the README for how to open them).
