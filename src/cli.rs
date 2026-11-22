use std::{ffi::OsString, path::PathBuf, time::Duration};

use anyhow::{Result, bail};
use clap::{ArgGroup, Parser, error::ErrorKind};

pub const MINIMUM_WATCH_INTERVAL: u64 = 10;

#[derive(Debug, Parser)]
#[command(name = "rz", version, about, no_binary_name = true)]
#[command(group(ArgGroup::new("action").args([
    "save", "watch", "watch_status", "watch_stop", "clean", "list",
    "install_pi_extension", "replay_scrollback", "watch_worker", "ui",
])))]
#[command(group(ArgGroup::new("restore").args([
    "session", "keep_existing", "dry_run",
]).multiple(true).conflicts_with("action")))]
#[command(group(ArgGroup::new("window_action").args(["save", "watch"])))]
struct Cli {
    /// Open the interactive workspace and storage manager
    #[arg(long)]
    ui: bool,
    /// Save a named Ghostty workspace
    #[arg(long, value_name = "NAME")]
    save: Option<String>,
    /// Skip terminal history when saving
    #[arg(long, requires = "save")]
    no_scrollback: bool,
    /// Save only the front window (the default for watchers)
    #[arg(long, requires = "window_action", conflicts_with_all = ["all_windows", "window_id"])]
    current_window: bool,
    #[arg(long, requires = "save", hide = true)]
    window_id: Option<String>,
    /// Start a watcher that stops when this shell exits
    #[arg(long, value_name = "NAME", requires = "every")]
    watch: Option<String>,
    /// Watch interval, such as 30s, 15m, or 1h
    #[arg(long, requires = "watch", value_name = "INTERVAL", value_parser = parse_interval)]
    every: Option<u64>,
    /// Watch all Ghostty windows
    #[arg(long, requires = "watch")]
    all_windows: bool,
    /// Show this shell's watcher
    #[arg(long)]
    watch_status: bool,
    /// Stop this shell's watcher
    #[arg(long)]
    watch_stop: bool,
    /// Remove snapshots older than AGE (default: 7d)
    #[arg(long, num_args = 0..=1, value_name = "AGE", default_missing_value = "7d", value_parser = parse_age)]
    clean: Option<u64>,
    /// List snapshots, or inspect a number from the list
    #[arg(long, num_args = 0..=1, value_name = "NUMBER", value_parser = parse_number)]
    list: Option<Option<usize>>,
    /// Restore the newest snapshot with this name, timestamp, or ID
    #[arg(long, value_name = "SELECTOR")]
    session: Option<String>,
    /// Leave existing windows open after restoring
    #[arg(long)]
    keep_existing: bool,
    /// Preview a restore without changing Ghostty
    #[arg(long)]
    dry_run: bool,
    /// Install or update the Pi session tracker
    #[arg(long)]
    install_pi_extension: bool,
    #[arg(long, hide = true)]
    replay_scrollback: Option<PathBuf>,
    #[arg(long, num_args = 8, hide = true)]
    watch_worker: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Ui,
    Save(SaveOptions),
    Restore(RestoreOptions),
    List(Option<usize>),
    Clean { age_seconds: u64 },
    StartWatch(WatchOptions),
    WatchStatus,
    WatchStop,
    WatchWorker(WorkerOptions),
    InstallPiExtension,
    ReplayScrollback(PathBuf),
    Print(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SaveOptions {
    pub name: String,
    pub capture_scrollback: bool,
    pub current_window: bool,
    pub window_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreOptions {
    pub selector: Option<String>,
    pub dry_run: bool,
    pub close_existing: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatchOptions {
    pub name: String,
    pub interval_seconds: u64,
    pub scope: WatchScope,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchScope {
    CurrentWindow,
    AllWindows,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerOptions {
    pub token: String,
    pub name: String,
    pub interval_seconds: u64,
    pub owner_pid: u32,
    pub owner_started_at: String,
    pub scope: WatchScope,
    pub window_id: Option<String>,
    pub state_path: String,
}

pub fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Command> {
    let cli = match Cli::try_parse_from(args) {
        Ok(cli) => cli,
        Err(error)
            if matches!(
                error.kind(),
                ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
            ) =>
        {
            return Ok(Command::Print(error.to_string()));
        }
        Err(error) => return Err(error.into()),
    };
    Ok(if cli.ui {
        Command::Ui
    } else if let Some(name) = cli.save {
        Command::Save(SaveOptions {
            name,
            capture_scrollback: !cli.no_scrollback,
            current_window: cli.current_window,
            window_id: cli.window_id,
        })
    } else if let Some(name) = cli.watch {
        Command::StartWatch(WatchOptions {
            name,
            interval_seconds: cli.every.expect("Clap requires --every"),
            scope: if cli.all_windows {
                WatchScope::AllWindows
            } else {
                WatchScope::CurrentWindow
            },
        })
    } else if cli.watch_status {
        Command::WatchStatus
    } else if cli.watch_stop {
        Command::WatchStop
    } else if let Some(args) = cli.watch_worker {
        Command::WatchWorker(WorkerOptions {
            token: args[0].clone(),
            name: args[1].clone(),
            interval_seconds: args[2].parse()?,
            owner_pid: args[3].parse()?,
            owner_started_at: args[4].clone(),
            scope: match args[5].as_str() {
                "current-window" => WatchScope::CurrentWindow,
                "all-windows" => WatchScope::AllWindows,
                _ => bail!("invalid watcher scope"),
            },
            window_id: (args[6] != "-").then(|| args[6].clone()),
            state_path: args[7].clone(),
        })
    } else if let Some(age_seconds) = cli.clean {
        Command::Clean { age_seconds }
    } else if let Some(number) = cli.list {
        Command::List(number)
    } else if cli.install_pi_extension {
        Command::InstallPiExtension
    } else if let Some(path) = cli.replay_scrollback {
        Command::ReplayScrollback(path)
    } else {
        Command::Restore(RestoreOptions {
            selector: cli.session,
            dry_run: cli.dry_run,
            close_existing: !cli.keep_existing,
        })
    })
}

fn parse_number(value: &str) -> Result<usize> {
    let number = value.parse::<usize>()?;
    if number == 0 || value.starts_with('0') {
        bail!("use a number shown by rz --list");
    }
    Ok(number)
}

fn parse_duration(value: &str, minimum: u64) -> Result<u64> {
    // Retain legacy spellings such as 2.days and bare seconds.
    let value = value.trim().to_ascii_lowercase();
    let split = value
        .find(|character: char| character.is_ascii_alphabetic())
        .unwrap_or(value.len());
    let (number, unit) = value.split_at(split);
    let number = number.strip_suffix('.').unwrap_or(number);
    let normalized = format!(
        "{}{number}{}",
        if number.starts_with('.') { "0" } else { "" },
        if unit.is_empty() { "s" } else { unit }
    );
    let duration = humantime::parse_duration(&normalized)?;
    let seconds = duration
        .as_secs()
        .checked_add(u64::from(duration.subsec_nanos() >= 500_000_000))
        .ok_or_else(|| anyhow::anyhow!("duration is too large"))?;
    if seconds < minimum || duration > Duration::from_secs(i64::MAX as u64) {
        bail!("duration must be at least {minimum} seconds and fit in a signed 64-bit value");
    }
    Ok(seconds)
}

fn parse_interval(value: &str) -> Result<u64> {
    parse_duration(value, MINIMUM_WATCH_INTERVAL)
}

pub(crate) fn parse_age(value: &str) -> Result<u64> {
    parse_duration(value, 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(values: &[&str]) -> Vec<OsString> {
        values.iter().map(OsString::from).collect()
    }

    #[test]
    fn parses_fast_current_window_save() {
        let command = parse(strings(&[
            "--save",
            "backup",
            "--no-scrollback",
            "--current-window",
        ]))
        .unwrap();
        assert_eq!(
            command,
            Command::Save(SaveOptions {
                name: "backup".into(),
                capture_scrollback: false,
                current_window: true,
                window_id: None,
            })
        );
    }

    #[test]
    fn rejects_two_window_selectors() {
        let error = parse(strings(&[
            "--save",
            "backup",
            "--current-window",
            "--window-id",
            "window-1",
        ]))
        .unwrap_err();
        assert!(error.to_string().contains("cannot be used with"));
    }

    #[test]
    fn parses_restore_modes() {
        let command = parse(strings(&[
            "--keep-existing",
            "--session",
            "work",
            "--dry-run",
        ]))
        .unwrap();
        assert_eq!(
            command,
            Command::Restore(RestoreOptions {
                selector: Some("work".into()),
                dry_run: true,
                close_existing: false,
            })
        );
    }

    #[test]
    fn parses_watch_intervals_and_scopes() {
        let command = parse(strings(&["--watch", "backup", "--every", "15m"])).unwrap();
        assert_eq!(
            command,
            Command::StartWatch(WatchOptions {
                name: "backup".into(),
                interval_seconds: 900,
                scope: WatchScope::CurrentWindow,
            })
        );
        assert!(parse(strings(&["--watch", "backup", "--every", "5s"])).is_err());
    }

    #[test]
    fn parses_friendly_cleanup_ages() {
        assert_eq!(parse_age("2.days").unwrap(), 172_800);
        assert_eq!(parse_age("2weeks").unwrap(), 1_209_600);
        assert_eq!(parse_age("12h").unwrap(), 43_200);
        assert_eq!(parse_age("1.5.hours").unwrap(), 5_400);
        assert_eq!(parse_age(".5h").unwrap(), 1_800);
        assert_eq!(parse_age("90").unwrap(), 90);
        assert_eq!(parse_age("1.5").unwrap(), 2);
        assert_eq!(parse_age("1h 30m").unwrap(), 5_400);
        assert!(parse_age("last-week").is_err());
        assert!(parse_age("0").is_err());
        assert!(parse_age("-1h").is_err());
        assert!(parse_age("NaN").is_err());
    }

    #[test]
    fn validates_list_number() {
        assert_eq!(parse(strings(&["--list"])).unwrap(), Command::List(None));
        assert_eq!(
            parse(strings(&["--list", "12"])).unwrap(),
            Command::List(Some(12))
        );
        for value in ["0", "01", "nope", "-1"] {
            assert!(parse(strings(&["--list", value])).is_err());
        }
    }

    #[test]
    fn preserves_default_restore_help_and_internal_worker_protocol() {
        assert_eq!(
            parse(strings(&[])).unwrap(),
            Command::Restore(RestoreOptions {
                selector: None,
                dry_run: false,
                close_existing: true,
            })
        );
        assert!(
            matches!(parse(strings(&["--help"])).unwrap(), Command::Print(output) if output.contains("--save") && !output.contains("--watch-worker"))
        );
        assert!(
            matches!(parse(strings(&["-V"])).unwrap(), Command::Print(output) if output.trim() == concat!("rz ", env!("CARGO_PKG_VERSION")))
        );
        assert!(
            matches!(parse(strings(&["--watch-worker", "token", "work", "900", "42", "Sun Nov 22 10:00:00 2026", "all-windows", "-", "/tmp/state.json"])).unwrap(), Command::WatchWorker(options) if options.window_id.is_none() && options.owner_pid == 42)
        );
        assert!(parse(strings(&["--watch-worker", "too", "few"])).is_err());
    }

    #[test]
    fn rejects_conflicting_modes_and_orphaned_options() {
        for values in [
            vec!["--save", "work", "--dry-run"],
            vec!["--list", "--session", "work"],
            vec!["--clean", "--watch-stop"],
            vec!["--no-scrollback"],
            vec!["--every", "15m"],
            vec!["--current-window"],
            vec!["--ui", "--session", "work"],
            vec!["--watch", "work"],
            vec![
                "--watch",
                "work",
                "--every",
                "15m",
                "--current-window",
                "--all-windows",
            ],
        ] {
            assert!(parse(strings(&values)).is_err(), "{values:?}");
        }
    }
}
