use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};

use serde::Deserialize;
use url::Url;

use crate::error::{AgentError, AgentResult};
use crate::platform;
use crate::process::stop_owned_process;

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
}

impl StartingBridge {
    pub fn start(executable: &Path, startup: &Path, log: &Path) -> AgentResult<Self> {
        let mut command = Command::new(executable);
        command
            .arg("serve")
            .arg("--startup")
            .arg(startup)
            .arg("--ready")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::from(platform::open_private_append(log)?));
        Ok(Self {
            child: Some(platform::spawn_detached(&mut command)?),
        })
    }

    pub fn ready(&mut self) -> AgentResult<ReadyRecord> {
        let stdout = self
            .child
            .as_mut()
            .and_then(|child| child.stdout.take())
            .ok_or_else(|| AgentError::State("the bridge startup channel is unavailable".into()))?;
        let mut line = String::new();
        BufReader::new(stdout).read_line(&mut line)?;
        serde_json::from_str(&line)
            .map_err(|_| AgentError::State("the bridge stopped before it was ready".into()))
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
