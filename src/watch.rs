use std::{
    fs::{self, OpenOptions},
    os::unix::process::CommandExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration as StdDuration, Instant},
};

use anyhow::{Context, Result, bail};
use chrono::{Duration, Local};
use nix::{
    sys::signal::{Signal, kill},
    unistd::{Pid, getppid},
};
use signal_hook::consts::signal::{SIGHUP, SIGINT, SIGTERM};

use crate::{
    App,
    app::{random_hex, validate_name},
    cli::{WatchOptions, WatchScope, WorkerOptions},
    command, ghostty,
    model::WatchState,
    ui,
};

const MINIMUM_WATCH_INTERVAL: u64 = 10;

impl App {
    pub(crate) fn start_watch(&self, options: WatchOptions) -> Result<()> {
        validate_name(&options.name)?;
        ghostty::ensure_running()?;
        let owner_pid = u32::try_from(getppid().as_raw())?;
        let owner_started_at = process_start_marker(owner_pid);
        if owner_started_at.is_empty() {
            bail!("could not identify the owning shell");
        }
        fs::create_dir_all(&self.watchers_dir)?;
        let state_path = self.watch_state_path(owner_pid);
        if read_watch_state(&state_path).is_some_and(|state| watcher_alive(&state)) {
            let state = read_watch_state(&state_path).expect("watch state exists");
            bail!(
                "this shell already owns watcher {:?}; run rz --watch-status",
                state.name
            );
        }
        let _ = fs::remove_file(&state_path);

        let window_id = match options.scope {
            WatchScope::CurrentWindow => Some(ghostty::current_window_id()?),
            WatchScope::AllWindows => None,
        };
        let token = random_hex(16)?;
        let log_path = self.watchers_dir.join(format!("shell-{owner_pid}.log"));
        let log = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)?;
        let executable = std::env::current_exe()?;
        let scope = match options.scope {
            WatchScope::CurrentWindow => "current-window",
            WatchScope::AllWindows => "all-windows",
        };
        let worker_arguments = [
            "--watch-worker".to_owned(),
            token.clone(),
            options.name.clone(),
            options.interval_seconds.to_string(),
            owner_pid.to_string(),
            owner_started_at.clone(),
            scope.to_owned(),
            window_id.clone().unwrap_or_else(|| "-".into()),
            state_path.to_string_lossy().into_owned(),
        ];
        let mut worker = Command::new(executable)
            .args(worker_arguments)
            .stdin(Stdio::null())
            .stdout(Stdio::from(log.try_clone()?))
            .stderr(Stdio::from(log))
            .process_group(0)
            .spawn()
            .context("failed to start snapshot watcher")?;
        let worker_pid = worker.id();

        let state = WatchState {
            version: 1,
            token,
            name: options.name.clone(),
            interval_seconds: options.interval_seconds,
            scope: scope.into(),
            window_id: window_id.clone(),
            owner_pid,
            owner_started_at,
            worker_pid,
            started_at: Local::now().to_rfc3339(),
            log_path: log_path.clone(),
            last_attempt_at: None,
            last_success_at: None,
            last_snapshot_id: None,
            next_attempt_at: None,
            last_error: None,
        };
        if let Err(error) = write_watch_state(&state_path, &state) {
            let _ = worker.kill();
            return Err(error);
        }
        drop(worker);

        ui::print_box(
            "WATCHER STARTED",
            &[
                ui::labeled("NAME", &options.name),
                ui::labeled("EVERY", display_interval(options.interval_seconds)),
                ui::labeled(
                    "SCOPE",
                    window_id.map_or_else(
                        || "all Ghostty windows".into(),
                        |id| format!("bound to Ghostty window {id}"),
                    ),
                ),
                ui::labeled("MODE", "fast snapshots; scrollback skipped"),
                ui::labeled(
                    "OWNER",
                    format!("shell PID {owner_pid}; watcher stops when this shell exits"),
                ),
                ui::labeled("LOG", log_path.to_string_lossy()),
            ],
        );
        Ok(())
    }

    pub(crate) fn watch_status(&self) -> Result<()> {
        let state_path = self.watch_state_path(u32::try_from(getppid().as_raw())?);
        let Some(state) = read_watch_state(&state_path).filter(watcher_alive) else {
            let _ = fs::remove_file(state_path);
            ui::print_box(
                "WATCHER STATUS",
                &["No active watcher belongs to this shell.".into()],
            );
            return Ok(());
        };
        ui::print_box(
            "WATCHER STATUS",
            &[
                ui::labeled("STATUS", "running"),
                ui::labeled("NAME", &state.name),
                ui::labeled("EVERY", display_interval(state.interval_seconds)),
                ui::labeled(
                    "SCOPE",
                    state.window_id.as_ref().map_or_else(
                        || "all Ghostty windows".into(),
                        |id| format!("Ghostty window {id}"),
                    ),
                ),
                ui::labeled("WORKER", format!("PID {}", state.worker_pid)),
                ui::labeled("STARTED", display_time(&state.started_at)),
                ui::labeled(
                    "LAST SAVE",
                    state
                        .last_success_at
                        .as_deref()
                        .map(display_time)
                        .unwrap_or_else(|| "pending".into()),
                ),
                ui::labeled(
                    "SNAPSHOT",
                    state.last_snapshot_id.as_deref().unwrap_or("pending"),
                ),
                ui::labeled(
                    "NEXT SAVE",
                    state
                        .next_attempt_at
                        .as_deref()
                        .map(display_time)
                        .unwrap_or_else(|| "pending".into()),
                ),
                ui::labeled("LAST ERROR", state.last_error.as_deref().unwrap_or("none")),
                ui::labeled("LOG", state.log_path.to_string_lossy()),
            ],
        );
        Ok(())
    }

    pub(crate) fn stop_watch(&self) -> Result<()> {
        let state_path = self.watch_state_path(u32::try_from(getppid().as_raw())?);
        let Some(state) = read_watch_state(&state_path).filter(watcher_alive) else {
            let _ = fs::remove_file(state_path);
            ui::print_box(
                "WATCHER STOPPED",
                &["No active watcher belonged to this shell.".into()],
            );
            return Ok(());
        };
        kill(
            Pid::from_raw(i32::try_from(state.worker_pid)?),
            Signal::SIGTERM,
        )?;
        let deadline = Instant::now() + StdDuration::from_secs(5);
        while process_alive(state.worker_pid) && Instant::now() < deadline {
            thread::sleep(StdDuration::from_millis(100));
        }
        if process_alive(state.worker_pid) {
            ui::print_box(
                "WATCHER STOP REQUESTED",
                &[
                    ui::labeled("WORKER", format!("PID {}", state.worker_pid)),
                    ui::labeled("LOG", state.log_path.to_string_lossy()),
                ],
            );
        } else {
            let _ = fs::remove_file(state_path);
            ui::print_box(
                "WATCHER STOPPED",
                &[
                    ui::labeled("NAME", state.name),
                    ui::labeled("WORKER", format!("PID {}", state.worker_pid)),
                ],
            );
        }
        Ok(())
    }

    pub(crate) fn run_watch_worker(&self, options: WorkerOptions) -> Result<()> {
        let state_path = PathBuf::from(&options.state_path);
        let result = self.watch_worker_loop(&options, &state_path);
        remove_watch_state(&state_path, &options.token);
        result
    }

    fn watch_worker_loop(&self, options: &WorkerOptions, state_path: &Path) -> Result<()> {
        validate_name(&options.name)?;
        if options.token.len() != 32
            || !options
                .token
                .chars()
                .all(|character| character.is_ascii_hexdigit())
        {
            bail!("invalid watcher token");
        }
        if options.interval_seconds < MINIMUM_WATCH_INTERVAL {
            bail!("invalid watcher interval");
        }
        match (options.scope, options.window_id.as_ref()) {
            (WatchScope::CurrentWindow, None) => {
                bail!("current-window watcher is missing its bound window ID")
            }
            (WatchScope::AllWindows, Some(_)) => {
                bail!("all-windows watcher cannot have a bound window ID")
            }
            _ => {}
        }
        if state_path != self.watch_state_path(options.owner_pid) {
            bail!("invalid watcher state path");
        }
        let stop_requested = Arc::new(AtomicBool::new(false));
        for signal in [SIGHUP, SIGINT, SIGTERM] {
            signal_hook::flag::register(signal, Arc::clone(&stop_requested))?;
        }
        thread::sleep(StdDuration::from_millis(100));
        while !stop_requested.load(Ordering::Relaxed)
            && owner_alive(options.owner_pid, &options.owner_started_at)
        {
            update_watch_state(state_path, &options.token, |state| {
                state.last_attempt_at = Some(Local::now().to_rfc3339());
                state.last_error = None;
            })?;
            let mut save = Command::new(std::env::current_exe()?);
            save.args(["--save", &options.name, "--no-scrollback"]);
            if let Some(window_id) = &options.window_id {
                save.args(["--window-id", window_id]);
            }
            let success = save.status().is_ok_and(|status| status.success());
            let completed = Local::now();
            update_watch_state(state_path, &options.token, |state| {
                if success {
                    state.last_success_at = Some(completed.to_rfc3339());
                    state.last_snapshot_id = self.newest_snapshot_id(&options.name);
                    state.last_error = None;
                } else {
                    state.last_error = Some(format!(
                        "save failed at {}; see watcher log",
                        completed.to_rfc3339()
                    ));
                }
                state.next_attempt_at = Some(
                    (completed + Duration::seconds(options.interval_seconds as i64)).to_rfc3339(),
                );
            })?;
            if !wait_for_interval(
                options.interval_seconds,
                options.owner_pid,
                &options.owner_started_at,
                &stop_requested,
            ) {
                break;
            }
        }
        Ok(())
    }

    fn watch_state_path(&self, owner_pid: u32) -> PathBuf {
        self.watchers_dir.join(format!("shell-{owner_pid}.json"))
    }
}

fn read_watch_state(path: &Path) -> Option<WatchState> {
    serde_json::from_str(&fs::read_to_string(path).ok()?).ok()
}

fn write_watch_state(path: &Path, state: &WatchState) -> Result<()> {
    fs::create_dir_all(path.parent().context("watch state has no parent")?)?;
    let temporary = path.with_extension(format!("json.{}.tmp", std::process::id()));
    let result = (|| -> Result<()> {
        let mut json = serde_json::to_string_pretty(state)?;
        json.push('\n');
        fs::write(&temporary, json)?;
        fs::rename(&temporary, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}

fn update_watch_state(
    path: &Path,
    token: &str,
    update: impl FnOnce(&mut WatchState),
) -> Result<()> {
    let Some(mut state) = read_watch_state(path).filter(|state| state.token == token) else {
        return Ok(());
    };
    update(&mut state);
    write_watch_state(path, &state)
}

fn remove_watch_state(path: &Path, token: &str) {
    if read_watch_state(path).is_some_and(|state| state.token == token) {
        let _ = fs::remove_file(path);
    }
}

fn watcher_alive(state: &WatchState) -> bool {
    owner_alive(state.owner_pid, &state.owner_started_at)
        && process_alive(state.worker_pid)
        && process_command(state.worker_pid).contains(&state.token)
}

fn owner_alive(pid: u32, started_at: &str) -> bool {
    process_alive(pid) && process_start_marker(pid) == started_at
}

fn process_alive(pid: u32) -> bool {
    i32::try_from(pid)
        .ok()
        .is_some_and(|pid| kill(Pid::from_raw(pid), None).is_ok())
}

fn process_start_marker(pid: u32) -> String {
    command::try_run("ps", &["-p", &pid.to_string(), "-o", "lstart="]).unwrap_or_default()
}

fn process_command(pid: u32) -> String {
    command::try_run("ps", &["-ww", "-p", &pid.to_string(), "-o", "command="]).unwrap_or_default()
}

fn wait_for_interval(
    seconds: u64,
    owner_pid: u32,
    owner_started_at: &str,
    stop_requested: &AtomicBool,
) -> bool {
    let deadline = Instant::now() + StdDuration::from_secs(seconds);
    loop {
        if stop_requested.load(Ordering::Relaxed) || !owner_alive(owner_pid, owner_started_at) {
            return false;
        }
        let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
            return true;
        };
        thread::sleep(remaining.min(StdDuration::from_secs(5)));
    }
}

fn display_interval(seconds: u64) -> String {
    if seconds % 3_600 == 0 {
        format!("{}h", seconds / 3_600)
    } else if seconds % 60 == 0 {
        format!("{}m", seconds / 60)
    } else {
        format!("{seconds}s")
    }
}

fn display_time(value: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(value)
        .map(|time| time.format("%Y-%m-%d %H:%M:%S %:z").to_string())
        .unwrap_or_else(|_| value.to_owned())
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    fn state(root: &Path) -> WatchState {
        WatchState {
            version: 1,
            token: "a".repeat(32),
            name: "backup".into(),
            interval_seconds: 900,
            scope: "current-window".into(),
            window_id: Some("window".into()),
            owner_pid: 100,
            owner_started_at: "start".into(),
            worker_pid: 200,
            started_at: "2026-07-20T12:00:00+03:00".into(),
            log_path: root.join("watch.log"),
            last_attempt_at: None,
            last_success_at: None,
            last_snapshot_id: None,
            next_attempt_at: None,
            last_error: None,
        }
    }

    #[test]
    fn atomically_updates_matching_watcher_state() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("watch.json");
        let state = state(temp.path());
        write_watch_state(&path, &state).unwrap();
        update_watch_state(&path, &state.token, |state| {
            state.last_snapshot_id = Some("backup_1".into());
        })
        .unwrap();
        assert_eq!(
            read_watch_state(&path).unwrap().last_snapshot_id.as_deref(),
            Some("backup_1")
        );
    }

    #[test]
    fn displays_compact_intervals() {
        assert_eq!(display_interval(3_600), "1h");
        assert_eq!(display_interval(900), "15m");
        assert_eq!(display_interval(15), "15s");
    }
}
