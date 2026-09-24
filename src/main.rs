use anyhow::Result;
use chrono::Utc;
use clap::{Parser, Subcommand};
use gh_overview::config::{self, Config};
use gh_overview::github::client::{GithubClient, GithubSource};
use gh_overview::github::token::GhCli;
use gh_overview::notify::bundle;
use gh_overview::paths::Paths;
use gh_overview::service::ServiceInstaller;
use gh_overview::service::launchd::Launchd;
use gh_overview::store::Store;
use gh_overview::{daemon, report, tui};
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(name = "ghov", version, about = "GitHub PR to-do overview and review notifier")]
struct Cli {
    #[command(subcommand)]
    command: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    Daemon,
    Install,
    Uninstall,
    Status,
    Debug {
        #[command(subcommand)]
        command: DebugCmd,
    },
}

#[derive(Subcommand)]
enum DebugCmd {
    Fetch {
        #[arg(long)]
        account: String,
    },
}

fn init_logging() {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .init();
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let paths = Paths::discover()?;
    paths.ensure_dirs()?;
    if matches!(cli.command, Some(Cmd::Uninstall)) {
        Launchd::for_current_user()?.uninstall()?;
        bundle::clear_and_remove(&bundle::current_user_app()?)?;
        println!("uninstalled LaunchAgent and notifier app");
        return Ok(());
    }
    let config: Config = config::load_or_create(&paths.config_file, config::discover_gh_accounts)?;
    match cli.command {
        None => tui::run(&paths, &config),
        Some(Cmd::Daemon) => {
            init_logging();
            daemon::run(&paths, config)
        }
        Some(Cmd::Install) => {
            let exe = std::env::current_exe()?;
            if exe.components().any(|c| c.as_os_str() == "target") {
                eprintln!(
                    "warning: {} is a build artifact; run `cargo install --path .` first so the agent survives `cargo clean`",
                    exe.display()
                );
            }
            let env: Vec<(String, String)> = ["PATH", "XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_STATE_HOME"]
                .into_iter()
                .filter_map(|name| std::env::var(name).ok().map(|value| (name.to_string(), value)))
                .collect();
            if let Some(binary) = bundle::NOTIFIER_BINARY {
                let app = bundle::current_user_app()?;
                bundle::write_bundle(&app, binary)?;
                bundle::sign(&app)?;
                bundle::request_permission(&app)?;
                println!("installed {}", app.display());
                println!("if macOS asks whether gh-overview may send notifications, choose Options → Allow");
            }
            Launchd::for_current_user()?.install(&exe, &paths.log_file, &env)?;
            println!("installed LaunchAgent running {} daemon", exe.display());
            println!("log: {}", paths.log_file.display());
            Ok(())
        }
        Some(Cmd::Uninstall) => Ok(()),
        Some(Cmd::Status) => {
            let store = Store::open(&paths.db_file)?;
            print!("{}", report::status_report(&store.db(), &config, Utc::now())?);
            Ok(())
        }
        Some(Cmd::Debug {
            command: DebugCmd::Fetch { account },
        }) => {
            init_logging();
            let snapshot = GithubClient::new(GhCli)?.fetch(&account)?;
            print!("{}", report::snapshot_report(&snapshot, &config.identity()));
            Ok(())
        }
    }
}
