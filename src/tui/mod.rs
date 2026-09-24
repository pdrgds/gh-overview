pub mod app;
pub mod status;
pub mod view;

use std::time::{Duration, Instant};

use anyhow::Result;
use chrono::Utc;
use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{self, Event, KeyEventKind};

use crate::browser;
use crate::config::Config;
use crate::paths::Paths;
use crate::store::{Command, Store};
use app::{Action, App};

const RELOAD: Duration = Duration::from_secs(2);

pub fn run(paths: &Paths, config: &Config) -> Result<()> {
    let store = Store::open(&paths.db_file)?;
    let mut app = App::new(config);
    let mut terminal = ratatui::init();
    let result = event_loop(&mut terminal, &store, &mut app, config);
    ratatui::restore();
    result
}

fn event_loop(terminal: &mut DefaultTerminal, store: &Store, app: &mut App, config: &Config) -> Result<()> {
    let mut last_load: Option<Instant> = None;
    loop {
        if last_load.is_none_or(|t| t.elapsed() >= RELOAD) {
            app.load(&store.db(), config, Utc::now())?;
            last_load = Some(Instant::now());
        }
        terminal.draw(|frame| view::render(frame, app))?;
        if event::poll(Duration::from_millis(250))?
            && let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
        {
            dispatch(
                app.on_key(key, Utc::now()),
                |login, url| browser::open_url(config.browser_for(login), url).is_ok(),
                |command| store.db().enqueue(command, Utc::now()),
            )?;
            last_load = None;
        }
        if app.quit {
            return Ok(());
        }
    }
}

fn dispatch(
    actions: Vec<Action>,
    mut open: impl FnMut(&str, &str) -> bool,
    mut enqueue: impl FnMut(&Command) -> Result<()>,
) -> Result<()> {
    let mut opened = true;
    for action in actions {
        match action {
            Action::Open { login, url } => opened = open(&login, &url),
            Action::Enqueue(Command::Ack { .. }) if !opened => {}
            Action::Enqueue(command) => enqueue(&command)?,
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failed_open_does_not_acknowledge() {
        let actions = || {
            vec![
                Action::Open {
                    login: "me-work".into(),
                    url: "https://github.com/acme/api/pull/1".into(),
                },
                Action::Enqueue(Command::Ack {
                    pr_key: "acme/api#1".into(),
                }),
            ]
        };
        let mut queued = Vec::new();
        dispatch(
            actions(),
            |_, _| false,
            |c| {
                queued.push(c.clone());
                Ok(())
            },
        )
        .unwrap();
        assert!(queued.is_empty());
        dispatch(
            actions(),
            |_, _| true,
            |c| {
                queued.push(c.clone());
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(
            queued,
            vec![Command::Ack {
                pr_key: "acme/api#1".into()
            }]
        );
    }

    #[test]
    fn other_commands_are_enqueued_regardless() {
        let mut queued = Vec::new();
        dispatch(
            vec![Action::Enqueue(Command::Refresh)],
            |_, _| false,
            |c| {
                queued.push(c.clone());
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(queued, vec![Command::Refresh]);
    }
}
