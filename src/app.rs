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
use tempfile::Builder;

use crate::{
    agents,
    cli::{self, Command, RestoreOptions, SaveOptions},
    command, ghostty,
    model::{Agent, SNAPSHOT_VERSION, Scope, Snapshot},
    pi, storage,
    store::{SnapshotEntry, SnapshotStore},
    ui::{self, display_time},
};

static NAME_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[a-zA-Z0-9][a-zA-Z0-9._-]*$").expect("valid snapshot name regex")
});

#[derive(Debug, Clone)]
pub struct App {
    pub(crate) home: PathBuf,
    pub(crate) snapshots: SnapshotStore,
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
            snapshots: SnapshotStore {
                root: state_root.join("snapshots"),
            },
            watchers_dir: state_root.join("watchers"),
            home,
            capture_script,
        })
    }

    #[cfg(test)]
    pub(crate) fn for_test(root: &Path) -> Self {
        Self {
            home: root.join("home"),
            snapshots: SnapshotStore {
                root: root.join("snapshots"),
            },
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
            Command::ReplayScrollback(path) => {
                storage::replay_scrollback(&path, &mut std::io::stdout().lock())?;
                Ok(())
            }
            Command::InstallPiExtension => {
                let path = pi::install(&self.home)?;
                ui::print_box(
                    "PI SESSION SUPPORT",
                    &[
                        ui::labeled("INSTALLED", path.to_string_lossy()),
                        ui::labeled(
                            "NEXT",
                            "Run /reload in existing Pi terminals, or start Pi again.",
                        ),
                    ],
                );
                Ok(())
            }
            Command::Print(output) => {
                print!("{output}");
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
        fs::create_dir_all(&self.snapshots.root)?;
        let final_dir = self.snapshots.root.join(&snapshot_id);
        if final_dir.exists() {
            bail!("snapshot already exists: {snapshot_id}");
        }
        let staging = Builder::new()
            .prefix(".rz-")
            .tempdir_in(&self.snapshots.root)?;
        fs::create_dir(staging.path().join("scrollback"))?;
        self.capture_snapshot(
            &options,
            target_window_id.as_deref(),
            &timestamp,
            &snapshot_id,
            staging.path(),
            &final_dir,
        )
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
        let warning_ttys = target_window_id.map(|_| {
            rows.iter()
                .map(|row| row.tty.trim_start_matches("/dev/").to_owned())
                .collect::<HashSet<_>>()
        });
        let (process_sessions, discovery_warnings) =
            agents::running_sessions(&self.home, warning_ttys.as_ref())?;
        let (assignments, mut warnings) = agents::assign_sessions(
            &rows,
            &process_sessions,
            &self.snapshots.root,
            target_window_id.is_none(),
        );
        warnings.extend(discovery_warnings);
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
        let mut scrollback_bytes = 0;
        let mut scrollback_stored_bytes = 0;
        for row in &mut rows {
            if let Some(session) = assignments.get(&row.terminal_id) {
                row.agent_session = Some(session.clone());
                if session.agent == Agent::Pi || row.working_directory.as_os_str().is_empty() {
                    row.working_directory = session.cwd.clone();
                }
            }
            let Some(source) = row.scrollback_path.as_deref().filter(|path| path.is_file()) else {
                continue;
            };
            if source.metadata()?.len() == 0 {
                continue;
            }
            let relative = format!(
                "scrollback/window-{:02}-tab-{:02}-terminal-{:02}.txt.lz4",
                row.window_index, row.tab_index, row.terminal_index
            );
            let destination = staging_dir.join(&relative);
            scrollback_bytes += storage::compress_scrollback(source, &destination)?;
            scrollback_stored_bytes += destination.metadata()?.len();
            row.scrollback_file = Some(relative);
        }
        let mut detected_sessions = if target_window_id.is_some() {
            assignments.into_values().collect::<Vec<_>>()
        } else {
            process_sessions
        };
        detected_sessions.sort_by_key(|session| (session.agent, session.tty.clone()));
        detected_sessions.dedup_by_key(|session| session.key());
        let mut snapshot = ghostty::build_snapshot(ghostty::SnapshotInput {
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
        if options.capture_scrollback {
            snapshot.scrollback_bytes = Some(scrollback_bytes);
            snapshot.scrollback_stored_bytes = Some(scrollback_stored_bytes);
        }
        storage::write_json(&staging_dir.join("state.json"), &snapshot)?;
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
        lines.extend(content_lines(&snapshot));
        lines.push(ui::labeled(
            "SIZE",
            format!(
                "{} on disk",
                storage::format_bytes(storage::usage(final_dir)?.allocated)
            ),
        ));
        lines.push(ui::labeled(
            "STORAGE",
            self.storage_summary(self.snapshots.entries()?.len())?,
        ));
        lines.push(String::new());
        lines.extend(saved_tab_lines(&snapshot));
        lines.push(String::new());
        lines.push(ui::labeled("LOCATION", final_dir.to_string_lossy()));
        lines.extend(
            snapshot
                .warnings
                .iter()
                .map(|warning| ui::labeled("WARNING", warning)),
        );
        ui::print_box("WORKSPACE SAVED", &lines);
        Ok(())
    }

    fn restore(&self, options: RestoreOptions) -> Result<()> {
        let entry = self.snapshots.resolve(options.selector.as_deref())?;
        let snapshot_dir = &entry.path;
        let snapshot = entry.snapshot()?;
        validate_snapshot(snapshot)?;
        if options.dry_run {
            print_restore_preview(snapshot, snapshot_dir, options.close_existing);
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
            agents::running_sessions(&self.home, None)?
                .0
                .into_iter()
                .map(|session| session.key())
                .collect()
        };
        let plan = ghostty::restore_script(
            snapshot,
            snapshot_dir,
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
            ui::labeled("LAYOUT", layout_summary(snapshot)),
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
            lines.push(ui::labeled("WARNING", output));
        }
        ui::print_box("WORKSPACE RESTORED", &lines);
        Ok(())
    }

    fn list(&self, number: Option<usize>) -> Result<()> {
        let snapshots = self.snapshots.entries()?;
        if snapshots.is_empty() {
            ui::print_box(
                "SAVED WORKSPACES",
                &[
                    "No snapshots found.".into(),
                    ui::labeled("LOCATION", self.snapshots.root.to_string_lossy()),
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
        let mut lines = vec![
            ui::labeled("STORAGE", self.storage_summary(snapshots.len())?),
            String::new(),
        ];
        for (index, directory) in snapshots.iter().rev().enumerate() {
            let snapshot = directory.snapshot()?;
            lines.push(format!("{:02}  {}", index + 1, snapshot.id));
            lines.push(format!(
                "    {}  |  {}  |  {}",
                display_time(&snapshot.saved_at),
                layout_summary(snapshot),
                storage::format_bytes(storage::usage(&directory.path)?.allocated),
            ));
            lines.push(format!("    {}", session_summary(snapshot)));
            if !snapshot.warnings.is_empty() {
                lines.push(format!(
                    "    {} warning(s); inspect with rz --list {}",
                    snapshot.warnings.len(),
                    index + 1
                ));
            }
            lines.push(String::new());
        }
        lines.push(String::new());
        lines.push(ui::labeled(
            "LOCATION",
            self.snapshots.root.to_string_lossy(),
        ));
        ui::print_box("SAVED WORKSPACES", &lines);
        Ok(())
    }

    fn storage_summary(&self, count: usize) -> Result<String> {
        Ok(format!(
            "{} | {} on disk",
            counted(count, "snapshot"),
            storage::format_bytes(self.snapshots.usage()?.allocated)
        ))
    }

    fn clean(&self, age_seconds: u64) -> Result<()> {
        let age = Duration::try_seconds(i64::try_from(age_seconds)?)
            .context("cleanup age is too large")?;
        let cutoff = Local::now()
            .checked_sub_signed(age)
            .context("cleanup cutoff is out of range")?;
        let mut removed = Vec::new();
        let mut kept = 0;
        let mut warnings = Vec::new();
        for entry in self.snapshots.entries()? {
            let result = (|| -> Result<()> {
                let snapshot = entry.snapshot()?;
                let saved_at = DateTime::parse_from_rfc3339(&snapshot.saved_at)?;
                if saved_at < cutoff {
                    self.snapshots.remove(&entry.path)?;
                    removed.push((snapshot.id.clone(), saved_at));
                } else {
                    kept += 1;
                }
                Ok(())
            })();
            if let Err(error) = result {
                warnings.push(format!("kept {}: {error}", entry.id()));
            }
        }
        let mut lines = vec![
            ui::labeled("OLDER THAN", human_duration(age_seconds)),
            ui::labeled("CUTOFF", cutoff.to_rfc3339()),
            ui::labeled("REMOVED", format!("{} snapshot(s)", removed.len())),
            ui::labeled("KEPT", format!("{kept} newer snapshot(s)")),
            ui::labeled("LOCATION", self.snapshots.root.to_string_lossy()),
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

fn validate_snapshot(snapshot: &Snapshot) -> Result<()> {
    if snapshot.version != SNAPSHOT_VERSION {
        bail!("unsupported snapshot version {}", snapshot.version);
    }
    if snapshot.windows.is_empty() {
        bail!("snapshot has no windows");
    }
    Ok(())
}

fn print_snapshot_details(number: usize, entry: &SnapshotEntry) -> Result<()> {
    let snapshot = entry.snapshot()?;
    let directory = &entry.path;
    validate_snapshot(snapshot)?;
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
        ui::labeled("LAYOUT", layout_summary(snapshot)),
    ];
    lines.extend(content_lines(snapshot));
    lines.push(ui::labeled(
        "SIZE",
        format!(
            "{} on disk",
            storage::format_bytes(storage::usage(directory)?.allocated)
        ),
    ));
    if !snapshot.rz_version.is_empty() {
        lines.push(ui::labeled(
            "SAVED BY",
            format!("rz {}", snapshot.rz_version),
        ));
    }
    lines.push(String::new());
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
                "{}.{}  {}  |  {}",
                window.index,
                tab.index,
                untitled(&tab.name),
                counted(tab.terminals.len(), "pane")
            );
            if let Some(layout) = &tab.layout {
                summary.push_str(&format!(" | {}", layout.description()));
            }
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
                if let Some(frame) = terminal.frame {
                    lines.push(ui::labeled(
                        "BOUNDS",
                        format!(
                            "{}x{} at {},{}",
                            frame.width, frame.height, frame.x, frame.y
                        ),
                    ));
                }
                if let Some(session) = terminal.session() {
                    lines.push(ui::labeled(
                        &session.agent.label().to_ascii_uppercase(),
                        session.id,
                    ));
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
    ];
    for agent in Agent::ALL {
        let sessions = snapshot
            .sessions()
            .filter(|session| session.agent == agent)
            .collect::<Vec<_>>();
        let count = if sessions.is_empty() {
            "none".into()
        } else {
            counted(sessions.len(), agent.unit())
        };
        lines.push(ui::labeled(&agent.label().to_ascii_uppercase(), count));
        lines.extend(sessions.iter().map(|session| ui::labeled("", session.id)));
    }
    lines.push(ui::labeled("LOCATION", directory.to_string_lossy()));
    lines.extend(
        snapshot
            .warnings
            .iter()
            .map(|warning| ui::labeled("WARNING", warning)),
    );
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
        "{} | {} | {}",
        counted(snapshot.windows.len(), "window"),
        counted(snapshot.tabs().count(), "tab"),
        counted(snapshot.terminals().count(), "pane")
    )
}

fn saved_tab_lines(snapshot: &Snapshot) -> Vec<String> {
    let mut lines = Vec::new();
    for window in &snapshot.windows {
        for tab in &window.tabs {
            lines.push(ui::labeled(
                &format!("TAB {}.{}", window.index, tab.index),
                untitled(&tab.name),
            ));
            let agents = Agent::ALL
                .into_iter()
                .filter_map(|agent| {
                    let count = tab
                        .terminals
                        .iter()
                        .filter(|terminal| {
                            terminal
                                .session()
                                .is_some_and(|session| session.agent == agent)
                        })
                        .count();
                    (count > 0).then(|| format!("{} {count}", agent.label()))
                })
                .collect::<Vec<_>>();
            let sessions = if agents.is_empty() {
                "no saved agent sessions".into()
            } else {
                agents.join(", ")
            };
            let layout = tab
                .layout
                .as_ref()
                .map(|layout| format!(" | {}", layout.description()))
                .unwrap_or_default();
            lines.push(ui::labeled(
                "",
                format!(
                    "{}{layout} | {sessions}",
                    counted(tab.terminals.len(), "pane")
                ),
            ));
        }
    }
    lines
}

fn counted(count: usize, noun: &str) -> String {
    format!("{count} {noun}{}", if count == 1 { "" } else { "s" })
}

fn session_summary(snapshot: &Snapshot) -> String {
    Agent::ALL
        .into_iter()
        .map(|agent| {
            let count = snapshot
                .terminals()
                .filter(|terminal| {
                    terminal
                        .session()
                        .is_some_and(|session| session.agent == agent)
                })
                .count();
            format!("{} {count}", agent.label())
        })
        .collect::<Vec<_>>()
        .join(" | ")
}

fn content_lines(snapshot: &Snapshot) -> Vec<String> {
    let files = snapshot
        .terminals()
        .filter(|terminal| terminal.scrollback_file.is_some())
        .count();
    let scrollback = if !snapshot.scrollback_captured {
        "skipped".into()
    } else if files == 0 {
        "none".into()
    } else if let (Some(original), Some(stored)) =
        (snapshot.scrollback_bytes, snapshot.scrollback_stored_bytes)
    {
        let saving = if original == 0 {
            0.0
        } else {
            100.0 * (1.0 - stored as f64 / original as f64)
        };
        let change = if saving >= 0.0 {
            format!("{saving:.1}% smaller")
        } else {
            format!("{:.1}% larger", -saving)
        };
        format!(
            "{} | LZ4 | {} -> {} ({change})",
            counted(files, "file"),
            storage::format_bytes(original),
            storage::format_bytes(stored)
        )
    } else {
        format!("{} | plain text", counted(files, "file"))
    };
    vec![
        ui::labeled("SESSIONS", session_summary(snapshot)),
        ui::labeled("SCROLLBACK", scrollback),
    ]
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
    fn compact_summary_shows_all_agents_and_precise_compression_savings() {
        let temp = TempDir::new().unwrap();
        write_snapshot(temp.path(), "work_1", "2026-11-22T13:00:00Z");
        let mut snapshot = SnapshotStore::read(temp.path()).unwrap();
        snapshot.scrollback_captured = true;
        snapshot.scrollback_bytes = Some(1_000_000);
        snapshot.scrollback_stored_bytes = Some(1_000);
        snapshot.windows[0].tabs[0].terminals[0].scrollback_file = Some("history.txt.lz4".into());
        let lines = content_lines(&snapshot);
        assert!(lines[0].contains("Codex 0 | Amp 0 | Claude 0 | Pi 0"));
        assert!(lines[1].contains("99.9% smaller"));
        assert!(lines.iter().all(|line| line.chars().count() <= 88));
    }

    #[test]
    fn reads_version_one_ruby_snapshots() {
        let temp = TempDir::new().unwrap();
        write_snapshot(temp.path(), "work_1", "2026-07-20T12:00:00+03:00");
        let snapshot = SnapshotStore::read(temp.path()).unwrap();
        validate_snapshot(&snapshot).unwrap();
        assert_eq!(snapshot.id, "work_1");
        assert_eq!(
            snapshot.windows[0].tabs[0].terminals[0].working_directory,
            "/tmp/project"
        );
        assert_eq!(snapshot.detected_codex_sessions[0].agent, Agent::Codex);
        assert!(snapshot.detected_claude_sessions.is_empty());
        assert!(snapshot.detected_pi_sessions.is_empty());
        assert!(
            snapshot.windows[0].tabs[0].terminals[0]
                .pi_session_id
                .is_none()
        );
    }

    #[test]
    fn sorts_snapshot_directories_by_saved_time() {
        let temp = TempDir::new().unwrap();
        let app = App::for_test(temp.path());
        write_snapshot(
            &app.snapshots.root.join("new"),
            "new",
            "2026-07-21T12:00:00+03:00",
        );
        write_snapshot(
            &app.snapshots.root.join("old"),
            "old",
            "2026-07-20T12:00:00+03:00",
        );
        let directories = app.snapshots.entries().unwrap();
        assert_eq!(directories[0].path.file_name().unwrap(), "old");
        assert_eq!(directories[1].path.file_name().unwrap(), "new");
    }

    #[test]
    fn refuses_to_remove_symlinked_snapshot() {
        let temp = TempDir::new().unwrap();
        let external = TempDir::new().unwrap();
        let app = App::for_test(temp.path());
        fs::create_dir_all(&app.snapshots.root).unwrap();
        write_snapshot(external.path(), "external", "2026-07-01T12:00:00+03:00");
        let link = app.snapshots.root.join("external");
        symlink(external.path(), &link).unwrap();
        let error = app.snapshots.remove(&link).unwrap_err();
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
