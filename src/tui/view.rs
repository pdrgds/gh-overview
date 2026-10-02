use chrono::Local;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Clear, Paragraph, Row, Table, TableState};

use super::app::{App, Badge, ViewRow};
use super::status::Health;
use crate::store::Tab;

const HIGHLIGHT: &str = "▶ ";
const SPACING: u16 = 1;

pub fn render(frame: &mut Frame, app: &App) {
    let warnings: Vec<&String> = app.status.errors.iter().chain(app.status.degraded.iter()).collect();
    let [header, warning, body, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(u16::from(!warnings.is_empty())),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .areas(frame.area());
    render_header(frame, header, app);
    if !warnings.is_empty() {
        let text = warnings
            .iter()
            .map(|w| format!("▲ {w}"))
            .collect::<Vec<_>>()
            .join("   ");
        frame.render_widget(
            Paragraph::new(format!(
                " {}",
                truncate(&text, warning.width.saturating_sub(1) as usize)
            ))
            .style(Style::new().fg(Color::Yellow)),
            warning,
        );
    }
    render_table(frame, body, app);
    render_footer(frame, footer, app);
    if app.snoozing {
        render_snooze(frame, body, app);
    }
}

fn render_footer(frame: &mut Frame, area: Rect, app: &App) {
    let footer = match &app.notice {
        Some(notice) => Paragraph::new(format!(" {notice}")).style(Style::new().fg(Color::Yellow)),
        None => {
            let snooze = if app.can_snooze() { " s snooze ·" } else { "" };
            Paragraph::new(format!(
                " enter open ·{snooze} d done · a reviewed · m mute today · r refresh · q quit"
            ))
            .style(Style::new().fg(Color::DarkGray))
        }
    };
    frame.render_widget(footer, area);
}

fn review_label(app: &App) -> String {
    let needs = app.review.iter().filter(|r| !r.reviewed).count();
    if app.reviewed_count == 0 {
        format!("[1] To review ({needs})")
    } else {
        format!("[1] To review ({needs} · {} reviewed)", app.reviewed_count)
    }
}

fn render_header(frame: &mut Frame, area: Rect, app: &App) {
    let tab = |label: String, active: bool| {
        let style = if active {
            Style::new().add_modifier(Modifier::BOLD | Modifier::REVERSED)
        } else {
            Style::new()
        };
        Span::styled(label, style)
    };
    let left = Line::from(vec![
        Span::styled(" gh-overview   ", Style::new().add_modifier(Modifier::BOLD)),
        tab(review_label(app), app.tab == Tab::Review),
        Span::raw("   "),
        tab(format!("[2] My PRs ({})", app.mine.len()), app.tab == Tab::Mine),
    ]);
    let mut right = Line::default();
    if let Some(until) = app.status.muted_until {
        right.push_span(Span::styled(
            format!("🔕 muted until {}  ", until.with_timezone(&Local).format("%H:%M")),
            Style::new().fg(Color::Yellow),
        ));
    }
    right.push_span(match &app.status.health {
        Health::Ok { polled_ago: Some(ago) } => {
            Span::styled(format!("● polled {ago} ago "), Style::new().fg(Color::Green))
        }
        Health::Ok { polled_ago: None } => Span::styled("● waiting for first poll ", Style::new().fg(Color::Green)),
        Health::DaemonDown => Span::styled("● daemon not running — ghov install ", Style::new().fg(Color::Red)),
    });
    let right_width = (right.width() as u16).min(area.width);
    let [l, r] = Layout::horizontal([Constraint::Min(0), Constraint::Length(right_width)]).areas(area);
    frame.render_widget(Paragraph::new(left), l);
    frame.render_widget(Paragraph::new(right).right_aligned(), r);
}

struct Column {
    title: &'static str,
    width: u16,
    cell: fn(&ViewRow) -> String,
}

fn badge_text(badge: &Badge) -> String {
    match badge {
        Badge::None => String::new(),
        Badge::Pinging => "🔔 pinging".into(),
        Badge::Snoozed(until) => format!("💤 {}", until.with_timezone(&Local).format("%H:%M")),
        Badge::Seen => "✓ seen".into(),
    }
}

fn columns(tab: Tab, width: u16) -> Vec<Column> {
    let repo = |width| Column {
        title: "REPO",
        width,
        cell: |r| r.repo.clone(),
    };
    let number = Column {
        title: "#",
        width: 6,
        cell: |r| r.number.to_string(),
    };
    let account = Column {
        title: "ACCT",
        width: 10,
        cell: |r| r.account.clone(),
    };
    let why = |width| Column {
        title: "WHY",
        width,
        cell: |r| r.why.clone(),
    };
    let alert = Column {
        title: "ALERT",
        width: 10,
        cell: |r| badge_text(&r.badge),
    };
    match tab {
        Tab::Review => {
            let mut cols = vec![repo(if width >= 100 { 28 } else { 20 }), number];
            if width >= 100 {
                cols.push(Column {
                    title: "AUTHOR",
                    width: 14,
                    cell: |r| r.author.clone(),
                });
            }
            if width >= 85 {
                cols.push(Column {
                    title: "AGE",
                    width: 4,
                    cell: |r| r.age.clone(),
                });
            }
            cols.push(alert);
            cols.push(account);
            cols.push(Column {
                title: "VIA",
                width: 14,
                cell: |r| r.team.as_ref().map(|t| format!("team:{t}")).unwrap_or_default(),
            });
            cols
        }
        Tab::Mine if width >= 100 => vec![repo(28), number, why(30), alert, account],
        Tab::Mine => vec![repo(20), number, why(22), alert],
    }
}

fn display_width(text: &str) -> usize {
    Span::raw(text).width()
}

fn truncate(text: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    if display_width(text) <= width {
        return text.to_string();
    }
    let mut kept = String::new();
    let mut used = 0;
    for ch in text.chars() {
        let w = display_width(ch.encode_utf8(&mut [0; 4]));
        if used + w + 1 > width {
            break;
        }
        kept.push(ch);
        used += w;
    }
    kept.push('…');
    kept
}

fn render_table(frame: &mut Frame, area: Rect, app: &App) {
    let cols = columns(app.tab, area.width);
    let fixed: u16 =
        cols.iter().map(|c| c.width).sum::<u16>() + SPACING * cols.len() as u16 + HIGHLIGHT.chars().count() as u16;
    let title_width = area.width.saturating_sub(fixed).max(10) as usize;
    let mut widths: Vec<Constraint> = cols.iter().map(|c| Constraint::Length(c.width)).collect();
    widths.insert(2, Constraint::Fill(1));
    let mut header: Vec<&str> = cols.iter().map(|c| c.title).collect();
    header.insert(2, "TITLE");
    let rows = app.rows().iter().map(|r| {
        let mut cells: Vec<Cell> = cols.iter().map(|c| Cell::from((c.cell)(r))).collect();
        let mut title = r.title.clone();
        if r.is_draft {
            title = format!("[draft] {title}");
        }
        if r.reviewed {
            title = format!("[{}] {title}", r.why);
        }
        cells.insert(2, Cell::from(truncate(&title, title_width)));
        let row = Row::new(cells);
        if r.reviewed {
            row.style(Style::new().fg(Color::DarkGray))
        } else {
            row
        }
    });
    let table = Table::new(rows, widths)
        .header(Row::new(header).style(Style::new().add_modifier(Modifier::BOLD)))
        .column_spacing(SPACING)
        .highlight_symbol(HIGHLIGHT)
        .row_highlight_style(Style::new().add_modifier(Modifier::REVERSED))
        .block(Block::new().borders(Borders::TOP | Borders::BOTTOM));
    let mut state = TableState::default().with_selected(app.selected_index());
    frame.render_stateful_widget(table, area, &mut state);
}

fn render_snooze(frame: &mut Frame, area: Rect, app: &App) {
    let mut lines: Vec<Line> = app
        .snooze_labels()
        .iter()
        .enumerate()
        .map(|(i, label)| Line::raw(format!(" {}  {label}", i + 1)))
        .collect();
    lines.push(Line::raw(" esc cancel"));
    let height = (lines.len() as u16 + 2).min(area.height);
    let [_, mid, _] =
        Layout::vertical([Constraint::Fill(1), Constraint::Length(height), Constraint::Fill(1)]).areas(area);
    let [_, popup, _] =
        Layout::horizontal([Constraint::Fill(1), Constraint::Length(24), Constraint::Fill(1)]).areas(mid);
    frame.render_widget(Clear, popup);
    frame.render_widget(Paragraph::new(lines).block(Block::bordered().title(" Snooze ")), popup);
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};

    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use super::*;
    use crate::config::Config;
    use crate::domain::alert::AlertState;
    use crate::domain::reasons::Reason;
    use crate::domain::testkit::*;
    use crate::store::PrRow;
    use crate::tui::app::{mine_rows, review_rows};
    use crate::tui::status::HeaderStatus;

    fn config() -> Config {
        let mut c = Config::with_accounts(vec!["me-work".into(), "me-home".into()]);
        c.accounts[0].label = "work".into();
        c.accounts[1].label = "personal".into();
        c.snooze_choices = vec!["15m".into(), "1h".into(), "tomorrow".into()];
        c
    }

    fn app() -> App {
        let config = config();
        let mut app = App::new(&config);
        let mut a = PrRow::review(
            "me-work",
            &crate::domain::model::ReviewRequest {
                base: base("acme/gateway", 412),
                direct: true,
                team: None,
                event: None,
                reviews: vec![],
            },
        );
        a.title = "[api] add rate limits to the public gateway endpoints".into();
        a.author = "alice".into();
        let mut b = PrRow::review(
            "me-work",
            &crate::domain::model::ReviewRequest {
                base: base("acme/infrastructure", 77),
                direct: false,
                team: Some("platform".into()),
                event: None,
                reviews: vec![],
            },
        );
        b.author = "bob".into();
        let mut c = PrRow::review(
            "me-work",
            &crate::domain::model::ReviewRequest {
                base: base("acme/web", 1719),
                direct: true,
                team: None,
                event: None,
                reviews: vec![],
            },
        );
        c.title = "content transfer between environments".into();
        c.author = "sam".into();
        c.reasons = vec![Reason::ChangesRequested {
            by: vec!["raad".into()],
        }];
        let review_alerts = HashMap::from([(
            "acme/gateway#412".to_string(),
            AlertState {
                cycle_started_at: Some(t(-5)),
                ..Default::default()
            },
        )]);
        app.review = review_rows(vec![a, b, c], &review_alerts, &HashSet::new(), &config, t(0));
        app.reviewed_count = 1;
        app.show_reviewed = true;
        let mut m = PrRow::mine(
            "me-home",
            &my_pr("octocat/dotfiles", 310),
            vec![
                Reason::Threads { count: 3 },
                Reason::ChangesRequested {
                    by: vec!["alice".into()],
                },
            ],
            Some(t(-5)),
        );
        m.is_draft = true;
        let alerts = HashMap::from([(
            "octocat/dotfiles#310".to_string(),
            AlertState {
                cycle_started_at: Some(t(-5)),
                ..Default::default()
            },
        )]);
        app.mine = mine_rows(vec![m], &alerts, &HashSet::new(), &config, t(0));
        app.status = HeaderStatus {
            health: Health::Ok {
                polled_ago: Some("12s".into()),
            },
            errors: vec![],
            degraded: None,
            muted_until: None,
        };
        app
    }

    fn draw(app: &App, width: u16, height: u16) -> Terminal<TestBackend> {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|f| render(f, app)).unwrap();
        terminal
    }

    fn needing_review_only(mut app: App) -> App {
        app.show_reviewed = false;
        app.review.retain(|r| !r.reviewed);
        app
    }

    #[test]
    fn review_tab() {
        insta::assert_snapshot!(draw(&needing_review_only(app()), 120, 7).backend());
    }

    #[test]
    fn review_tab_narrow() {
        insta::assert_snapshot!(draw(&needing_review_only(app()), 80, 7).backend());
    }

    #[test]
    fn review_tab_showing_reviewed() {
        insta::assert_snapshot!(draw(&app(), 120, 8).backend());
    }

    #[test]
    fn mine_tab() {
        let mut app = app();
        app.tab = Tab::Mine;
        insta::assert_snapshot!(draw(&app, 120, 6).backend());
    }

    #[test]
    fn mine_tab_with_snooze_popup() {
        let mut app = app();
        app.tab = Tab::Mine;
        app.snoozing = true;
        insta::assert_snapshot!(draw(&app, 120, 10).backend());
    }

    #[test]
    fn header_warnings_and_daemon_down() {
        let mut app = app();
        app.status = HeaderStatus {
            health: Health::DaemonDown,
            errors: vec!["personal: gh auth token failed (last ok 10m ago)".into()],
            degraded: Some("notifications are off for gh-overview".into()),
            muted_until: None,
        };
        insta::assert_snapshot!(draw(&app, 100, 4).backend());
    }

    #[test]
    fn header_shows_a_mute() {
        let mut app = needing_review_only(app());
        app.status.muted_until = Some(
            t(0).with_timezone(&Local)
                .date_naive()
                .and_hms_opt(23, 30, 0)
                .unwrap()
                .and_local_timezone(Local)
                .unwrap()
                .with_timezone(&chrono::Utc),
        );
        let backend = draw(&app, 120, 4);
        let header: String = (0..120)
            .map(|x| backend.backend().buffer()[(x, 0)].symbol().to_string())
            .collect();
        assert!(
            header.contains("🔕") && header.contains("muted until 23:30"),
            "{header}"
        );
        assert!(header.contains("● polled 12s ago"), "{header}");
    }

    fn footer(app: &App) -> String {
        let backend = draw(app, 120, 7);
        (0..120)
            .map(|x| backend.backend().buffer()[(x, 6)].symbol().to_string())
            .collect()
    }

    fn press(app: &mut App, c: char) {
        app.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE), t(0));
    }

    #[test]
    fn footer_offers_snooze_only_on_a_row_with_an_alert() {
        let mut app = needing_review_only(app());
        assert!(footer(&app).contains("s snooze"), "{}", footer(&app));
        press(&mut app, 'j');
        assert!(!footer(&app).contains("s snooze"), "{}", footer(&app));
        assert!(footer(&app).contains("d done"), "{}", footer(&app));
    }

    #[test]
    fn footer_says_why_s_did_nothing() {
        let mut app = needing_review_only(app());
        press(&mut app, 'j');
        press(&mut app, 's');
        assert!(
            footer(&app).contains("#77 isn't pinging, nothing to snooze"),
            "{}",
            footer(&app)
        );
    }

    #[test]
    fn mine_tab_narrow() {
        let mut app = app();
        app.tab = Tab::Mine;
        insta::assert_snapshot!(draw(&app, 80, 6).backend());
    }

    #[test]
    fn columns_shrink_with_the_terminal() {
        let titles = |tab, width| columns(tab, width).iter().map(|c| c.title).collect::<Vec<_>>();
        assert_eq!(
            titles(Tab::Review, 120),
            vec!["REPO", "#", "AUTHOR", "AGE", "ALERT", "ACCT", "VIA"]
        );
        assert_eq!(
            titles(Tab::Review, 99),
            vec!["REPO", "#", "AGE", "ALERT", "ACCT", "VIA"]
        );
        assert_eq!(titles(Tab::Review, 84), vec!["REPO", "#", "ALERT", "ACCT", "VIA"]);
        let fixed: u16 = columns(Tab::Review, 80).iter().map(|c| c.width).sum();
        assert!(fixed + 5 * SPACING + 2 <= 70);
        assert_eq!(titles(Tab::Mine, 100), vec!["REPO", "#", "WHY", "ALERT", "ACCT"]);
        assert_eq!(titles(Tab::Mine, 99), vec!["REPO", "#", "WHY", "ALERT"]);
        let fixed: u16 = columns(Tab::Mine, 80).iter().map(|c| c.width).sum();
        assert!(fixed + 4 * SPACING + 2 <= 64);
    }

    #[test]
    fn truncation_measures_display_width() {
        assert_eq!(truncate("short", 10), "short");
        assert_eq!(truncate("short", 0), "");
        assert_eq!(truncate("abcdefghij", 5), "abcd…");
        assert_eq!(truncate("修复登录错误", 7), "修复登…");
        assert_eq!(display_width(&truncate("修复登录错误", 7)), 7);
    }
}
