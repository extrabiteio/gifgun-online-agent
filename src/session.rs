use std::fmt;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use subtle::ConstantTimeEq;

use crate::error::{AgentError, AgentResult};
use crate::platform;

const SESSION_LIFETIME_MILLIS: u64 = 24 * 60 * 60 * 1_000;

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: String) -> Self {
        Self(value)
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("[REDACTED]")
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    Starting,
    Waiting,
    Connected,
    Stopped,
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentSessionRecord {
    pub session_id: String,
    pub port: u16,
    pub agent_token: Secret,
    pub pid: Option<u32>,
    pub created_at: u64,
    pub expires_at: u64,
    pub status: SessionStatus,
}

impl AgentSessionRecord {
    pub fn new(session_id: String, port: u16, agent_token: String) -> Self {
        let created_at = now_millis();
        Self {
            session_id,
            port,
            agent_token: Secret::new(agent_token),
            pid: None,
            created_at,
            expires_at: created_at.saturating_add(SESSION_LIFETIME_MILLIS),
            status: SessionStatus::Starting,
        }
    }
}

impl fmt::Debug for AgentSessionRecord {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AgentSessionRecord")
            .field("session_id", &self.session_id)
            .field("port", &self.port)
            .field("agent_token", &self.agent_token)
            .field("pid", &self.pid)
            .field("created_at", &self.created_at)
            .field("expires_at", &self.expires_at)
            .field("status", &self.status)
            .finish()
    }
}

#[derive(Clone, Deserialize, PartialEq, Serialize)]
pub struct StartupRecord {
    pub envelope: Value,
    pub session: AgentSessionRecord,
}

impl fmt::Debug for StartupRecord {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StartupRecord")
            .field("envelope", &"[REDACTED]")
            .field("session", &self.session)
            .finish()
    }
}

#[derive(Clone, Debug)]
pub struct SessionStore {
    root: PathBuf,
}

impl SessionStore {
    pub fn discover() -> AgentResult<Self> {
        if let Some(path) = std::env::var_os("GIFGUN_AGENT_CACHE_DIR") {
            return Ok(Self::at(path));
        }
        let directories = ProjectDirs::from("com", "Extrabite", "GifGun Agent")
            .ok_or_else(|| AgentError::State("no per-user cache directory is available".into()))?;
        Ok(Self::at(directories.cache_dir()))
    }

    pub fn at(path: impl AsRef<Path>) -> Self {
        Self {
            root: path.as_ref().to_path_buf(),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn ensure(&self) -> AgentResult<()> {
        platform::create_private_dir(&self.sessions_directory())?;
        Ok(())
    }

    pub fn sessions_directory(&self) -> PathBuf {
        self.root.join("sessions")
    }

    pub fn session_path(&self, session_id: &str) -> PathBuf {
        self.sessions_directory().join(format!("{session_id}.json"))
    }

    pub fn startup_path(&self, session_id: &str) -> PathBuf {
        self.sessions_directory()
            .join(format!("{session_id}.startup.json"))
    }

    pub fn current_path(&self) -> PathBuf {
        self.root.join("current")
    }

    pub fn log_path(&self, session_id: &str) -> PathBuf {
        self.root.join(format!("bridge-{session_id}.log"))
    }

    pub fn write_session(&self, session: &AgentSessionRecord) -> AgentResult<()> {
        ensure_session_id(&session.session_id)?;
        self.ensure()?;
        self.write_json(&self.session_path(&session.session_id), session)
    }

    pub fn read_session(&self, session_id: Option<&str>) -> AgentResult<AgentSessionRecord> {
        let selected = match session_id {
            Some(value) => value.to_owned(),
            None => self.current_session_id()?,
        };
        ensure_session_id(&selected)?;
        let session: AgentSessionRecord = self.read_json(&self.session_path(&selected))?;
        if session.expires_at <= now_millis() {
            let _ = self.remove_session(&selected);
            return Err(AgentError::State(
                "the selected agent session has expired".into(),
            ));
        }
        Ok(session)
    }

    pub fn write_startup(&self, startup: &StartupRecord) -> AgentResult<PathBuf> {
        ensure_session_id(&startup.session.session_id)?;
        self.ensure()?;
        let path = self.startup_path(&startup.session.session_id);
        self.write_json(&path, startup)?;
        Ok(path)
    }

    pub fn consume_startup(&self, path: &Path) -> AgentResult<StartupRecord> {
        let expected_parent = self.sessions_directory();
        let valid_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.ends_with(".startup.json"));
        let metadata = fs::symlink_metadata(path)?;
        if path.parent() != Some(expected_parent.as_path())
            || !valid_name
            || !metadata.file_type().is_file()
        {
            return Err(AgentError::State(
                "startup record is outside the private session directory".into(),
            ));
        }
        let contents = fs::read(path);
        let removal = fs::remove_file(path);
        let contents = contents?;
        removal?;
        serde_json::from_slice(&contents)
            .map_err(|error| AgentError::State(format!("startup record is malformed: {error}")))
    }

    pub fn set_current(&self, session_id: &str) -> AgentResult<()> {
        ensure_session_id(session_id)?;
        self.ensure()?;
        write_private(&self.current_path(), format!("{session_id}\n").as_bytes())
    }

    pub fn current_session_id(&self) -> AgentResult<String> {
        let value = String::from_utf8(fs::read(self.current_path())?)
            .map_err(|_| AgentError::State("current session selection is malformed".into()))?;
        let selected = value.trim();
        if ensure_session_id(selected).is_err() {
            return Err(AgentError::State(
                "current session selection is malformed".into(),
            ));
        }
        Ok(selected.to_owned())
    }

    pub fn remove_session(&self, session_id: &str) -> AgentResult<()> {
        ensure_session_id(session_id)?;
        remove_if_exists(&self.session_path(session_id))?;
        let current = self.current_session_id().unwrap_or_default();
        if current == session_id {
            remove_if_exists(&self.current_path())?;
        }
        remove_if_exists(&self.startup_path(session_id))?;
        Ok(())
    }

    fn write_json<T: Serialize>(&self, path: &Path, value: &T) -> AgentResult<()> {
        let mut contents = serde_json::to_vec(value)
            .map_err(|error| AgentError::State(format!("session state is invalid: {error}")))?;
        contents.push(b'\n');
        write_private(path, &contents)
    }

    fn read_json<T: for<'de> Deserialize<'de>>(&self, path: &Path) -> AgentResult<T> {
        serde_json::from_slice(&fs::read(path)?)
            .map_err(|error| AgentError::State(format!("session state is malformed: {error}")))
    }
}

pub fn random_secret(bytes: usize) -> AgentResult<String> {
    let mut value = vec![0_u8; bytes];
    getrandom::fill(&mut value).map_err(|_| AgentError::Random)?;
    Ok(URL_SAFE_NO_PAD.encode(value))
}

pub fn secrets_equal(left: Option<&str>, right: Option<&str>) -> bool {
    let (Some(left), Some(right)) = (left, right) else {
        return false;
    };
    if left.len() != right.len() {
        return false;
    }
    bool::from(left.as_bytes().ct_eq(right.as_bytes()))
}

fn ensure_session_id(session_id: &str) -> AgentResult<()> {
    if session_id.len() < 8
        || session_id.len() > 256
        || !session_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    {
        return Err(AgentError::State("session identifier is malformed".into()));
    }
    Ok(())
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

fn write_private(path: &Path, contents: &[u8]) -> AgentResult<()> {
    let mut file = platform::open_private_write(path)?;
    file.write_all(contents)?;
    Ok(())
}

fn remove_if_exists(path: &Path) -> AgentResult<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}
