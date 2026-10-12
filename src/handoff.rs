use crate::error::{AgentError, AgentResult};
use crate::session::ensure_session_id;

pub fn instruction(session_id: &str) -> AgentResult<String> {
    ensure_session_id(session_id)?;
    let asset = match (std::env::consts::OS, std::env::consts::ARCH) {
        ("windows", "x86_64") => "gifgun-agent-windows-x64.exe",
        ("macos", "aarch64") => "gifgun-agent-darwin-arm64",
        ("macos", "x86_64") => "gifgun-agent-darwin-x64",
        ("linux", "x86_64") => "gifgun-agent-linux-x64",
        _ => {
            return Err(AgentError::State(
                "this platform has no published Agent executable".into(),
            ));
        }
    };
    let version = env!("CARGO_PKG_VERSION");
    Ok(format!(
        "Use my approved GifGun editor session on this machine.\n\nRequired native Agent version: {version}\nSession: {session_id}\n\nReuse the exact-version executable if it is cached. Otherwise download it without administrator access from this public URL:\nhttps://github.com/extrabiteio/gifgun-online-agent/releases/download/v{version}/{asset}\nVerify a new download against SHA256SUMS from that release before running it. No GitHub login or credential is needed. Do not print redirected download URLs. On macOS or Linux, make the file executable.\n\nRun --version and require the exact line \"gifgun-agent {version}\". Use the same local user and session store as the launcher. Do not change GIFGUN_AGENT_CACHE_DIR.\n\nUse --session {session_id} for every session command. Run status and require browserConnected true. If this session is missing or disconnected, stop and ask me to reconnect it. Do not select another session, run open, launch, or pair, or read session credential files.\n\nRun --help, then capabilities --list and state --summary with this session. Use runtime capability definitions for my task. Chain document revisions and wait for active work to finish. Do not inspect GifGun source code. Keep output small. Media and rendering stay in the browser."
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handoff_contains_only_public_download_and_explicit_session_guidance() {
        let text = instruction("session-owned").unwrap();
        assert!(text.contains("--session session-owned"));
        assert!(text.contains("browserConnected true"));
        assert!(text.contains("SHA256SUMS"));
        assert!(text.contains("Do not select another session"));
        assert!(!text.contains("pairingToken"));
        assert!(!text.contains("browserToken"));
        assert!(!text.contains("agentToken"));
        assert!(!text.contains("127.0.0.1"));
        assert!(instruction("invalid\ninstruction").is_err());
    }
}
