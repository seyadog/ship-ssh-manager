mod agent;
mod app;
mod keys;
mod notify;
mod session;
mod settings;
mod spaces;
mod store;
mod tabby;
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
    if args.next().as_deref() == Some("import-tabby") {
        let path = args.next().map(Into::into).unwrap_or_else(tabby::default_path);
        let mut store = store::Store::load()?;
        let (folders, servers, skipped) = tabby::import(&mut store, &path)?;
        println!("Imported {servers} servers and {folders} folders ({skipped} already present).");
        println!("Passwords are not imported (Tabby keeps them in its own vault): set them in ship's vault.");
        return Ok(());
    }
    let store = store::Store::load()?;
    let vault = vault::Vault::new(store::Store::config_dir().join("vault.json"));
    let mut app = app::App::new(store, vault);
    app.spaces = spaces::Spaces::load()?;
    app.sound = settings::Settings::load().sound;
    app.rebuild();

    let mut terminal = ratatui::init();
    execute!(std::io::stdout(), EnableMouseCapture, EnableBracketedPaste)?;
    // On panic, leave the terminal usable before printing the error.
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = execute!(std::io::stdout(), DisableBracketedPaste, DisableMouseCapture);
        hook(info);
    }));

    let result = run(&mut terminal, &mut app);

    let _ = execute!(std::io::stdout(), DisableBracketedPaste, DisableMouseCapture);
    ratatui::restore();
    result
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
