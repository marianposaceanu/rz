use std::{
    collections::{BTreeMap, HashSet},
    ffi::OsString,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use chrono::Local;

use crate::{
    command,
    model::{
        Agent, AgentSession, CaptureRow, Position, SNAPSHOT_VERSION, Scope, SessionKey, Size,
        Snapshot, Tab, Terminal, Window,
    },
};

const CAPTURE_HEADER: [&str; 17] = [
    "window_index",
    "window_id",
    "window_name",
    "window_x",
    "window_y",
    "window_width",
    "window_height",
    "tab_index",
    "tab_id",
    "tab_name",
    "tab_selected",
    "terminal_index",
    "terminal_id",
    "terminal_name",
    "terminal_focused",
    "working_directory",
    "scrollback_path",
];

pub fn ensure_running() -> Result<()> {
    version()
        .map(|_| ())
        .map_err(|_| anyhow::anyhow!("Ghostty must be running to save a snapshot"))
}

pub fn version() -> Result<String> {
    command::run(
        "osascript",
        &["-e", "tell application \"Ghostty\" to get version"],
    )
}

pub fn current_window_id() -> Result<String> {
    let id = command::run(
        "osascript",
        &[
            "-e",
            "tell application \"Ghostty\" to return id of first window as text",
        ],
    )?;
    if id.is_empty() {
        bail!("Ghostty has no current window");
    }
    Ok(id)
}

pub fn capture(
    script_path: &Path,
    capture_scrollback: bool,
    window_id: Option<&str>,
) -> Result<Vec<CaptureRow>> {
    let mut args = vec![script_path.as_os_str().to_owned()];
    if !capture_scrollback {
        args.push(OsString::from("--no-scrollback"));
    }
    if let Some(window_id) = window_id {
        args.push(OsString::from("--window-id"));
        args.push(OsString::from(window_id));
    }
    let output = command::run("osascript", &args)?;
    parse_capture(&output)
}

fn parse_capture(output: &str) -> Result<Vec<CaptureRow>> {
    let mut lines = output.lines().filter(|line| !line.is_empty());
    let header = lines
        .next()
        .map(|line| line.split('\t').collect::<Vec<_>>())
        .unwrap_or_default();
    if header != CAPTURE_HEADER {
        bail!("unexpected Ghostty capture format");
    }
    lines.map(parse_capture_row).collect()
}

fn parse_capture_row(line: &str) -> Result<CaptureRow> {
    let mut values = line.split('\t').collect::<Vec<_>>();
    if values.len() == CAPTURE_HEADER.len() - 1 {
        values.push("");
    }
    if values.len() != CAPTURE_HEADER.len() {
        bail!("malformed Ghostty capture row");
    }
    Ok(CaptureRow {
        window_index: parse_number(values[0], "window index")?,
        window_id: values[1].to_owned(),
        window_name: values[2].to_owned(),
        window_x: parse_optional_number(values[3], "window x")?,
        window_y: parse_optional_number(values[4], "window y")?,
        window_width: parse_optional_number(values[5], "window width")?,
        window_height: parse_optional_number(values[6], "window height")?,
        tab_index: parse_number(values[7], "tab index")?,
        tab_id: values[8].to_owned(),
        tab_name: values[9].to_owned(),
        tab_selected: values[10].eq_ignore_ascii_case("true"),
        terminal_index: parse_number(values[11], "terminal index")?,
        terminal_id: values[12].to_owned(),
        terminal_name: values[13].to_owned(),
        terminal_focused: values[14].eq_ignore_ascii_case("true"),
        working_directory: PathBuf::from(values[15]),
        scrollback_path: (!values[16].is_empty()).then(|| PathBuf::from(values[16])),
        scrollback_file: None,
        agent_session: None,
    })
}

fn parse_number<T>(value: &str, label: &str) -> Result<T>
where
    T: std::str::FromStr,
    T::Err: std::error::Error + Send + Sync + 'static,
{
    value.parse().with_context(|| format!("invalid {label}"))
}

fn parse_optional_number<T>(value: &str, label: &str) -> Result<Option<T>>
where
    T: std::str::FromStr,
    T::Err: std::error::Error + Send + Sync + 'static,
{
    if value.is_empty() {
        Ok(None)
    } else {
        parse_number(value, label).map(Some)
    }
}

pub struct SnapshotInput<'a> {
    pub name: &'a str,
    pub id: &'a str,
    pub timestamp: &'a str,
    pub rows: &'a [CaptureRow],
    pub process_sessions: &'a [AgentSession],
    pub warnings: Vec<String>,
    pub capture_scrollback: bool,
    pub target_window_id: Option<&'a str>,
    pub ghostty_version: String,
}

pub fn build_snapshot(input: SnapshotInput<'_>) -> Snapshot {
    let grouped_windows = input.rows.iter().fold(
        BTreeMap::<usize, Vec<&CaptureRow>>::new(),
        |mut grouped, row| {
            grouped.entry(row.window_index).or_default().push(row);
            grouped
        },
    );
    let windows = grouped_windows
        .into_iter()
        .map(|(window_index, window_rows)| {
            let first = window_rows[0];
            let grouped_tabs = window_rows.iter().fold(
                BTreeMap::<usize, Vec<&&CaptureRow>>::new(),
                |mut grouped, row| {
                    grouped.entry(row.tab_index).or_default().push(row);
                    grouped
                },
            );
            let tabs = grouped_tabs
                .into_iter()
                .map(|(tab_index, mut tab_rows)| {
                    tab_rows.sort_by_key(|row| row.terminal_index);
                    let first = *tab_rows[0];
                    let terminals = tab_rows
                        .into_iter()
                        .map(|row| {
                            let mut terminal = Terminal {
                                index: row.terminal_index,
                                id: Some(row.terminal_id.clone()),
                                name: row.terminal_name.clone(),
                                focused: row.terminal_focused,
                                working_directory: row
                                    .working_directory
                                    .to_string_lossy()
                                    .into_owned(),
                                scrollback_file: row.scrollback_file.clone(),
                                codex_session_id: None,
                                amp_thread_id: None,
                            };
                            if let Some(session) = &row.agent_session {
                                match session.agent {
                                    Agent::Amp => {
                                        terminal.amp_thread_id = Some(session.session_id.clone())
                                    }
                                    Agent::Codex => {
                                        terminal.codex_session_id = Some(session.session_id.clone())
                                    }
                                }
                            }
                            terminal
                        })
                        .collect();
                    Tab {
                        index: tab_index,
                        id: Some(first.tab_id.clone()),
                        name: first.tab_name.clone(),
                        selected: first.tab_selected,
                        terminals,
                    }
                })
                .collect();
            Window {
                index: window_index,
                id: Some(first.window_id.clone()),
                name: first.window_name.clone(),
                position: first
                    .window_x
                    .zip(first.window_y)
                    .map(|(x, y)| Position { x, y }),
                size: first
                    .window_width
                    .zip(first.window_height)
                    .map(|(width, height)| Size { width, height }),
                tabs,
            }
        })
        .collect::<Vec<_>>();
    let tabs_count = windows.iter().map(|window| window.tabs.len()).sum();
    let detected_codex_sessions = input
        .process_sessions
        .iter()
        .filter(|session| session.agent == Agent::Codex)
        .cloned()
        .collect();
    let detected_amp_threads = input
        .process_sessions
        .iter()
        .filter(|session| session.agent == Agent::Amp)
        .cloned()
        .collect();
    Snapshot {
        version: SNAPSHOT_VERSION,
        name: input.name.to_owned(),
        id: input.id.to_owned(),
        saved_at: Local::now().to_rfc3339(),
        timestamp: input.timestamp.to_owned(),
        ghostty_version: input.ghostty_version,
        tabs_count,
        terminals_count: input.rows.len(),
        scrollback_captured: input.capture_scrollback,
        scope: match input.target_window_id {
            Some(window_id) => Scope::Window {
                window_id: window_id.to_owned(),
            },
            None => Scope::AllWindows,
        },
        windows,
        detected_codex_sessions,
        detected_amp_threads,
        warnings: input.warnings,
        limitations: vec![
            "Ghostty does not expose split geometry; terminal counts restore as right-hand splits."
                .into(),
            if input.capture_scrollback {
                "Scrollback restores as plain text output, without original styling or interactive state."
                    .into()
            } else {
                "Scrollback was intentionally skipped for this fast snapshot.".into()
            },
            "Only Codex conversations and Amp threads are resumed; arbitrary child processes are not reconstructible."
                .into(),
        ],
    }
}

#[derive(Debug)]
pub struct RestorePlan {
    pub script: String,
    pub duplicates: Vec<SessionKey>,
}

pub fn restore_script(
    snapshot: &Snapshot,
    snapshot_dir: &Path,
    running_sessions: &HashSet<SessionKey>,
    home: &Path,
    close_existing: bool,
    ready_file: Option<&Path>,
) -> Result<RestorePlan> {
    let mut lines = vec![
        "tell application \"Ghostty\"".to_owned(),
        "activate".to_owned(),
    ];
    if close_existing {
        lines.push("set rzExistingWindowIds to id of every window".into());
    }
    let mut duplicates = Vec::new();
    let mut final_focus = None;

    for (window_offset, window) in snapshot.windows.iter().enumerate() {
        let window_number = window_offset + 1;
        let window_variable = format!("rzWindow{window_number}");
        let mut selected_tab = None;
        for (tab_offset, tab) in window.tabs.iter().enumerate() {
            if tab.terminals.is_empty() {
                continue;
            }
            let tab_number = tab_offset + 1;
            let first_configuration = format!("rzConfig{window_number}_{tab_number}_1");
            append_configuration(
                &mut lines,
                &first_configuration,
                &tab.terminals[0],
                snapshot_dir,
                running_sessions,
                &mut duplicates,
                home,
                ready_file,
            );
            let tab_variable = format!("rzTab{window_number}_{tab_number}");
            if tab_offset == 0 {
                lines.push(format!(
                    "set {window_variable} to new window with configuration {first_configuration}"
                ));
                lines.push(format!(
                    "set {tab_variable} to selected tab of {window_variable}"
                ));
            } else {
                lines.push(format!(
                    "set {tab_variable} to new tab in {window_variable} with configuration {first_configuration}"
                ));
            }
            let first_terminal = format!("rzTerminal{window_number}_{tab_number}_1");
            let mut terminal_variables = vec![first_terminal.clone()];
            lines.push(format!(
                "set {first_terminal} to focused terminal of {tab_variable}"
            ));

            for (terminal_offset, terminal) in tab.terminals.iter().skip(1).enumerate() {
                let terminal_number = terminal_offset + 2;
                let configuration =
                    format!("rzConfig{window_number}_{tab_number}_{terminal_number}");
                let terminal_variable =
                    format!("rzTerminal{window_number}_{tab_number}_{terminal_number}");
                append_configuration(
                    &mut lines,
                    &configuration,
                    terminal,
                    snapshot_dir,
                    running_sessions,
                    &mut duplicates,
                    home,
                    ready_file,
                );
                lines.push(format!(
                    "set {terminal_variable} to split {first_terminal} direction right with configuration {configuration}"
                ));
                terminal_variables.push(terminal_variable);
            }
            if !tab.name.is_empty() {
                lines.push(format!(
                    "perform action {} on {first_terminal}",
                    applescript_string(&format!("set_tab_title:{}", tab.name))
                ));
            }
            let focused = tab
                .terminals
                .iter()
                .position(|terminal| terminal.focused)
                .unwrap_or(0);
            if tab.selected {
                final_focus = terminal_variables.get(focused).cloned();
                selected_tab = Some(tab_variable);
            }
        }
        if let Some(tab) = selected_tab {
            lines.push(format!("select tab {tab}"));
        }
        append_window_geometry(&mut lines, window);
    }
    if let Some(terminal) = final_focus {
        lines.push(format!("focus {terminal}"));
    }
    if close_existing {
        append_close_existing(&mut lines, ready_file)?;
    }
    lines.push("end tell".into());
    let mut seen = HashSet::new();
    duplicates.retain(|session| seen.insert(session.clone()));
    Ok(RestorePlan {
        script: lines.join("\n"),
        duplicates,
    })
}

#[allow(clippy::too_many_arguments)]
fn append_configuration(
    lines: &mut Vec<String>,
    variable: &str,
    terminal: &Terminal,
    snapshot_dir: &Path,
    running_sessions: &HashSet<SessionKey>,
    duplicates: &mut Vec<SessionKey>,
    home: &Path,
    ready_file: Option<&Path>,
) {
    let cwd = if terminal.working_directory.is_empty() {
        home
    } else {
        Path::new(&terminal.working_directory)
    };
    lines.push(format!("set {variable} to new surface configuration"));
    lines.push(format!(
        "set initial working directory of {variable} to {}",
        applescript_string(&cwd.to_string_lossy())
    ));
    lines.push(format!(
        "set command of {variable} to {}",
        applescript_string(&restore_command(
            terminal,
            snapshot_dir,
            running_sessions,
            duplicates,
            cwd,
            ready_file,
        ))
    ));
    lines.push(format!("set wait after command of {variable} to true"));
}

fn restore_command(
    terminal: &Terminal,
    snapshot_dir: &Path,
    running_sessions: &HashSet<SessionKey>,
    duplicates: &mut Vec<SessionKey>,
    cwd: &Path,
    ready_file: Option<&Path>,
) -> String {
    let mut commands = vec![working_directory_report_command(cwd)];
    if let Some(ready_file) = ready_file {
        commands.push(format!(
            "until [ -e {} ]; do /bin/sleep 0.1; done",
            shell_quote(&ready_file.to_string_lossy())
        ));
    }
    if let Some(relative) = terminal.scrollback_file.as_deref() {
        let scrollback = snapshot_dir.join(relative);
        if scrollback.is_file() {
            commands.push(format!(
                "/bin/cat -- {}",
                shell_quote(&scrollback.to_string_lossy())
            ));
        }
    }
    match terminal.session() {
        Some(session) if !running_sessions.contains(&session.key()) => match session.agent {
            Agent::Amp => commands.push(format!(
                "exec amp threads continue {}",
                shell_quote(session.id)
            )),
            Agent::Codex => commands.push(format!("exec codex resume {}", shell_quote(session.id))),
        },
        Some(session) => {
            duplicates.push(session.key());
            commands.push(format!(
                "/usr/bin/printf '\\n[rz] {} {} already running: %s\\n' {}",
                session.agent.label(),
                session.agent.unit(),
                shell_quote(session.id)
            ));
            commands.push("exec /bin/zsh -l".into());
        }
        None => commands.push("exec /bin/zsh -l".into()),
    }
    format!("/bin/zsh -lc {}", shell_quote(&commands.join("; ")))
}

fn working_directory_report_command(cwd: &Path) -> String {
    let hostname = hostname::get()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    format!(
        "/usr/bin/printf '\\033]7;kitty-shell-cwd://%s%s\\007' {} {}",
        shell_quote(&hostname),
        shell_quote(&cwd.to_string_lossy())
    )
}

fn append_close_existing(lines: &mut Vec<String>, ready_file: Option<&Path>) -> Result<()> {
    let ready_file = ready_file.ok_or_else(|| anyhow::anyhow!("missing restore readiness file"))?;
    let ready_path = shell_quote(&ready_file.to_string_lossy());
    let ready_command =
        format!("/usr/bin/touch -- {ready_path}; /bin/sleep 5; /bin/rm -f -- {ready_path}");
    let helper_action = format!("do shell script {}", applescript_string(&ready_command));
    lines.push(
        r#"set rzCloseScript to "delay 1" & linefeed & "tell application \"Ghostty\"" & linefeed"#
            .into(),
    );
    lines.push("repeat with rzExistingWindowId in rzExistingWindowIds".into());
    lines.push(r#"set rzCloseScript to rzCloseScript & "try" & linefeed & "set rzWindowToClose to first window whose id is " & quote & (rzExistingWindowId as text) & quote & linefeed & "set rzTerminalToClose to focused terminal of selected tab of rzWindowToClose" & linefeed & "perform action \"close_window\" on rzTerminalToClose" & linefeed & "delay 0.2" & linefeed & "tell application \"System Events\"" & linefeed & "tell process \"Ghostty\"" & linefeed & "set rzDidConfirmClose to false" & linefeed & "repeat 10 times" & linefeed & "repeat with rzUiWindow in every window" & linefeed & "if exists sheet 1 of rzUiWindow then" & linefeed & "if exists static text \"Close Window?\" of sheet 1 of rzUiWindow then" & linefeed & "click button \"Close\" of sheet 1 of rzUiWindow" & linefeed & "set rzDidConfirmClose to true" & linefeed & "exit repeat" & linefeed & "end if" & linefeed & "end if" & linefeed & "end repeat" & linefeed & "if rzDidConfirmClose then exit repeat" & linefeed & "delay 0.05" & linefeed & "end repeat" & linefeed & "end tell" & linefeed & "end tell" & linefeed & "end try" & linefeed"#.into());
    lines.push("end repeat".into());
    lines.push(format!(
        "set rzCloseScript to rzCloseScript & \"end tell\" & linefeed & {} & linefeed",
        applescript_string(&helper_action)
    ));
    lines.push(
        r#"do shell script "/usr/bin/nohup /usr/bin/osascript -e " & quoted form of rzCloseScript & " >/dev/null 2>&1 &""#
            .into(),
    );
    Ok(())
}

fn append_window_geometry(lines: &mut Vec<String>, window: &Window) {
    let (Some(position), Some(size)) = (window.position, window.size) else {
        return;
    };
    lines.extend([
        "tell application \"System Events\"".into(),
        "tell process \"Ghostty\"".into(),
        format!(
            "set size of front window to {{{}, {}}}",
            size.width, size.height
        ),
        format!(
            "set position of front window to {{{}, {}}}",
            position.x, position.y
        ),
        "end tell".into(),
        "end tell".into(),
    ]);
}

fn applescript_string(value: &str) -> String {
    format!(
        "\"{}\"",
        value
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace(['\r', '\n'], " ")
    )
}

fn shell_quote(value: &str) -> String {
    if value.is_empty() {
        return "''".into();
    }
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn terminal() -> Terminal {
        Terminal {
            index: 1,
            id: Some("terminal".into()),
            name: "amp".into(),
            focused: true,
            working_directory: "/Users/me/project".into(),
            scrollback_file: None,
            codex_session_id: None,
            amp_thread_id: Some("T-01a023e2-3f9d-7705-98ed-4ea63108e87e".into()),
        }
    }

    #[test]
    fn parses_capture_tsv() {
        let output = format!(
            "{}\n2\twindow-current\tWork\t100\t120\t1200\t800\t1\ttab-1\tProject\ttrue\t1\tterminal-1\tamp\ttrue\t/tmp/project\t\n",
            CAPTURE_HEADER.join("\t")
        );
        let rows = parse_capture(&output).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].window_index, 2);
        assert_eq!(rows[0].working_directory, Path::new("/tmp/project"));
        assert_eq!(rows[0].scrollback_path, None);
    }

    #[test]
    fn restores_amp_threads_and_avoids_duplicates() {
        let terminal = terminal();
        let mut duplicates = Vec::new();
        let command = restore_command(
            &terminal,
            Path::new("/tmp/state"),
            &HashSet::new(),
            &mut duplicates,
            Path::new("/Users/me/project"),
            None,
        );
        assert!(command.contains("exec amp threads continue"));
        assert!(duplicates.is_empty());

        let running = HashSet::from([terminal.session().unwrap().key()]);
        let command = restore_command(
            &terminal,
            Path::new("/tmp/state"),
            &running,
            &mut duplicates,
            Path::new("/Users/me/project"),
            None,
        );
        assert!(command.contains("Amp thread already running"));
        assert!(!command.contains("exec amp threads continue"));
        assert_eq!(duplicates.len(), 1);
    }

    #[test]
    fn escapes_the_nested_close_window_applescript() {
        let mut lines = Vec::new();
        append_close_existing(&mut lines, Some(Path::new("/tmp/ready"))).unwrap();
        let script = lines.join("\n");

        assert!(script.contains(r#"tell application \"Ghostty\""#));
        assert!(!script.contains(r#"tell application \\"Ghostty\\""#));
    }

    #[test]
    fn shell_quote_handles_apostrophes() {
        assert_eq!(shell_quote("it's"), "'it'\\''s'");
    }
}
