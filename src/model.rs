use std::{fmt, path::PathBuf};

use serde::{Deserialize, Serialize};

pub const SNAPSHOT_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snapshot {
    pub version: u32,
    #[serde(default)]
    pub name: String,
    pub id: String,
    pub saved_at: String,
    #[serde(default)]
    pub timestamp: String,
    #[serde(default)]
    pub ghostty_version: String,
    #[serde(default)]
    pub tabs_count: usize,
    #[serde(default)]
    pub terminals_count: usize,
    #[serde(default)]
    pub scrollback_captured: bool,
    #[serde(default)]
    pub scope: Scope,
    #[serde(default)]
    pub windows: Vec<Window>,
    #[serde(default)]
    pub detected_codex_sessions: Vec<AgentSession>,
    #[serde(default)]
    pub detected_amp_threads: Vec<AgentSession>,
    #[serde(default)]
    pub warnings: Vec<String>,
    #[serde(default)]
    pub limitations: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Scope {
    Window {
        window_id: String,
    },
    #[default]
    AllWindows,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Window {
    pub index: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default)]
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<Position>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<Size>,
    #[serde(default)]
    pub tabs: Vec<Tab>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Position {
    pub x: i64,
    pub y: i64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Size {
    pub width: i64,
    pub height: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tab {
    pub index: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub selected: bool,
    #[serde(default)]
    pub terminals: Vec<Terminal>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Terminal {
    pub index: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub focused: bool,
    #[serde(default)]
    pub working_directory: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scrollback_file: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub codex_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub amp_thread_id: Option<String>,
}

impl Terminal {
    pub fn session(&self) -> Option<SessionRef<'_>> {
        if let Some(id) = self.codex_session_id.as_deref() {
            Some(SessionRef {
                agent: Agent::Codex,
                id,
            })
        } else {
            self.amp_thread_id.as_deref().map(|id| SessionRef {
                agent: Agent::Amp,
                id,
            })
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Agent {
    Amp,
    Codex,
}

impl Agent {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Amp => "Amp",
            Self::Codex => "Codex",
        }
    }

    pub const fn unit(self) -> &'static str {
        match self {
            Self::Amp => "thread",
            Self::Codex => "session",
        }
    }
}

impl fmt::Display for Agent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Amp => "amp",
            Self::Codex => "codex",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentSession {
    #[serde(default = "default_agent")]
    pub agent: Agent,
    pub pid: u32,
    pub tty: String,
    pub cwd: PathBuf,
    pub session_id: String,
    #[serde(default)]
    pub command: String,
}

const fn default_agent() -> Agent {
    Agent::Codex
}

impl AgentSession {
    pub fn key(&self) -> SessionKey {
        SessionKey {
            agent: self.agent,
            id: self.session_id.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SessionKey {
    pub agent: Agent,
    pub id: String,
}

#[derive(Debug, Clone, Copy)]
pub struct SessionRef<'a> {
    pub agent: Agent,
    pub id: &'a str,
}

impl SessionRef<'_> {
    pub fn key(self) -> SessionKey {
        SessionKey {
            agent: self.agent,
            id: self.id.to_owned(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct CaptureRow {
    pub window_index: usize,
    pub window_id: String,
    pub window_name: String,
    pub window_x: Option<i64>,
    pub window_y: Option<i64>,
    pub window_width: Option<i64>,
    pub window_height: Option<i64>,
    pub tab_index: usize,
    pub tab_id: String,
    pub tab_name: String,
    pub tab_selected: bool,
    pub terminal_index: usize,
    pub terminal_id: String,
    pub terminal_name: String,
    pub terminal_focused: bool,
    pub working_directory: PathBuf,
    pub scrollback_path: Option<PathBuf>,
    pub scrollback_file: Option<String>,
    pub agent_session: Option<AgentSession>,
}

impl CaptureRow {
    pub fn has_geometry(&self) -> bool {
        self.window_x.is_some()
            && self.window_y.is_some()
            && self.window_width.is_some()
            && self.window_height.is_some()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WatchState {
    pub version: u32,
    pub token: String,
    pub name: String,
    pub interval_seconds: u64,
    pub scope: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_id: Option<String>,
    pub owner_pid: u32,
    pub owner_started_at: String,
    pub worker_pid: u32,
    pub started_at: String,
    pub log_path: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_attempt_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_success_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_snapshot_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_attempt_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}
