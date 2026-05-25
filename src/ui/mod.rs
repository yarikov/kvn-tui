pub mod layout;
pub mod nav;
pub mod styles;
pub mod widgets;

use std::io;

use crossterm::event::{Event, EventStream};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::ExecutableCommand;
use futures_util::{FutureExt, StreamExt};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use tokio::time::{interval, Duration};

use crate::app::{App, AppMode};
use crate::input::handle_event;
use crate::singbox;

/// Run the TUI main loop until the user requests quit.
pub async fn run(mut app: App) -> io::Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    stdout.execute(EnterAlternateScreen)?;

    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = run_loop(&mut terminal, &mut app).await;

    // Ensure sing-box is stopped before exiting.
    singbox::disconnect(&mut app);

    disable_raw_mode()?;
    terminal.backend_mut().execute(LeaveAlternateScreen)?;

    result
}

/// Inner async event loop that draws and processes input.
async fn run_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut App,
) -> io::Result<()> {
    let mut reader = EventStream::new();
    const TICK_RATE_MS: u64 = 250;
    let mut tick = interval(Duration::from_millis(TICK_RATE_MS));

    loop {
        tokio::select! {
            biased;
            Some(Ok(event)) = reader.next().fuse() => {
                if let Event::Key(key) = event {
                    if handle_event(app, Event::Key(key)) {
                        return Ok(());
                    }
                }
            }
            _ = tick.tick() => {
                app.on_tick();
            }
        }

        if app.needs_redraw {
            terminal.clear()?;
            app.needs_redraw = false;
        }
        terminal.draw(|f| layout::draw(f, app))?;

        // Handle pending connection requests.
        if app.mode == AppMode::Connecting {
            if let Some(profile) = app.selected_profile().cloned() {
                app.connecting = true;
                if let Err(e) = singbox::connect(app, &profile) {
                    app.set_error(format!("Connection failed: {}", e));
                    app.connecting = false;
                }
            } else {
                app.mode = AppMode::Normal;
            }
        }
    }
}
