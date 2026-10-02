pub mod app;
pub mod status;
pub mod view;

use std::sync::mpsc;
use std::thread;
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
    let (opened_tx, opened_rx) = mpsc::channel::<Vec<Command>>();
    let mut last_load: Option<Instant> = None;
    loop {
        for then in opened_rx.try_iter() {
            enqueue_all(store, &then)?;
            last_load = None;
        }
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
                |login, url, then| {
                    let browser = config.browser_for(login).cloned();
                    let url = url.to_owned();
                    let opened = opened_tx.clone();
                    thread::spawn(move || {
                        if browser::open_url(browser.as_ref(), &url).is_ok() {
                            let _ = opened.send(then);
                        }
                    });
                },
                |command| store.db().enqueue(command, Utc::now()),
            )?;
            last_load = None;
        }
        if app.quit {
            drop(opened_tx);
            for then in opened_rx {
                enqueue_all(store, &then)?;
            }
            return Ok(());
        }
    }
}

fn enqueue_all(store: &Store, commands: &[Command]) -> Result<()> {
    commands
        .iter()
        .try_for_each(|command| store.db().enqueue(command, Utc::now()))
}

fn dispatch(
    actions: Vec<Action>,
    mut open: impl FnMut(&str, &str, Vec<Command>),
    mut enqueue: impl FnMut(&Command) -> Result<()>,
) -> Result<()> {
    let mut opening = None;
    let mut then = Vec::new();
    for action in actions {
        match action {
            Action::Open { login, url } => opening = Some((login, url)),
            Action::Enqueue(command @ Command::Ack { .. }) if opening.is_some() => then.push(command),
            Action::Enqueue(command) => enqueue(&command)?,
        }
    }
    if let Some((login, url)) = opening {
        open(&login, &url, then);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_open_defers_its_acknowledgement_until_the_browser_opened() {
        let ack = Command::Ack {
            pr_key: "acme/api#1".into(),
        };
        let mut opened = Vec::new();
        let mut queued = Vec::new();
        dispatch(
            vec![
                Action::Open {
                    login: "me-work".into(),
                    url: "https://github.com/acme/api/pull/1".into(),
                },
                Action::Enqueue(ack.clone()),
            ],
            |login, url, then| opened.push((login.to_owned(), url.to_owned(), then)),
            |c| {
                queued.push(c.clone());
                Ok(())
            },
        )
        .unwrap();
        assert!(queued.is_empty());
        assert_eq!(
            opened,
            vec![(
                "me-work".to_owned(),
                "https://github.com/acme/api/pull/1".to_owned(),
                vec![ack]
            )]
        );
    }

    #[test]
    fn other_commands_are_enqueued_regardless() {
        let mut queued = Vec::new();
        dispatch(
            vec![Action::Enqueue(Command::Refresh)],
            |_, _, _| {},
            |c| {
                queued.push(c.clone());
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(queued, vec![Command::Refresh]);
    }
}
