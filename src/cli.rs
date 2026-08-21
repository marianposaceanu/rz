use std::ffi::OsString;

use anyhow::{Result, bail};

pub const USAGE: &str = "Usage:
  rz --save NAME
  rz --save NAME [--no-scrollback] [--current-window]
  rz --watch NAME --every INTERVAL [--current-window | --all-windows]
  rz --watch-status
  rz --watch-stop
  rz --clean [AGE]
  rz [--keep-existing]
  rz --session NAME_OR_TIMESTAMP [--keep-existing] [--dry-run]
  rz --dry-run [--keep-existing]
  rz --list [NUMBER]
  rz --version";

const MINIMUM_WATCH_INTERVAL: u64 = 10;
const DEFAULT_CLEAN_AGE: u64 = 7 * 24 * 60 * 60;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Save(SaveOptions),
    Restore(RestoreOptions),
    List(Option<usize>),
    Clean { age_seconds: u64 },
    StartWatch(WatchOptions),
    WatchStatus,
    WatchStop,
    WatchWorker(WorkerOptions),
    Help,
    Version,
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
    let args = args
        .into_iter()
        .map(|arg| {
            arg.into_string()
                .map_err(|_| anyhow::anyhow!("arguments must be valid UTF-8"))
        })
        .collect::<Result<Vec<_>>>()?;

    match args.first().map(String::as_str) {
        Some("--save") => parse_save(&args[1..]).map(Command::Save),
        Some("--watch") => parse_watch(&args[1..]).map(Command::StartWatch),
        Some("--watch-status") if args.len() == 1 => Ok(Command::WatchStatus),
        Some("--watch-stop") if args.len() == 1 => Ok(Command::WatchStop),
        Some("--watch-worker") => parse_worker(&args[1..]).map(Command::WatchWorker),
        Some("--clean") => parse_clean(&args[1..]),
        Some("--list") => parse_list(&args[1..]).map(Command::List),
        Some("--help") if args.len() == 1 => Ok(Command::Help),
        Some("--version" | "-V") if args.len() == 1 => Ok(Command::Version),
        _ => parse_restore(&args).map(Command::Restore),
    }
}

fn parse_save(args: &[String]) -> Result<SaveOptions> {
    let Some(name) = args.first() else {
        bail!("missing snapshot name");
    };
    let mut options = SaveOptions {
        name: name.clone(),
        capture_scrollback: true,
        current_window: false,
        window_id: None,
    };
    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--no-scrollback" => options.capture_scrollback = false,
            "--current-window" => options.current_window = true,
            "--window-id" => {
                index += 1;
                options.window_id = Some(
                    args.get(index)
                        .ok_or_else(|| anyhow::anyhow!("missing value for --window-id"))?
                        .clone(),
                );
            }
            option => bail!("unknown save option {option:?}"),
        }
        index += 1;
    }
    if options.current_window && options.window_id.is_some() {
        bail!("use either --current-window or --window-id, not both");
    }
    Ok(options)
}

fn parse_restore(args: &[String]) -> Result<RestoreOptions> {
    let mut options = RestoreOptions {
        selector: None,
        dry_run: false,
        close_existing: true,
    };
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--session" => {
                if options.selector.is_some() {
                    bail!("use --session only once");
                }
                index += 1;
                options.selector = Some(
                    args.get(index)
                        .ok_or_else(|| anyhow::anyhow!("missing value for --session"))?
                        .clone(),
                );
            }
            "--dry-run" => options.dry_run = true,
            "--keep-existing" => options.close_existing = false,
            option => bail!("unknown restore option {option:?}"),
        }
        index += 1;
    }
    Ok(options)
}

fn parse_watch(args: &[String]) -> Result<WatchOptions> {
    let Some(name) = args.first() else {
        bail!("missing watcher snapshot name");
    };
    let mut interval_seconds = None;
    let mut scope = WatchScope::CurrentWindow;
    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--every" => {
                index += 1;
                interval_seconds =
                    Some(parse_interval(args.get(index).ok_or_else(|| {
                        anyhow::anyhow!("missing value for --every")
                    })?)?);
            }
            "--current-window" => scope = WatchScope::CurrentWindow,
            "--all-windows" => scope = WatchScope::AllWindows,
            option => bail!("unknown watcher option {option:?}"),
        }
        index += 1;
    }
    Ok(WatchOptions {
        name: name.clone(),
        interval_seconds: interval_seconds
            .ok_or_else(|| anyhow::anyhow!("missing --every INTERVAL"))?,
        scope,
    })
}

fn parse_worker(args: &[String]) -> Result<WorkerOptions> {
    if args.len() != 8 {
        bail!("malformed watcher worker arguments");
    }
    let interval_seconds = args[2].parse()?;
    let owner_pid = args[3].parse()?;
    let scope = match args[5].as_str() {
        "current-window" => WatchScope::CurrentWindow,
        "all-windows" => WatchScope::AllWindows,
        _ => bail!("invalid watcher scope"),
    };
    Ok(WorkerOptions {
        token: args[0].clone(),
        name: args[1].clone(),
        interval_seconds,
        owner_pid,
        owner_started_at: args[4].clone(),
        scope,
        window_id: (args[6] != "-").then(|| args[6].clone()),
        state_path: args[7].clone(),
    })
}

fn parse_list(args: &[String]) -> Result<Option<usize>> {
    if args.is_empty() {
        return Ok(None);
    }
    if args.len() != 1 || args[0].starts_with('0') {
        bail!("use rz --list [NUMBER] with a number shown by rz --list");
    }
    let number = args[0]
        .parse::<usize>()
        .map_err(|_| anyhow::anyhow!("use rz --list [NUMBER] with a number shown by rz --list"))?;
    if number == 0 {
        bail!("use rz --list [NUMBER] with a number shown by rz --list");
    }
    Ok(Some(number))
}

fn parse_clean(args: &[String]) -> Result<Command> {
    if args.len() > 1 {
        bail!("use rz --clean [AGE] with a single age such as 7d or 2.weeks");
    }
    let age_seconds = match args.first() {
        Some(value) => parse_age(value)?,
        None => DEFAULT_CLEAN_AGE,
    };
    Ok(Command::Clean { age_seconds })
}

fn parse_interval(value: &str) -> Result<u64> {
    let split = value
        .char_indices()
        .find(|(_, character)| !character.is_ascii_digit() && *character != '.')
        .map_or(value.len(), |(index, _)| index);
    let (number, unit) = value.split_at(split);
    let multiplier = match unit.to_ascii_lowercase().as_str() {
        "" | "s" => 1.0,
        "m" => 60.0,
        "h" => 3_600.0,
        _ => bail!(
            "invalid interval {value:?}; use seconds, minutes, or hours such as 30s, 15m, or 1h"
        ),
    };
    let seconds = parse_positive_float(number, "interval", value, multiplier)?;
    if seconds < MINIMUM_WATCH_INTERVAL {
        bail!("watch interval must be at least {MINIMUM_WATCH_INTERVAL} seconds");
    }
    Ok(seconds)
}

fn parse_age(value: &str) -> Result<u64> {
    let split = value
        .char_indices()
        .find(|(_, character)| !character.is_ascii_digit() && *character != '.')
        .map_or(value.len(), |(index, _)| index);
    let (number, raw_unit) = value.split_at(split);
    let unit = raw_unit.trim_start_matches('.').to_ascii_lowercase();
    let multiplier = match unit.as_str() {
        "" | "s" | "second" | "seconds" => 1.0,
        "m" | "minute" | "minutes" => 60.0,
        "h" | "hour" | "hours" => 3_600.0,
        "d" | "day" | "days" => 86_400.0,
        "w" | "week" | "weeks" => 604_800.0,
        _ => bail!("invalid cleanup age {value:?}; use values such as 12h, 7d, 7.days, or 2.weeks"),
    };
    let seconds = parse_positive_float(number, "cleanup age", value, multiplier)?;
    if seconds < 1 {
        bail!("cleanup age must be at least one second");
    }
    Ok(seconds)
}

fn parse_positive_float(number: &str, kind: &str, original: &str, multiplier: f64) -> Result<u64> {
    let parsed = number.parse::<f64>().map_err(|_| {
        anyhow::anyhow!("invalid {kind} {original:?}; expected a positive number and unit")
    })?;
    if !parsed.is_finite() || parsed < 0.0 {
        bail!("invalid {kind} {original:?}; expected a positive number and unit");
    }
    Ok((parsed * multiplier).round() as u64)
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
        assert!(
            error
                .to_string()
                .contains("either --current-window or --window-id")
        );
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
        assert!(parse_age("last-week").is_err());
    }

    #[test]
    fn validates_list_number() {
        assert_eq!(parse_list(&[]).unwrap(), None);
        assert_eq!(parse_list(&["12".into()]).unwrap(), Some(12));
        assert!(parse_list(&["0".into()]).is_err());
    }
}
