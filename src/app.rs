use std::{
    collections::{BTreeMap, HashSet},
    env,
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    sync::LazyLock,
};

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Duration, Local};
use regex::Regex;

use crate::{
    agents,
    cli::{self, Command, RestoreOptions, SaveOptions},
    command, ghostty,
    model::{Agent, SNAPSHOT_VERSION, Scope, Snapshot},
    ui,
};

static NAME_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[a-zA-Z0-9][a-zA-Z0-9._-]*$").expect("valid snapshot name regex")
});

#[derive(Debug, Clone)]
pub struct App {
    pub(crate) home: PathBuf,
    pub(crate) snapshots_dir: PathBuf,
    pub(crate) watchers_dir: PathBuf,
    capture_script: PathBuf,
}

impl App {
    pub fn from_env() -> Result<Self> {
        let home = env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or_else(|| anyhow::anyhow!("HOME is not set"))?;
        let state_root = env::var_os("RZ_STATE_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".local/state/ghostty-rz"));
        let capture_script = capture_script_path()?;
        Ok(Self {
            snapshots_dir: state_root.join("snapshots"),
            watchers_dir: state_root.join("watchers"),
            home,
            capture_script,
        })
    }

    #[cfg(test)]
    pub(crate) fn for_test(root: &Path) -> Self {
        Self {
            home: root.join("home"),
            snapshots_dir: root.join("snapshots"),
            watchers_dir: root.join("watchers"),
            capture_script: root.join("snapshot-capture.applescript"),
        }
    }

    pub fn run(&self, args: impl IntoIterator<Item = OsString>) -> Result<()> {
        match cli::parse(args)? {
            Command::Save(options) => self.save(options),
            Command::Restore(options) => self.restore(options),
            Command::List(number) => self.list(number),
            Command::Clean { age_seconds } => self.clean(age_seconds),
            Command::StartWatch(options) => self.start_watch(options),
            Command::WatchStatus => self.watch_status(),
            Command::WatchStop => self.stop_watch(),
            Command::WatchWorker(options) => self.run_watch_worker(options),
            Command::Help => {
                println!("{}", cli::USAGE);
                Ok(())
            }
            Command::Version => {
                println!("rz {}", env!("CARGO_PKG_VERSION"));
                Ok(())
            }
        }
    }

    pub fn print_error(error: &anyhow::Error) {
        ui::print_error(error);
    }

    pub(crate) fn save(&self, options: SaveOptions) -> Result<()> {
        validate_name(&options.name)?;
        ghostty::ensure_running()?;
        let target_window_id = match (&options.window_id, options.current_window) {
            (Some(id), _) => Some(id.clone()),
            (None, true) => Some(ghostty::current_window_id()?),
            (None, false) => None,
        };
        let timestamp = Local::now().format("%Y%m%d-%H%M%S").to_string();
        let snapshot_id = format!("{}_{}", options.name, timestamp);
        fs::create_dir_all(&self.snapshots_dir)?;
        let final_dir = self.snapshots_dir.join(&snapshot_id);
        if final_dir.exists() {
            bail!("snapshot already exists: {snapshot_id}");
        }
        let staging_dir = self
            .snapshots_dir
            .join(format!(".{snapshot_id}.{}.tmp", std::process::id()));
        fs::create_dir_all(staging_dir.join("scrollback"))?;
        let result = self.capture_snapshot(
            &options,
            target_window_id.as_deref(),
            &timestamp,
            &snapshot_id,
            &staging_dir,
            &final_dir,
        );
        if result.is_err() && staging_dir.starts_with(&self.snapshots_dir) {
            let _ = fs::remove_dir_all(&staging_dir);
        }
        result
    }

    #[allow(clippy::too_many_arguments)]
    fn capture_snapshot(
        &self,
        options: &SaveOptions,
        target_window_id: Option<&str>,
        timestamp: &str,
        snapshot_id: &str,
        staging_dir: &Path,
        final_dir: &Path,
    ) -> Result<()> {
        let mut rows = ghostty::capture(
            &self.capture_script,
            options.capture_scrollback,
            target_window_id,
        )?;
        if rows.is_empty() {
            if let Some(window_id) = target_window_id {
                bail!("Ghostty window {window_id:?} is no longer available");
            }
            bail!("Ghostty has no restorable terminal surfaces");
        }
        let process_sessions = agents::running_sessions()?;
        let (assignments, mut warnings) = agents::assign_sessions(
            &rows,
            &process_sessions,
            &self.snapshots_dir,
            target_window_id.is_none(),
        );
        let windows_with_geometry = rows.iter().fold(BTreeMap::new(), |mut windows, row| {
            windows
                .entry(row.window_index)
                .or_insert(row.has_geometry());
            windows
        });
        if windows_with_geometry
            .values()
            .any(|has_geometry| !has_geometry)
        {
            warnings.push(
                "window geometry unavailable; grant your shell Accessibility permission".into(),
            );
        }
        for row in &mut rows {
            if let Some(session) = assignments.get(&row.terminal_id) {
                row.agent_session = Some(session.clone());
                if row.working_directory.as_os_str().is_empty() {
                    row.working_directory = session.cwd.clone();
                }
            }
            let Some(source) = row.scrollback_path.as_deref().filter(|path| path.is_file()) else {
                continue;
            };
            let relative = format!(
                "scrollback/window-{:02}-tab-{:02}-terminal-{:02}.txt",
                row.window_index, row.tab_index, row.terminal_index
            );
            fs::copy(source, staging_dir.join(&relative))?;
            row.scrollback_file = Some(relative);
        }
        let mut detected_sessions = if target_window_id.is_some() {
            assignments.into_values().collect::<Vec<_>>()
        } else {
            process_sessions
        };
        detected_sessions.sort_by_key(|session| (session.agent, session.tty.clone()));
        detected_sessions.dedup_by_key(|session| session.key());
        let snapshot = ghostty::build_snapshot(ghostty::SnapshotInput {
            name: &options.name,
            id: snapshot_id,
            timestamp,
            rows: &rows,
            process_sessions: &detected_sessions,
            warnings: warnings.clone(),
            capture_scrollback: options.capture_scrollback,
            target_window_id,
            ghostty_version: ghostty::version()?,
        });
        let mut json = serde_json::to_string_pretty(&snapshot)?;
        json.push('\n');
        fs::write(staging_dir.join("state.json"), json)?;
        fs::rename(staging_dir, final_dir)?;

        let mut lines = vec![
            ui::labeled("SNAPSHOT", snapshot_id),
            ui::labeled(
                "SCOPE",
                target_window_id.map_or_else(
                    || "all Ghostty windows".into(),
                    |id| format!("Ghostty window {id}"),
                ),
            ),
            ui::labeled("LAYOUT", layout_summary(&snapshot)),
        ];
        lines.extend(saved_tab_lines(&snapshot));
        let terminals = snapshot
            .windows
            .iter()
            .flat_map(|window| &window.tabs)
            .flat_map(|tab| &tab.terminals)
            .collect::<Vec<_>>();
        lines.push(ui::labeled(
            "CONTENT",
            content_summary(
                terminals
                    .iter()
                    .filter(|terminal| terminal.codex_session_id.is_some())
                    .count(),
                terminals
                    .iter()
                    .filter(|terminal| terminal.amp_thread_id.is_some())
                    .count(),
                terminals
                    .iter()
                    .filter(|terminal| terminal.scrollback_file.is_some())
                    .count(),
                options.capture_scrollback,
            ),
        ));
        lines.push(ui::labeled("LOCATION", final_dir.to_string_lossy()));
        lines.extend(
            warnings
                .iter()
                .map(|warning| ui::labeled("WARNING", warning)),
        );
        ui::print_box("WORKSPACE SAVED", &lines);
        Ok(())
    }

    fn restore(&self, options: RestoreOptions) -> Result<()> {
        let snapshot_dir = self.resolve_snapshot(options.selector.as_deref())?;
        let snapshot = read_snapshot(&snapshot_dir)?;
        validate_snapshot(&snapshot)?;
        if options.dry_run {
            print_restore_preview(&snapshot, &snapshot_dir, options.close_existing);
            return Ok(());
        }
        let ready_file = if options.close_existing {
            Some(PathBuf::from(format!(
                "/tmp/ghostty-rz-{}-{}.ready",
                std::process::id(),
                random_hex(6)?
            )))
        } else {
            None
        };
        let running_sessions = if options.close_existing {
            HashSet::new()
        } else {
            agents::running_sessions()?
                .into_iter()
                .map(|session| session.key())
                .collect()
        };
        let plan = ghostty::restore_script(
            &snapshot,
            &snapshot_dir,
            &running_sessions,
            &self.home,
            options.close_existing,
            ready_file.as_deref(),
        )?;
        if env::var("RZ_PRINT_APPLESCRIPT").as_deref() == Ok("1") {
            println!("{}", plan.script);
            return Ok(());
        }
        let output = command::run("osascript", &["-e", &plan.script])?;
        let mut lines = vec![
            ui::labeled("SNAPSHOT", &snapshot.id),
            ui::labeled("LAYOUT", layout_summary(&snapshot)),
            ui::labeled(
                "STATUS",
                if options.close_existing {
                    "Restored; previous Ghostty window(s) scheduled to close"
                } else {
                    "Restored into new Ghostty window(s); existing windows kept"
                },
            ),
        ];
        for duplicate in plan.duplicates {
            lines.push(ui::labeled(
                "WARNING",
                format!(
                    "{} {} already running; opened a shell: {}",
                    duplicate.agent.label(),
                    duplicate.agent.unit(),
                    duplicate.id
                ),
            ));
        }
        if !output.is_empty() {
            lines.push(ui::labeled("GHOSTTY", output));
        }
        ui::print_box("WORKSPACE RESTORED", &lines);
        Ok(())
    }

    fn list(&self, number: Option<usize>) -> Result<()> {
        let snapshots = self.snapshot_directories();
        if snapshots.is_empty() {
            ui::print_box(
                "SAVED WORKSPACES",
                &[
                    "No snapshots found.".into(),
                    ui::labeled("LOCATION", self.snapshots_dir.to_string_lossy()),
                ],
            );
            return Ok(());
        }
        if let Some(number) = number {
            let Some(directory) = snapshots.iter().rev().nth(number - 1) else {
                bail!(
                    "snapshot number {number} is out of range; choose 1-{} from rz --list",
                    snapshots.len()
                );
            };
            return print_snapshot_details(number, directory);
        }
        let mut lines = Vec::new();
        for (index, directory) in snapshots.iter().rev().enumerate() {
            let snapshot = read_snapshot(directory)?;
            lines.push(format!("{:02}  {}", index + 1, snapshot.id));
            lines.push(format!(
                "    {}  |  {}",
                display_time(&snapshot.saved_at),
                layout_summary(&snapshot)
            ));
        }
        lines.push(String::new());
        lines.push(ui::labeled(
            "LOCATION",
            self.snapshots_dir.to_string_lossy(),
        ));
        ui::print_box("SAVED WORKSPACES", &lines);
        Ok(())
    }

    fn clean(&self, age_seconds: u64) -> Result<()> {
        let cutoff = Local::now() - Duration::seconds(i64::try_from(age_seconds)?);
        let mut removed = Vec::new();
        let mut kept = 0;
        let mut warnings = Vec::new();
        for directory in self.snapshot_directories() {
            let result = (|| -> Result<()> {
                let snapshot = read_snapshot(&directory)?;
                let saved_at = DateTime::parse_from_rfc3339(&snapshot.saved_at)?;
                if saved_at < cutoff {
                    self.remove_snapshot_directory(&directory)?;
                    removed.push((snapshot.id, saved_at));
                } else {
                    kept += 1;
                }
                Ok(())
            })();
            if let Err(error) = result {
                warnings.push(format!(
                    "kept {}: {error}",
                    directory.file_name().unwrap_or_default().to_string_lossy()
                ));
            }
        }
        let mut lines = vec![
            ui::labeled("OLDER THAN", human_duration(age_seconds)),
            ui::labeled("CUTOFF", cutoff.to_rfc3339()),
            ui::labeled("REMOVED", format!("{} snapshot(s)", removed.len())),
            ui::labeled("KEPT", format!("{kept} newer snapshot(s)")),
            ui::labeled("LOCATION", self.snapshots_dir.to_string_lossy()),
        ];
        lines.extend(removed.into_iter().map(|(id, saved_at)| {
            ui::labeled(
                "PURGED",
                format!("{id}  |  {}", display_time(&saved_at.to_rfc3339())),
            )
        }));
        lines.extend(
            warnings
                .iter()
                .map(|warning| ui::labeled("WARNING", warning)),
        );
        ui::print_box("SNAPSHOTS CLEANED", &lines);
        Ok(())
    }

    pub(crate) fn snapshot_directories(&self) -> Vec<PathBuf> {
        let Ok(entries) = fs::read_dir(&self.snapshots_dir) else {
            return Vec::new();
        };
        let mut directories = entries
            .flatten()
            .filter(|entry| !entry.file_name().to_string_lossy().starts_with('.'))
            .map(|entry| entry.path())
            .filter(|directory| directory.is_dir() && directory.join("state.json").is_file())
            .collect::<Vec<_>>();
        directories.sort_by_key(|directory| {
            read_snapshot(directory)
                .map(|snapshot| (snapshot.saved_at, snapshot.id))
                .unwrap_or_else(|_| {
                    (
                        String::new(),
                        directory
                            .file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .into_owned(),
                    )
                })
        });
        directories
    }

    fn remove_snapshot_directory(&self, directory: &Path) -> Result<()> {
        let parent = directory.parent();
        let metadata = fs::symlink_metadata(directory)?;
        if parent != Some(self.snapshots_dir.as_path()) || metadata.file_type().is_symlink() {
            bail!("refusing to remove unsafe snapshot path {directory:?}");
        }
        fs::remove_dir_all(directory)?;
        Ok(())
    }

    fn resolve_snapshot(&self, selector: Option<&str>) -> Result<PathBuf> {
        let snapshots = self.snapshot_directories();
        if snapshots.is_empty() {
            bail!("no saved sessions; create one with rz --save NAME");
        }
        let Some(selector) = selector else {
            return Ok(snapshots.last().expect("non-empty snapshots").clone());
        };
        if let Some(exact) = snapshots
            .iter()
            .find(|directory| directory.file_name().is_some_and(|name| name == selector))
        {
            return Ok(exact.clone());
        }
        let matching = snapshots.iter().rfind(|directory| {
            read_snapshot(directory)
                .is_ok_and(|snapshot| snapshot.name == selector || snapshot.timestamp == selector)
        });
        matching
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("no saved session matches {selector:?}; run rz --list"))
    }

    pub(crate) fn newest_snapshot_id(&self, name: &str) -> Option<String> {
        self.snapshot_directories()
            .iter()
            .rev()
            .find_map(|directory| {
                read_snapshot(directory)
                    .ok()
                    .filter(|snapshot| snapshot.name == name)
                    .map(|snapshot| snapshot.id)
            })
    }
}

fn capture_script_path() -> Result<PathBuf> {
    if let Some(directory) = env::var_os("RZ_SCRIPT_DIR") {
        return Ok(PathBuf::from(directory).join("snapshot-capture.applescript"));
    }
    let executable = env::current_exe()?.canonicalize()?;
    if let Some(prefix) = executable.parent().and_then(Path::parent) {
        let installed = prefix.join("libexec/snapshot-capture.applescript");
        if installed.is_file() {
            return Ok(installed);
        }
    }
    Ok(Path::new(env!("CARGO_MANIFEST_DIR")).join("libexec/snapshot-capture.applescript"))
}

fn read_snapshot(directory: &Path) -> Result<Snapshot> {
    let content = fs::read_to_string(directory.join("state.json"))?;
    serde_json::from_str(&content).context("invalid snapshot state")
}

fn validate_snapshot(snapshot: &Snapshot) -> Result<()> {
    if snapshot.version != SNAPSHOT_VERSION {
        bail!("unsupported snapshot version {}", snapshot.version);
    }
    if snapshot.windows.is_empty() {
        bail!("snapshot has no windows");
    }
    Ok(())
}

fn print_snapshot_details(number: usize, directory: &Path) -> Result<()> {
    let snapshot = read_snapshot(directory)?;
    validate_snapshot(&snapshot)?;
    let terminals = snapshot
        .windows
        .iter()
        .flat_map(|window| &window.tabs)
        .flat_map(|tab| &tab.terminals)
        .collect::<Vec<_>>();
    let scope = match &snapshot.scope {
        Scope::Window { window_id } => format!("Ghostty window {window_id}"),
        Scope::AllWindows => "all Ghostty windows".into(),
    };
    let mut lines = vec![
        ui::labeled("NUMBER", format!("{number:02}")),
        ui::labeled("SNAPSHOT", &snapshot.id),
        ui::labeled(
            "SAVED",
            format!(
                "{}  |  Ghostty {}",
                display_time(&snapshot.saved_at),
                snapshot.ghostty_version
            ),
        ),
        ui::labeled("SCOPE", scope),
        ui::labeled("LAYOUT", layout_summary(&snapshot)),
        ui::labeled(
            "CONTENT",
            content_summary(
                terminals
                    .iter()
                    .filter(|terminal| terminal.codex_session_id.is_some())
                    .count(),
                terminals
                    .iter()
                    .filter(|terminal| terminal.amp_thread_id.is_some())
                    .count(),
                terminals
                    .iter()
                    .filter(|terminal| terminal.scrollback_file.is_some())
                    .count(),
                snapshot.scrollback_captured,
            ),
        ),
        String::new(),
    ];
    for (window_offset, window) in snapshot.windows.iter().enumerate() {
        if window_offset > 0 {
            lines.push(String::new());
        }
        let name = untitled(&window.name);
        let mut summary = format!("{}  {name}", window.index);
        if let Some(size) = window.size {
            summary.push_str(&format!("  |  {}x{}", size.width, size.height));
        }
        if let Some(position) = window.position {
            summary.push_str(&format!(" at {},{}", position.x, position.y));
        }
        lines.push(ui::labeled("WINDOW", summary));
        for tab in &window.tabs {
            let mut summary = format!(
                "{}.{}  {}  |  {} terminal(s)",
                window.index,
                tab.index,
                untitled(&tab.name),
                tab.terminals.len()
            );
            if tab.selected {
                summary.push_str("  |  selected");
            }
            lines.push(ui::labeled("TAB", summary));
            for terminal in &tab.terminals {
                let mut attributes = Vec::new();
                if terminal.focused {
                    attributes.push("focused");
                }
                if terminal.scrollback_file.is_some() {
                    attributes.push("scrollback");
                }
                let mut summary = format!(
                    "{}.{}.{}  {}",
                    window.index,
                    tab.index,
                    terminal.index,
                    untitled(&terminal.name)
                );
                if !attributes.is_empty() {
                    summary.push_str(&format!("  |  {}", attributes.join(", ")));
                }
                lines.push(ui::labeled("TERM", summary));
                if !terminal.working_directory.is_empty() {
                    lines.push(ui::labeled("CWD", &terminal.working_directory));
                }
                if let Some(id) = &terminal.codex_session_id {
                    lines.push(ui::labeled("CODEX", id));
                }
                if let Some(id) = &terminal.amp_thread_id {
                    lines.push(ui::labeled("AMP", id));
                }
            }
        }
    }
    lines.extend(
        snapshot
            .warnings
            .iter()
            .map(|warning| ui::labeled("WARNING", warning)),
    );
    lines.push(String::new());
    lines.push(ui::labeled("LOCATION", directory.to_string_lossy()));
    ui::print_box("WORKSPACE DETAILS", &lines);
    Ok(())
}

fn print_restore_preview(snapshot: &Snapshot, directory: &Path, close_existing: bool) {
    let sessions = snapshot
        .windows
        .iter()
        .flat_map(|window| &window.tabs)
        .flat_map(|tab| &tab.terminals)
        .filter_map(|terminal| terminal.session())
        .collect::<Vec<_>>();
    let codex = sessions
        .iter()
        .filter(|session| session.agent == Agent::Codex)
        .collect::<Vec<_>>();
    let amp = sessions
        .iter()
        .filter(|session| session.agent == Agent::Amp)
        .collect::<Vec<_>>();
    let mut lines = vec![
        ui::labeled("STATUS", "Dry run - Ghostty will not be changed"),
        ui::labeled("SNAPSHOT", &snapshot.id),
        ui::labeled(
            "SAVED",
            format!(
                "{}  |  Ghostty {}",
                display_time(&snapshot.saved_at),
                snapshot.ghostty_version
            ),
        ),
        ui::labeled("LAYOUT", layout_summary(snapshot)),
        ui::labeled(
            "WINDOWS",
            if close_existing {
                "Existing Ghostty windows will close after restore"
            } else {
                "Existing Ghostty windows will remain open"
            },
        ),
        ui::labeled(
            "CODEX",
            if codex.is_empty() {
                "none".into()
            } else {
                format!("{} session(s)", codex.len())
            },
        ),
    ];
    lines.extend(
        codex
            .iter()
            .map(|session| format!("              {}", session.id)),
    );
    lines.push(ui::labeled(
        "AMP",
        if amp.is_empty() {
            "none".into()
        } else {
            format!("{} thread(s)", amp.len())
        },
    ));
    lines.extend(
        amp.iter()
            .map(|session| format!("              {}", session.id)),
    );
    lines.push(ui::labeled("LOCATION", directory.to_string_lossy()));
    lines.extend(
        snapshot
            .limitations
            .iter()
            .map(|note| ui::labeled("NOTE", note)),
    );
    ui::print_box("RESTORE PREVIEW", &lines);
}

fn layout_summary(snapshot: &Snapshot) -> String {
    format!(
        "{} window(s)  |  {} tab(s)  |  {} terminal(s)",
        snapshot.windows.len(),
        snapshot.tabs_count,
        snapshot
            .windows
            .iter()
            .flat_map(|window| &window.tabs)
            .map(|tab| tab.terminals.len())
            .sum::<usize>()
    )
}

fn saved_tab_lines(snapshot: &Snapshot) -> Vec<String> {
    snapshot
        .windows
        .iter()
        .flat_map(|window| {
            window.tabs.iter().map(|tab| {
                ui::labeled(
                    &format!("TAB {}.{}", window.index, tab.index),
                    untitled(&tab.name),
                )
            })
        })
        .collect()
}

fn content_summary(codex: usize, amp: usize, scrollback: usize, captured: bool) -> String {
    let scrollback = if captured {
        format!("{scrollback} scrollback file(s)")
    } else {
        "scrollback skipped".into()
    };
    format!("{codex} Codex session(s)  |  {amp} Amp thread(s)  |  {scrollback}")
}

fn display_time(value: &str) -> String {
    DateTime::parse_from_rfc3339(value)
        .map(|time| time.format("%Y-%m-%d %H:%M:%S %:z").to_string())
        .unwrap_or_else(|_| value.to_owned())
}

fn human_duration(seconds: u64) -> String {
    for (unit, name) in [(86_400, "day"), (3_600, "hour"), (60, "minute")] {
        if seconds >= unit && seconds % unit == 0 {
            let count = seconds / unit;
            return format!("{count} {name}{}", if count == 1 { "" } else { "s" });
        }
    }
    format!("{seconds} second{}", if seconds == 1 { "" } else { "s" })
}

fn untitled(value: &str) -> &str {
    if value.trim().is_empty() {
        "(untitled)"
    } else {
        value.trim()
    }
}

pub(crate) fn validate_name(name: &str) -> Result<()> {
    if !NAME_PATTERN.is_match(name) {
        bail!("invalid snapshot name {name:?}; use letters, numbers, dot, underscore, or dash");
    }
    Ok(())
}

pub(crate) fn random_hex(bytes: usize) -> Result<String> {
    let mut random = vec![0_u8; bytes];
    getrandom::fill(&mut random)
        .map_err(|error| anyhow::anyhow!("failed to obtain secure randomness: {error}"))?;
    Ok(random.iter().map(|byte| format!("{byte:02x}")).collect())
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::symlink;

    use tempfile::TempDir;

    use super::*;

    fn write_snapshot(directory: &Path, id: &str, saved_at: &str) {
        fs::create_dir_all(directory).unwrap();
        fs::write(
            directory.join("state.json"),
            format!(
                r#"{{"version":1,"name":"work","id":"{id}","saved_at":"{saved_at}","timestamp":"time","ghostty_version":"1.3.1","tabs_count":1,"terminals_count":1,"scrollback_captured":false,"scope":{{"type":"all_windows"}},"windows":[{{"index":1,"name":"Work","tabs":[{{"index":1,"name":"Project","selected":true,"terminals":[{{"index":1,"name":"shell","focused":true,"working_directory":"/tmp/project"}}]}}]}}],"detected_codex_sessions":[{{"pid":42,"tty":"ttys001","cwd":"/tmp/project","session_id":"019f7eb7-dc72-75b3-b042-91599cdd90ac","command":"codex"}}]}}"#
            ),
        )
        .unwrap();
    }

    #[test]
    fn reads_version_one_ruby_snapshots() {
        let temp = TempDir::new().unwrap();
        write_snapshot(temp.path(), "work_1", "2026-07-20T12:00:00+03:00");
        let snapshot = read_snapshot(temp.path()).unwrap();
        validate_snapshot(&snapshot).unwrap();
        assert_eq!(snapshot.id, "work_1");
        assert_eq!(
            snapshot.windows[0].tabs[0].terminals[0].working_directory,
            "/tmp/project"
        );
        assert_eq!(snapshot.detected_codex_sessions[0].agent, Agent::Codex);
    }

    #[test]
    fn sorts_snapshot_directories_by_saved_time() {
        let temp = TempDir::new().unwrap();
        let app = App::for_test(temp.path());
        write_snapshot(
            &app.snapshots_dir.join("new"),
            "new",
            "2026-07-21T12:00:00+03:00",
        );
        write_snapshot(
            &app.snapshots_dir.join("old"),
            "old",
            "2026-07-20T12:00:00+03:00",
        );
        let directories = app.snapshot_directories();
        assert_eq!(directories[0].file_name().unwrap(), "old");
        assert_eq!(directories[1].file_name().unwrap(), "new");
    }

    #[test]
    fn refuses_to_remove_symlinked_snapshot() {
        let temp = TempDir::new().unwrap();
        let external = TempDir::new().unwrap();
        let app = App::for_test(temp.path());
        fs::create_dir_all(&app.snapshots_dir).unwrap();
        write_snapshot(external.path(), "external", "2026-07-01T12:00:00+03:00");
        let link = app.snapshots_dir.join("external");
        symlink(external.path(), &link).unwrap();
        let error = app.remove_snapshot_directory(&link).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("refusing to remove unsafe snapshot")
        );
    }

    #[test]
    fn formats_human_durations() {
        assert_eq!(human_duration(604_800), "7 days");
        assert_eq!(human_duration(3_600), "1 hour");
        assert_eq!(human_duration(12), "12 seconds");
    }
}
