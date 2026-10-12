use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use serde::Deserialize;
use url::Url;

use crate::error::{AgentError, AgentResult};
use crate::platform;
use crate::process::stop_owned_process;
use crate::session::{AgentSessionRecord, SessionStore, StartupRecord, random_secret};

pub const APP_URL: &str = "https://gifgun.online/app";

pub fn app_url(value: &str) -> AgentResult<Url> {
    let parsed =
        Url::parse(value).map_err(|_| AgentError::Cli("the editor address is invalid".into()))?;
    let official = parsed.scheme() == "https"
        && parsed.host_str() == Some("gifgun.online")
        && parsed.port().is_none();
    let local = parsed.scheme() == "http" && parsed.host_str() == Some("127.0.0.1");
    if !(official || local)
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.path() != "/app"
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        return Err(AgentError::Cli(
            "use the official editor address or a loopback editor at /app".into(),
        ));
    }
    Ok(parsed)
}

pub fn workspace_url(mut app: Url, session_id: &str, port: u16, workspace_id: &str) -> Url {
    app.query_pairs_mut()
        .append_pair("ae", "1")
        .append_pair("workspace", workspace_id)
        .append_pair("agentSession", session_id)
        .append_pair("agentPort", &port.to_string());
    app
}

pub fn open_browser(url: &Url) -> AgentResult<()> {
    #[cfg(windows)]
    let mut command = {
        let root = std::env::var_os("SystemRoot").ok_or_else(|| {
            AgentError::State("the system browser launcher is unavailable".into())
        })?;
        if !Path::new(&root).is_absolute() {
            return Err(AgentError::State(
                "the system browser launcher is invalid".into(),
            ));
        }
        let mut command = Command::new(Path::new(&root).join("System32").join("rundll32.exe"));
        command.arg("url.dll,FileProtocolHandler");
        command
    };
    #[cfg(target_os = "macos")]
    let mut command = Command::new("/usr/bin/open");
    #[cfg(all(unix, not(target_os = "macos")))]
    let mut command = Command::new("xdg-open");
    let status = command
        .arg(url.as_str())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|_| AgentError::State("the system browser could not open GifGun".into()))?;
    if !status.success() {
        return Err(AgentError::State(
            "the system browser did not accept the editor address".into(),
        ));
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReadyRecord {
    pub session_id: String,
    pub port: u16,
}

// Keep ownership until the launch result reaches the caller. A failed launch stops only this child.
pub struct StartingBridge {
    child: Option<Child>,
    reader: Option<BufReader<ChildStdout>>,
}

impl StartingBridge {
    pub fn start(executable: &Path, startup: &Path, log: &Path) -> AgentResult<Self> {
        Self::start_mode(executable, startup, log, false)
    }

    fn start_mode(executable: &Path, startup: &Path, log: &Path, owned: bool) -> AgentResult<Self> {
        let mut command = Command::new(executable);
        command
            .arg("serve")
            .arg("--startup")
            .arg(startup)
            .arg("--ready")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::from(platform::open_private_append(log)?));
        if owned {
            command.arg("--owned");
        }
        let mut child = platform::spawn_detached(&mut command)?;
        let reader = child.stdout.take().map(BufReader::new);
        Ok(Self {
            child: Some(child),
            reader,
        })
    }

    pub fn ready(&mut self) -> AgentResult<ReadyRecord> {
        let reader = self
            .reader
            .as_mut()
            .ok_or_else(|| AgentError::State("the bridge startup channel is unavailable".into()))?;
        let mut line = String::new();
        reader.read_line(&mut line)?;
        serde_json::from_str(&line)
            .map_err(|_| AgentError::State("the bridge stopped before it was ready".into()))
    }

    pub fn own(mut self) -> AgentResult<OwnedBridge> {
        let child = self
            .child
            .as_mut()
            .ok_or_else(|| AgentError::State("the launch process is unavailable".into()))?;
        let mut owner = child.stdin.take().ok_or_else(|| {
            AgentError::State("the launch ownership channel is unavailable".into())
        })?;
        owner.write_all(b"ready\n")?;
        Ok(OwnedBridge {
            child: self.child.take().unwrap(),
            owner: Some(owner),
            reader: self.reader.take(),
        })
    }

    pub fn release(mut self) -> AgentResult<()> {
        let child = self
            .child
            .as_mut()
            .ok_or_else(|| AgentError::State("the launch process is unavailable".into()))?;
        let mut stdin = child.stdin.take().ok_or_else(|| {
            AgentError::State("the launch confirmation channel is unavailable".into())
        })?;
        stdin.write_all(b"ready\n")?;
        drop(stdin);
        self.child.take();
        Ok(())
    }
}

impl Drop for StartingBridge {
    fn drop(&mut self) {
        if let Some(child) = &mut self.child {
            let _ = stop_owned_process(child);
        }
    }
}

pub struct OwnedBridge {
    child: Child,
    owner: Option<ChildStdin>,
    pub reader: Option<BufReader<ChildStdout>>,
}

impl Drop for OwnedBridge {
    fn drop(&mut self) {
        // EOF ends this bridge's ownership. Let the daemon remove its own files.
        self.owner.take();
        let _ = self.child.wait();
    }
}

pub struct PendingEditor {
    pub session_id: String,
    pub port: u16,
    pub editor_url: Url,
    bridge: Option<StartingBridge>,
    store: SessionStore,
    transferred: bool,
}

impl PendingEditor {
    pub fn start(app: &str, no_browser: bool, owned: bool) -> AgentResult<Self> {
        let app = app_url(app)?;
        let store = SessionStore::discover()?;
        let id = random_secret(18)?;
        let mut session = AgentSessionRecord::new(id.clone(), 0, random_secret(32)?);
        session.workspace_id = Some(id.clone());
        store.write_session(&session)?;
        let mut pending = Self {
            session_id: id.clone(),
            port: 0,
            editor_url: app.clone(),
            bridge: None,
            store,
            transferred: false,
        };
        let startup = pending.store.write_startup(&StartupRecord {
            envelope: serde_json::Value::Null,
            session,
            launch_origin: Some(app.origin().ascii_serialization()),
        })?;
        pending.bridge = Some(StartingBridge::start_mode(
            &std::env::current_exe()?,
            &startup,
            &pending.store.log_path(&id),
            owned,
        )?);
        let ready = pending.bridge.as_mut().unwrap().ready()?;
        let persisted = pending.store.read_session(Some(&id))?;
        if ready.session_id != id
            || ready.port == 0
            || persisted.port != ready.port
            || persisted.workspace_id.as_deref() != Some(id.as_str())
        {
            return Err(AgentError::State(
                "the bridge startup response does not match this workspace".into(),
            ));
        }
        pending.port = ready.port;
        pending.editor_url = workspace_url(app, &id, ready.port, &id);
        if !no_browser {
            open_browser(&pending.editor_url)?;
        }
        Ok(pending)
    }

    pub fn detach(mut self) -> AgentResult<()> {
        self.bridge.take().unwrap().release()?;
        self.transferred = true;
        Ok(())
    }

    pub fn own(mut self) -> AgentResult<OwnedBridge> {
        let bridge = self.bridge.take().unwrap().own()?;
        self.transferred = true;
        Ok(bridge)
    }
}

impl Drop for PendingEditor {
    fn drop(&mut self) {
        if !self.transferred {
            self.bridge.take();
            let _ = self.store.remove_session(&self.session_id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn accepts_only_the_official_editor_or_explicit_loopback_editor() {
        assert!(app_url(APP_URL).is_ok());
        assert!(app_url("http://127.0.0.1:3130/app").is_ok());
        for value in [
            "https://other.test/app",
            "https://gifgun.online/app?token=synthetic",
            "https://synthetic:synthetic@gifgun.online/app",
            "file:///app",
            "http://gifgun.online/app",
        ] {
            assert!(app_url(value).is_err());
        }
        let url = workspace_url(
            app_url(APP_URL).unwrap(),
            "session-owned",
            43115,
            "workspace-owned",
        );
        assert_eq!(
            url.as_str(),
            "https://gifgun.online/app?ae=1&workspace=workspace-owned&agentSession=session-owned&agentPort=43115"
        );
    }
}
