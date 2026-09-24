# gh-overview (`ghov`)

A terminal overview of the GitHub pull requests waiting on you, across several `gh` accounts, plus a macOS background daemon that notifies you when someone reviews or comments on your PRs, re-notifies every 5 minutes until you act, and lets you snooze.

- **To review**: open PRs where you (or one of your teams) are a requested reviewer.
- **My PRs**: your open PRs where the ball is in your court: an unresolved review thread where someone else has the last word, changes requested on the current head, or a human comment you haven't answered.

```
 gh-overview   [1] To review (2)   [2] My PRs (1)                                                      ● polled 12s ago
────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────
  REPO                         #      TITLE                         WHY                            ALERT      ACCT
▶ octocat/dotfiles             310    [draft] PR 310                3 threads · changes: alice     🔔 pinging personal
────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────
 enter open · s snooze · d done · r refresh · tab/1/2 switch · j/k move · q quit
```

## Requirements

- macOS 13+ (the TUI and the polling core are portable, but the daemon service and notifications are macOS-only for now)
- [`gh`](https://cli.github.com) logged in to every account you want to watch (`gh auth status`)
- Xcode or the Command Line Tools (`swiftc`), used at build time for the small notifier app that `ghov install` sets up
- Rust 1.89+

## Install

```bash
git clone https://github.com/pdrgds/gh-overview
cd gh-overview
cargo install --path .
ghov install
```

`ghov install` writes the notifier app to `~/Applications/GhOverview Notifier.app`, asks macOS for notification permission, writes `~/Library/LaunchAgents/dev.pdrgds.gh-overview.plist` and starts the daemon. It copies your current `PATH` into the agent so it can find `gh`; run it again after moving `gh`.

When macOS asks whether **gh-overview** may send notifications, choose **Options → Allow** (clicking the banner itself counts as Don't Allow). Then, in **System Settings → Notifications → gh-overview**, set the style to **Alerts** so notifications stay on screen until you act. Until permission is granted, the TUI header says so and the daemon falls back to `osascript` notifications, which have no buttons and may not appear at all; about a minute after you allow notifications it switches back and re-shows the alerts that are still pinging.

## Use

```bash
ghov            # open the TUI
ghov status     # daemon health, last poll per account, active alerts
ghov debug fetch --account <login>   # what the daemon sees for one account
ghov uninstall  # stop the daemon, clear its notifications, remove the LaunchAgent and notifier app
```

| Key | Action |
|---|---|
| `1` / `2` / `Tab` | switch tab |
| `j` `k` / `↑` `↓`, `g` `G` | move, top, bottom |
| `Enter` / `o` | open in browser; on My PRs this also acknowledges (pauses pings for 30 minutes) |
| `s` | snooze the selected PR's alert |
| `d` | done: hide the PR until new activity arrives |
| `r` | ask the daemon to poll now |
| `q` | quit |

Notifications: clicking the body opens the PR and pauses the pings for 30 minutes (`remind_after_open`), after which they resume if the PR still needs you; a Snooze choice from the notification's Options menu pauses them; closing the notification does not count as acting, so it pings again after 5 minutes. Pings stop on their own once the PR no longer needs you (threads resolved, you pushed after a changes request, or you replied). Approvals and other news that leave nothing to do notify once, without a Snooze button. Empty bot reviews (for example a CodeRabbit pass with no new comments) don't notify at all.

## Configuration

`~/.config/gh-overview/config.toml` is created on first run from `gh auth status`. Rename the labels to taste:

```toml
poll_interval = "1m"
renotify_interval = "5m"
remind_after_open = "30m"
tomorrow_hour = 9
snooze_choices = ["15m", "1h", "tomorrow"]
extra_bots = []

[[accounts]]
login = "octocat-work"
label = "work"
browser = { app = "Google Chrome", args = ["--profile-directory=Profile 1"] }

[[accounts]]
login = "octocat"
label = "personal"
```

`browser` is optional. It opens that account's PRs, from the TUI and from notification clicks, in a given app instead of the default browser; `args` are passed to the app, for example a Chrome profile directory (`chrome://version` shows it as the last part of **Profile Path**).

`extra_bots` lists machine-user logins that should be treated as bots (their conversation comments never notify).

The daemon reads the config once at startup: after editing it, or after upgrading `ghov` (which also updates the notifier app), run `ghov install` again to restart it. Only one daemon runs at a time.

## Files

- State: `~/.local/share/gh-overview/state.db`
- Daemon log: `~/.local/state/gh-overview/daemon.log` (trimmed to its last ~1 MB at startup once it passes 5 MB)

The app directories are created readable only by you (`0700`).

macOS may keep listing **gh-overview** in System Settings → Notifications after `ghov uninstall`; the entry is harmless.

## How it works

- Every poll (1 minute by default) the daemon runs one GraphQL query per account against `api.github.com`, with a token it asks `gh auth token --user <login>` for on the spot; tokens are never written anywhere.
- What it has seen, the PR rows and each PR's alert state live in a local SQLite database that the TUI reads; the TUI sends acknowledgements, snoozes and refresh requests to the daemon through the same database.
- Notifications go through `GhOverview Notifier.app`, a small Swift helper (`notifier/main.swift`) that `ghov` embeds at build time and runs as one long-lived process next to the daemon, so clicks and snoozes on a notification reach the daemon.
- Nothing leaves your machine except the GitHub API requests.

## Development

```bash
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

TUI rendering is covered by [insta](https://insta.rs) snapshots under `src/tui/snapshots`; review changes with `cargo insta review`. `build.rs` compiles the Swift notifier with `swiftc` on macOS; on other platforms it is skipped.

Issues and pull requests are welcome, especially for Linux support (a systemd user service and a notifier with actions).

## License

[MIT](LICENSE)
