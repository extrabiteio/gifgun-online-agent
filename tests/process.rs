use std::fs;
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};

use gifgun_agent::session::{AgentSessionRecord, SessionStore, StartupRecord};
use serde_json::{Value, json};
use std::time::Duration;

use gifgun_agent::process::spawn_owned_process;
use tempfile::tempdir;

#[test]
fn managed_startup_stops_when_its_owner_closes_before_confirmation() {
    let directory = tempdir().unwrap();
    let store = SessionStore::at(directory.path());
    let mut session = AgentSessionRecord::new(
        "session-owned".to_owned(),
        0,
        "synthetic-agent-token".to_owned(),
    );
    session.workspace_id = Some("workspace-owned".to_owned());
    store.write_session(&session).unwrap();
    let startup = store
        .write_startup(&StartupRecord {
            envelope: Value::Null,
            session: session.clone(),
            launch_origin: Some("https://gifgun.test".to_owned()),
        })
        .unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_gifgun-agent"))
        .arg("serve")
        .arg("--startup")
        .arg(startup)
        .arg("--ready")
        .env("GIFGUN_AGENT_CACHE_DIR", directory.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let ready: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(ready["sessionId"], json!(session.session_id));
    let port = ready["port"].as_u64().unwrap() as u16;
    drop(child.stdin.take());
    let result = child.wait_with_output().unwrap();
    assert!(!result.status.success());
    assert!(!store.session_path(&session.session_id).exists());
    assert!(std::net::TcpStream::connect(("127.0.0.1", port)).is_err());
}

#[test]
fn spawns_an_owned_detached_child_with_private_logs() {
    let directory = tempdir().unwrap();
    let log = directory.path().join("child.log");
    let executable = std::env::current_exe().unwrap();

    let mut child = spawn_owned_process(&executable, &["--list"], &log).unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        if child.try_wait().unwrap().is_some() {
            break;
        }
        assert!(std::time::Instant::now() < deadline, "child did not exit");
        std::thread::sleep(Duration::from_millis(25));
    }

    let output = fs::read_to_string(&log).unwrap();
    assert!(output.contains("spawns_an_owned_detached_child"));

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(fs::metadata(log).unwrap().permissions().mode() & 0o077, 0);
    }
}
