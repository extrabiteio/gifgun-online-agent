use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::process::{Command, Stdio};
use std::sync::mpsc;

use gifgun_agent::session::{AgentSessionRecord, SessionStore};
use tempfile::tempdir;

#[test]
fn a_late_status_reply_does_not_restore_a_removed_session() {
    let directory = tempdir().unwrap();
    let store = SessionStore::at(directory.path());
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let session = AgentSessionRecord::new(
        "session-stopped".into(),
        listener.local_addr().unwrap().port(),
        "synthetic-agent-token".into(),
    );
    store.write_session(&session).unwrap();
    let (received, request) = mpsc::channel();
    let (release, released) = mpsc::channel();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            if line == "\r\n" {
                break;
            }
        }
        received.send(()).unwrap();
        released.recv().unwrap();
        let body =
            r#"{"sessionId":"session-stopped","status":"connected","browserConnected":true}"#;
        write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
    });
    let child = Command::new(env!("CARGO_BIN_EXE_gifgun-agent"))
        .args(["--session", &session.session_id, "status"])
        .env("GIFGUN_AGENT_CACHE_DIR", directory.path())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    request.recv().unwrap();
    store.remove_session(&session.session_id).unwrap();
    release.send(()).unwrap();
    assert!(child.wait_with_output().unwrap().status.success());
    server.join().unwrap();
    assert!(!store.session_path(&session.session_id).exists());
}
