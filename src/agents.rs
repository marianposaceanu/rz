use std::{
    collections::{BTreeSet, HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    sync::LazyLock,
};

use anyhow::Result;
use regex::Regex;

use crate::{
    command,
    model::{Agent, AgentSession, CaptureRow, SessionKey, Snapshot},
};

static PROCESS_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\s*(\d+)\s+(\d+)\s+(\S+)\s+(.+)$").expect("valid process regex")
});
static UUID_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}")
        .expect("valid UUID regex")
});
static CODEX_RESUME_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(r"(?i)\bresume\s+({})", UUID_PATTERN.as_str()))
        .expect("valid Codex resume regex")
});
static AMP_THREAD_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(r"(?i)T-{}", UUID_PATTERN.as_str())).expect("valid Amp thread regex")
});
static CODEX_ROLLOUT_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r"\s(/.*rollout-[^ ]+-({})\.jsonl)\s*$",
        UUID_PATTERN.as_str()
    ))
    .expect("valid Codex rollout regex")
});

#[derive(Debug, Clone)]
struct Process {
    pid: u32,
    ppid: u32,
    tty: String,
    command: String,
}

pub fn running_sessions() -> Result<Vec<AgentSession>> {
    let output = command::run("ps", &["-axo", "pid=,ppid=,tty=,command="])?;
    Ok(discover_sessions(
        &output,
        process_cwd,
        |pid, agent, command| match agent {
            Agent::Codex => CODEX_RESUME_PATTERN
                .captures(command)
                .and_then(|captures| captures.get(1))
                .map(|id| id.as_str().to_owned())
                .or_else(|| newest_open_codex_session(pid)),
            Agent::Amp => AMP_THREAD_PATTERN
                .find(command)
                .map(|id| id.as_str().to_owned())
                .or_else(|| open_amp_thread(pid)),
        },
    ))
}

fn discover_sessions(
    ps_output: &str,
    mut cwd_for_pid: impl FnMut(u32) -> Option<PathBuf>,
    mut id_for_process: impl FnMut(u32, Agent, &str) -> Option<String>,
) -> Vec<AgentSession> {
    let processes = parse_processes(ps_output);
    let by_pid = processes
        .iter()
        .map(|process| (process.pid, process))
        .collect::<HashMap<_, _>>();
    let ghostty_pids = processes
        .iter()
        .filter(|process| ghostty_process(&process.command))
        .map(|process| process.pid)
        .collect::<HashSet<_>>();

    processes
        .iter()
        .filter_map(|process| {
            let agent = if codex_process(&process.command) {
                Agent::Codex
            } else if amp_process(&process.command) {
                Agent::Amp
            } else {
                return None;
            };
            if !ghostty_pids.is_empty() && !descends_from(process, &ghostty_pids, &by_pid) {
                return None;
            }
            let cwd = cwd_for_pid(process.pid)?;
            let session_id = id_for_process(process.pid, agent, &process.command)?;
            Some(AgentSession {
                agent,
                pid: process.pid,
                tty: process.tty.clone(),
                cwd,
                session_id,
                command: process.command.clone(),
            })
        })
        .collect()
}

fn parse_processes(output: &str) -> Vec<Process> {
    output
        .lines()
        .filter_map(|line| {
            let captures = PROCESS_PATTERN.captures(line)?;
            Some(Process {
                pid: captures[1].parse().ok()?,
                ppid: captures[2].parse().ok()?,
                tty: captures[3].to_owned(),
                command: captures[4].to_owned(),
            })
        })
        .collect()
}

fn codex_process(command: &str) -> bool {
    command.contains("/bin/codex")
        && !command.contains("codex-code-mode-host")
        && !command.starts_with("node ")
}

fn amp_process(command: &str) -> bool {
    command
        .split_whitespace()
        .next()
        .and_then(|program| Path::new(program).file_name())
        .is_some_and(|program| program == "amp")
}

fn ghostty_process(command: &str) -> bool {
    command.contains("/Ghostty.app/Contents/MacOS/ghostty")
}

fn descends_from(
    process: &Process,
    ancestors: &HashSet<u32>,
    processes: &HashMap<u32, &Process>,
) -> bool {
    let mut seen = HashSet::new();
    let mut parent = process.ppid;
    while parent > 0 && seen.insert(parent) {
        if ancestors.contains(&parent) {
            return true;
        }
        let Some(process) = processes.get(&parent) else {
            return false;
        };
        parent = process.ppid;
    }
    false
}

fn process_cwd(pid: u32) -> Option<PathBuf> {
    let output = command::try_run(
        "lsof",
        &["-nP", "-a", "-p", &pid.to_string(), "-d", "cwd", "-Fn"],
    )?;
    output.lines().find_map(|line| {
        line.strip_prefix("n/")
            .map(|path| PathBuf::from(format!("/{path}")))
    })
}

fn newest_open_codex_session(pid: u32) -> Option<String> {
    let output = command::try_run("lsof", &["-nP", "-p", &pid.to_string()])?;
    output
        .lines()
        .filter_map(|line| {
            let captures = CODEX_ROLLOUT_PATTERN.captures(line)?;
            let path = PathBuf::from(captures.get(1)?.as_str());
            let modified = fs::metadata(&path).ok()?.modified().ok()?;
            let id = captures.get(2)?.as_str().to_owned();
            Some((modified, id))
        })
        .max_by_key(|(modified, _)| *modified)
        .map(|(_, id)| id)
}

fn open_amp_thread(pid: u32) -> Option<String> {
    let output = command::try_run("lsof", &["-nP", "-p", &pid.to_string(), "-Fn"])?;
    amp_thread_from_lsof(&output)
}

fn amp_thread_from_lsof(output: &str) -> Option<String> {
    let ids = output
        .lines()
        .filter(|line| {
            line.contains("/.cache/amp/logs/threads/") && line.trim_end().ends_with(".log")
        })
        .filter_map(|line| {
            AMP_THREAD_PATTERN
                .find(line)
                .map(|id| id.as_str().to_owned())
        })
        .collect::<BTreeSet<_>>();
    (ids.len() == 1).then(|| ids.into_iter().next()).flatten()
}

pub fn assign_sessions(
    rows: &[CaptureRow],
    sessions: &[AgentSession],
    snapshots_dir: &Path,
    warn_for_all_sessions: bool,
) -> (HashMap<String, AgentSession>, Vec<String>) {
    let mut assignments = HashMap::new();
    let mut remaining = sessions.to_vec();

    let mut ordered = sessions.to_vec();
    ordered.sort_by_key(|session| (session.agent, tty_number(&session.tty)));
    for session in ordered {
        let candidate = rows.iter().find(|row| {
            !assignments.contains_key(&row.terminal_id)
                && row.working_directory == session.cwd
                && agent_title(&row.terminal_name, &session.cwd, session.agent)
        });
        if let Some(row) = candidate {
            assignments.insert(row.terminal_id.clone(), session.clone());
            remove_session(&mut remaining, &session.key());
        }
    }

    let directories = remaining
        .iter()
        .map(|session| session.cwd.clone())
        .collect::<BTreeSet<_>>();
    for cwd in directories {
        let mut cwd_sessions = remaining
            .iter()
            .filter(|session| session.cwd == cwd)
            .cloned()
            .collect::<Vec<_>>();
        if cwd_sessions
            .iter()
            .map(|session| session.agent)
            .collect::<HashSet<_>>()
            .len()
            > 1
        {
            continue;
        }
        cwd_sessions.sort_by_key(|session| tty_number(&session.tty));
        let candidates = rows
            .iter()
            .filter(|row| {
                !assignments.contains_key(&row.terminal_id) && row.working_directory == cwd
            })
            .collect::<Vec<_>>();
        for (session, row) in cwd_sessions.into_iter().zip(candidates) {
            assignments.insert(row.terminal_id.clone(), session.clone());
            remove_session(&mut remaining, &session.key());
        }
    }

    let groups = remaining
        .iter()
        .map(|session| (session.agent, session.cwd.clone()))
        .collect::<BTreeSet<_>>();
    for (agent, cwd) in groups {
        let grouped = remaining
            .iter()
            .filter(|session| session.agent == agent && session.cwd == cwd)
            .cloned()
            .collect::<Vec<_>>();
        if grouped.len() != 1 {
            continue;
        }
        let candidates = rows
            .iter()
            .filter(|row| {
                !assignments.contains_key(&row.terminal_id)
                    && row.working_directory.as_os_str().is_empty()
                    && title_matches_cwd(&row.terminal_name, &cwd, agent)
            })
            .collect::<Vec<_>>();
        if candidates.len() == 1 {
            let session = &grouped[0];
            assignments.insert(candidates[0].terminal_id.clone(), session.clone());
            remove_session(&mut remaining, &session.key());
        }
    }

    let historical_names = historical_terminal_names(&remaining, snapshots_dir);
    for session in remaining.clone() {
        let Some(previous_name) = historical_names.get(&session.key()) else {
            continue;
        };
        let candidates = rows
            .iter()
            .filter(|row| {
                !assignments.contains_key(&row.terminal_id)
                    && row.working_directory.as_os_str().is_empty()
                    && stable_title(&row.terminal_name) == stable_title(previous_name)
            })
            .collect::<Vec<_>>();
        if candidates.len() == 1 {
            assignments.insert(candidates[0].terminal_id.clone(), session.clone());
            remove_session(&mut remaining, &session.key());
        }
    }

    let mut warnings = Vec::new();
    for session in &remaining {
        let available = rows
            .iter()
            .filter(|row| !assignments.contains_key(&row.terminal_id))
            .collect::<Vec<_>>();
        let title_candidates = available
            .iter()
            .filter(|row| {
                row.working_directory.as_os_str().is_empty()
                    && title_matches_cwd(&row.terminal_name, &session.cwd, session.agent)
            })
            .collect::<Vec<_>>();
        if !warn_for_all_sessions && title_candidates.is_empty() {
            continue;
        }
        let exact_cwd_exists = available
            .iter()
            .any(|row| row.working_directory == session.cwd);
        let ambiguous_agents = remaining.iter().any(|other| {
            other.key() != session.key() && other.cwd == session.cwd && other.agent != session.agent
        });
        let reason = if ambiguous_agents && exact_cwd_exists {
            "multiple agent types share this directory and terminal titles do not identify them"
        } else if title_candidates.len() > 1 {
            "multiple blank-directory Ghostty terminals have matching project titles"
        } else {
            "Ghostty exposed no terminal with that directory, a unique matching project title, or a unique prior session title"
        };
        warnings.push(format!(
            "{} {} {} on {} in {} was not matched: {reason}; it will not auto-resume",
            session.agent.label(),
            session.agent.unit(),
            session.session_id,
            session.tty,
            session.cwd.display()
        ));
    }

    (assignments, warnings)
}

fn remove_session(sessions: &mut Vec<AgentSession>, key: &SessionKey) {
    sessions.retain(|session| session.key() != *key);
}

fn historical_terminal_names(
    sessions: &[AgentSession],
    snapshots_dir: &Path,
) -> HashMap<SessionKey, String> {
    let mut wanted = sessions
        .iter()
        .map(AgentSession::key)
        .collect::<HashSet<_>>();
    let mut names = HashMap::new();
    if wanted.is_empty() {
        return names;
    }
    let Ok(entries) = fs::read_dir(snapshots_dir) else {
        return names;
    };
    let mut paths = entries
        .flatten()
        .map(|entry| entry.path().join("state.json"))
        .filter(|path| path.is_file())
        .collect::<Vec<_>>();
    paths.sort_by_key(|path| {
        fs::metadata(path)
            .and_then(|metadata| metadata.modified())
            .ok()
    });
    paths.reverse();
    for path in paths {
        let Ok(content) = fs::read_to_string(path) else {
            continue;
        };
        let Ok(snapshot) = serde_json::from_str::<Snapshot>(&content) else {
            continue;
        };
        for terminal in snapshot
            .windows
            .iter()
            .flat_map(|window| &window.tabs)
            .flat_map(|tab| &tab.terminals)
        {
            let Some(session) = terminal.session() else {
                continue;
            };
            let key = session.key();
            if wanted.contains(&key) && !terminal.name.is_empty() {
                names.insert(key.clone(), terminal.name.clone());
                wanted.remove(&key);
            }
        }
        if wanted.is_empty() {
            break;
        }
    }
    names
}

fn title_matches_cwd(title: &str, cwd: &Path, agent: Agent) -> bool {
    let basename = cwd
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    let title = title.trim();
    let starts_with_braille = title
        .chars()
        .next()
        .is_some_and(|character| ('\u{2800}'..='\u{28ff}').contains(&character));
    title == basename
        || title.ends_with(&format!("| {basename}"))
        || (starts_with_braille && title.ends_with(&format!(" {basename}")))
        || (agent == Agent::Codex
            && title.to_ascii_lowercase().contains("codex")
            && title.contains(basename))
        || (agent == Agent::Amp && amp_title(title) && title.contains(basename))
        || (title.contains("Action Required") && title.contains(basename))
}

fn agent_title(title: &str, cwd: &Path, agent: Agent) -> bool {
    if agent == Agent::Amp {
        return amp_title(title);
    }
    let basename = cwd
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    let starts_with_braille = title
        .chars()
        .next()
        .is_some_and(|character| ('\u{2800}'..='\u{28ff}').contains(&character));
    title.contains("Action Required")
        || title.to_ascii_lowercase().contains("codex")
        || title == basename
        || title.ends_with(&format!("| {basename}"))
        || (starts_with_braille && !amp_title(title))
}

fn amp_title(title: &str) -> bool {
    let normalized = title.trim().to_ascii_lowercase();
    normalized == "amp" || normalized.contains(" - amp - ") || normalized.ends_with(" - amp")
}

fn stable_title(title: &str) -> &str {
    let title = title.trim();
    match title.chars().next() {
        Some(character) if ('\u{2800}'..='\u{28ff}').contains(&character) => {
            title[character.len_utf8()..].trim_start()
        }
        _ => title,
    }
}

fn tty_number(tty: &str) -> u32 {
    let digits = tty
        .chars()
        .rev()
        .take_while(char::is_ascii_digit)
        .collect::<String>()
        .chars()
        .rev()
        .collect::<String>();
    digits.parse().unwrap_or(1_000_000)
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    const AMP_ID: &str = "T-01a023e2-3f9d-7705-98ed-4ea63108e87e";
    const CODEX_ID: &str = "019f7eb7-dc72-75b3-b042-91599cdd90ac";

    fn session(agent: Agent, tty: &str, cwd: &str, id: &str) -> AgentSession {
        AgentSession {
            agent,
            pid: 300,
            tty: tty.into(),
            cwd: cwd.into(),
            session_id: id.into(),
            command: agent.to_string(),
        }
    }

    fn row(id: &str, title: &str, cwd: &str) -> CaptureRow {
        CaptureRow {
            window_index: 1,
            window_id: "window".into(),
            window_name: "Work".into(),
            window_x: None,
            window_y: None,
            window_width: None,
            window_height: None,
            tab_index: 1,
            tab_id: "tab".into(),
            tab_name: "Project".into(),
            tab_selected: true,
            terminal_index: 1,
            terminal_id: id.into(),
            terminal_name: title.into(),
            terminal_focused: true,
            working_directory: cwd.into(),
            scrollback_path: None,
            scrollback_file: None,
            agent_session: None,
        }
    }

    #[test]
    fn extracts_exactly_one_amp_thread_from_lsof() {
        let output = format!(
            "p19089\nn/opt/homebrew/bin/amp\nn/Users/me/.cache/amp/logs/threads/{AMP_ID}.log\n"
        );
        assert_eq!(amp_thread_from_lsof(&output).as_deref(), Some(AMP_ID));
        let ambiguous = format!(
            "{output}n/Users/me/.cache/amp/logs/threads/T-01a0263b-7c89-7367-9e24-c78e3c0daedc.log\n"
        );
        assert_eq!(amp_thread_from_lsof(&ambiguous), None);
    }

    #[test]
    fn discovery_only_inspects_supported_ghostty_agents() {
        let ps = " 100 1 ?? /Applications/Ghostty.app/Contents/MacOS/ghostty\n\
                  200 100 ttys016 /bin/zsh -l\n\
                  300 200 ttys016 amp\n\
                  400 200 ttys016 /usr/bin/sleep 60";
        let mut inspected = Vec::new();
        let sessions = discover_sessions(
            ps,
            |pid| {
                inspected.push(pid);
                Some("/Users/me/project".into())
            },
            |_, _, _| Some(AMP_ID.into()),
        );
        assert_eq!(inspected, vec![300]);
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].agent, Agent::Amp);
    }

    #[test]
    fn matches_amp_and_codex_titles_in_the_same_directory() {
        let cwd = "/Users/me/dot-files";
        let rows = vec![
            row("amp-terminal", "⣒ display task - amp - ~/dot-files", cwd),
            row("codex-terminal", "codex | dot-files", cwd),
        ];
        let amp = session(Agent::Amp, "ttys016", cwd, AMP_ID);
        let codex = session(Agent::Codex, "ttys004", cwd, CODEX_ID);
        let temp = TempDir::new().unwrap();
        let (assigned, warnings) =
            assign_sessions(&rows, &[codex.clone(), amp.clone()], temp.path(), true);
        assert_eq!(assigned["amp-terminal"], amp);
        assert_eq!(assigned["codex-terminal"], codex);
        assert!(warnings.is_empty());
    }

    #[test]
    fn refuses_ambiguous_cross_agent_matching() {
        let cwd = "/Users/me/dot-files";
        let rows = vec![row("one", "shell one", cwd), row("two", "shell two", cwd)];
        let sessions = [
            session(Agent::Codex, "ttys004", cwd, CODEX_ID),
            session(Agent::Amp, "ttys016", cwd, AMP_ID),
        ];
        let temp = TempDir::new().unwrap();
        let (assigned, warnings) = assign_sessions(&rows, &sessions, temp.path(), true);
        assert!(assigned.is_empty());
        assert_eq!(warnings.len(), 2);
        assert!(
            warnings
                .iter()
                .all(|warning| warning.contains("multiple agent types"))
        );
    }

    #[test]
    fn matches_one_blank_cwd_by_unique_project_title() {
        let rows = vec![row("terminal", "fws-docs", "")];
        let session = session(Agent::Codex, "ttys002", "/Users/me/fws-docs", CODEX_ID);
        let temp = TempDir::new().unwrap();
        let (assigned, warnings) =
            assign_sessions(&rows, std::slice::from_ref(&session), temp.path(), true);
        assert_eq!(assigned["terminal"], session);
        assert!(warnings.is_empty());
    }

    #[test]
    fn does_not_guess_between_duplicate_blank_titles() {
        let rows = vec![row("one", "fws-docs", ""), row("two", "fws-docs", "")];
        let session = session(Agent::Codex, "ttys002", "/Users/me/fws-docs", CODEX_ID);
        let temp = TempDir::new().unwrap();
        let (assigned, warnings) = assign_sessions(&rows, &[session], temp.path(), true);
        assert!(assigned.is_empty());
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("multiple blank-directory"));
    }
}
