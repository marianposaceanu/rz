use std::{
    env, fs,
    io::{BufRead, BufReader, Read},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

const EXTENSION: &str = include_str!("../libexec/pi-session-tracker.ts");
const EXTENSION_NAME: &str = "rz-session-tracker.ts";

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackedSession {
    pid: u32,
    started_at: i64,
    pub session_id: String,
    pub session_file: PathBuf,
    pub cwd: PathBuf,
}

#[derive(Deserialize)]
struct SessionHeader {
    #[serde(rename = "type")]
    kind: String,
    id: String,
}

pub fn config_dir(home: &Path) -> PathBuf {
    match env::var_os("PI_CODING_AGENT_DIR").filter(|value| !value.is_empty()) {
        Some(value) => {
            let path = PathBuf::from(value);
            if path == Path::new("~") {
                home.to_owned()
            } else if let Ok(relative) = path.strip_prefix("~") {
                home.join(relative)
            } else {
                path
            }
        }
        None => home.join(".pi/agent"),
    }
}

pub fn session_for_process(agent_dir: &Path, pid: u32, started_at: i64) -> Option<TrackedSession> {
    let path = agent_dir.join("rz-sessions").join(format!("{pid}.json"));
    let session: TrackedSession = serde_json::from_str(&fs::read_to_string(path).ok()?).ok()?;
    if session.pid != pid
        || session.started_at.abs_diff(started_at) > 3
        || session.session_id.is_empty()
        || !session.session_file.is_absolute()
        || !session.cwd.is_absolute()
    {
        return None;
    }
    let file = fs::File::open(&session.session_file).ok()?;
    let header = BufReader::new(file.take(16_384)).lines().next()?.ok()?;
    let header: SessionHeader = serde_json::from_str(&header).ok()?;
    (header.kind == "session" && header.id == session.session_id).then_some(session)
}

pub fn install(home: &Path) -> Result<PathBuf> {
    install_in(&config_dir(home))
}

fn install_in(agent_dir: &Path) -> Result<PathBuf> {
    let directory = agent_dir.join("extensions");
    fs::create_dir_all(&directory).context("could not create Pi extensions directory")?;
    let destination = directory.join(EXTENSION_NAME);
    if destination.exists() && !fs::read_to_string(&destination)?.starts_with("// Managed by rz.") {
        bail!(
            "refusing to overwrite an unmanaged Pi extension: {}",
            destination.display()
        );
    }
    let temporary = directory.join(format!(".{EXTENSION_NAME}.{}.tmp", std::process::id()));
    fs::write(&temporary, EXTENSION).context("could not write Pi extension")?;
    fs::rename(&temporary, &destination).context("could not install Pi extension")?;
    Ok(destination)
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    #[test]
    fn accepts_only_the_persisted_session_owned_by_this_process() {
        let temp = TempDir::new().unwrap();
        let file = temp.path().join("custom sessions/work's session.jsonl");
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, "{\"type\":\"session\",\"id\":\"custom-id\"}\n").unwrap();
        let directory = temp.path().join("rz-sessions");
        fs::create_dir_all(&directory).unwrap();
        let record = serde_json::json!({
            "pid": 300,
            "startedAt": 1000,
            "sessionId": "custom-id",
            "sessionFile": file,
            "cwd": "/tmp/project",
        });
        let path = directory.join("300.json");
        fs::write(&path, record.to_string()).unwrap();
        let session = session_for_process(temp.path(), 300, 1001).unwrap();
        assert_eq!(session.session_id, "custom-id");
        assert_eq!(session.session_file, file);
        assert!(session_for_process(temp.path(), 300, 2000).is_none());
        assert!(session_for_process(temp.path(), 301, 1000).is_none());

        let mut wrong_pid = record.clone();
        wrong_pid["pid"] = 999.into();
        fs::write(&path, wrong_pid.to_string()).unwrap();
        assert!(session_for_process(temp.path(), 300, 1000).is_none());
        fs::write(&path, record.to_string()).unwrap();
        fs::write(&file, "{\"type\":\"session\",\"id\":\"other\"}\n").unwrap();
        assert!(session_for_process(temp.path(), 300, 1000).is_none());
        fs::remove_file(&file).unwrap();
        assert!(session_for_process(temp.path(), 300, 1000).is_none());
        fs::write(&path, "broken JSON").unwrap();
        assert!(session_for_process(temp.path(), 300, 1000).is_none());
    }

    #[test]
    fn installs_and_updates_only_its_own_extension() {
        let temp = TempDir::new().unwrap();
        let path = install_in(temp.path()).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), EXTENSION);
        assert_eq!(install_in(temp.path()).unwrap(), path);
        fs::write(&path, "// My extension\n").unwrap();
        assert!(install_in(temp.path()).is_err());
        assert_eq!(fs::read_to_string(path).unwrap(), "// My extension\n");
    }
}
