mod agent;
mod app;
mod clipboard;
#[cfg(unix)]
mod daemon;
#[cfg(not(unix))]
#[path = "daemon_stub.rs"]
mod daemon;
mod keys;
mod notify;
mod session;
mod settings;
mod spaces;
mod store;
mod tabby;
mod uistate;
mod ui;
mod vault;

use anyhow::Result;
use crossterm::{
    event::{
        self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture, Event, KeyEventKind,
    },
    execute,
};
use std::time::Duration;

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("import-tabby") => {
            let path = args.next().map(Into::into).unwrap_or_else(tabby::default_path);
            let mut store = store::Store::load()?;
            let (folders, servers, skipped) = tabby::import(&mut store, &path)?;
            println!("Imported {servers} servers and {folders} folders ({skipped} already present).");
            println!("Passwords are not imported (Tabby keeps them in its own vault): set them in ship's vault.");
            return Ok(());
        }
        Some("daemon") => return daemon::run_server(),
        Some("kill-server") => {
            println!("{}", daemon::kill_server()?);
            return Ok(());
        }
        _ => {}
    }
    let store = store::Store::load()?;
    let vault = vault::Vault::new(store::Store::config_dir().join("vault.json"));
    let mut app = app::App::new(store, vault);
    app.spaces = spaces::Spaces::load()?;
    app.sound = settings::Settings::load().sound;
    let ui = uistate::UiState::load();
    app.bastions_open = ui.bastions_open;
    app.jump_open = ui.open_nodes.into_iter().collect();
    app.ui_path = uistate::UiState::path();
    app.rebuild();

    let mut terminal = ratatui::init();
    execute!(std::io::stdout(), EnableMouseCapture, EnableBracketedPaste)?;
    // On panic, leave the terminal usable before printing the error.
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = execute!(std::io::stdout(), DisableBracketedPaste, DisableMouseCapture);
        hook(info);
    }));

    connect_background_server(&mut app);
    let result = run(&mut terminal, &mut app);

    let _ = execute!(std::io::stdout(), DisableBracketedPaste, DisableMouseCapture);
    ratatui::restore();
    if app.left_running > 0 {
        println!(
            "ship: {} session(s) keep running in the background. Run `ship` to come back, `ship kill-server` to stop them.",
            app.left_running
        );
    }
    result
}

/// Sessions live in a background server so they survive closing this window; reconnect to the sessions it holds.
fn connect_background_server(app: &mut app::App) {
    if std::env::var_os("SHIP_NO_DAEMON").is_some() {
        return;
    }
    match daemon::connect_or_start() {
        Ok(d) => {
            let (cols, rows) = crossterm::terminal::size().unwrap_or((120, 32));
            let side = 32.min(cols / 2);
            let server_version = d.server_version.clone();
            app.daemon = Some(std::sync::Arc::new(d));
            app.restore_sessions(rows.saturating_sub(2).max(1), cols.saturating_sub(side).max(1));
            if server_version != env!("CARGO_PKG_VERSION") {
                app.set_flash(format!(
                    "The background server is v{server_version} (this is v{}): `ship kill-server` restarts it on the new version, ending its sessions",
                    env!("CARGO_PKG_VERSION")
                ));
            }
        }
        Err(daemon::ConnectError::None) => {}
        Err(daemon::ConnectError::Incompatible(v)) => app.set_flash(format!(
            "A background ship v{v} is running and cannot be used: run `ship kill-server` (ends its sessions). Sessions will not persist."
        )),
        Err(daemon::ConnectError::Other(e)) => {
            app.set_flash(format!("No background server ({e:#}): sessions will end with this window"))
        }
    }
}

fn run(terminal: &mut ratatui::DefaultTerminal, app: &mut app::App) -> Result<()> {
    while !app.quit {
        terminal.draw(|f| ui::draw(f, app))?;
        if event::poll(Duration::from_millis(30))? {
            loop {
                match event::read()? {
                    Event::Key(k) if k.kind != KeyEventKind::Release => app.on_key(k),
                    Event::Mouse(m) => app.on_mouse(m),
                    Event::Paste(s) => app.on_paste(&s),
                    _ => {}
                }
                if !event::poll(Duration::ZERO)? {
                    break;
                }
            }
        }
        app.tick();
    }
    Ok(())
}
