use std::fs;

use gifgun_agent::session::{
    AgentSessionRecord, SessionStatus, SessionStore, StartupRecord, random_secret, secrets_equal,
};
use serde_json::json;
use tempfile::tempdir;

fn session() -> AgentSessionRecord {
    AgentSessionRecord::new(
        "session-private".to_owned(),
        43_115,
        "private-agent-token".to_owned(),
    )
}

#[test]
fn stores_selects_updates_and_removes_private_sessions() {
    let directory = tempdir().unwrap();
    let store = SessionStore::at(directory.path());
    let mut record = session();

    store.write_session(&record).unwrap();
    store.set_current(&record.session_id).unwrap();
    assert_eq!(store.current_session_id().unwrap(), record.session_id);
    assert_eq!(store.read_session(None).unwrap(), record);

    record.status = SessionStatus::Connected;
    record.pid = Some(1234);
    store.write_session(&record).unwrap();
    assert_eq!(store.read_session(None).unwrap(), record);

    let serialized = fs::read_to_string(store.session_path(&record.session_id)).unwrap();
    assert!(serialized.contains("private-agent-token"));
    assert!(!serialized.contains("pairingToken"));
    assert!(!format!("{record:?}").contains("private-agent-token"));

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(store.session_path(&record.session_id))
                .unwrap()
                .permissions()
                .mode()
                & 0o077,
            0
        );
    }

    store.remove_session(&record.session_id).unwrap();
    assert!(store.read_session(None).is_err());
}

#[test]
fn expired_session_fails_closed_and_clears_selection() {
    let directory = tempdir().unwrap();
    let store = SessionStore::at(directory.path());
    let mut record = session();
    record.expires_at = 0;
    store.write_session(&record).unwrap();
    store.set_current(&record.session_id).unwrap();

    assert!(store.read_session(None).is_err());
    assert!(!store.session_path(&record.session_id).exists());
    assert!(!store.current_path().exists());
}

#[test]
fn startup_handoff_is_one_use_and_separate_from_durable_session() {
    let directory = tempdir().unwrap();
    let store = SessionStore::at(directory.path());
    let startup = StartupRecord {
        envelope: json!({"pairingToken": "one-use-pairing-token"}),
        session: session(),
    };

    let path = store.write_startup(&startup).unwrap();
    assert!(!format!("{startup:?}").contains("one-use-pairing-token"));
    let consumed = store.consume_startup(&path).unwrap();
    assert_eq!(consumed, startup);
    assert!(!path.exists());
    assert!(store.consume_startup(&path).is_err());
}

#[test]
fn secrets_are_random_url_safe_and_compared_without_exposure() {
    let first = random_secret(32).unwrap();
    let second = random_secret(32).unwrap();
    assert_ne!(first, second);
    assert!(
        first
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "-_".contains(character))
    );
    assert!(secrets_equal(Some(&first), Some(&first)));
    assert!(!secrets_equal(Some(&first), Some(&second)));
    assert!(!secrets_equal(Some(&first), None));
}

#[test]
fn refuses_traversal_and_arbitrary_startup_files() {
    let directory = tempdir().unwrap();
    let store = SessionStore::at(directory.path().join("private"));
    assert!(store.read_session(Some("../outside")).is_err());
    assert!(store.set_current("../outside").is_err());

    let outside = directory.path().join("outside.startup.json");
    fs::write(&outside, "{}\n").unwrap();
    assert!(store.consume_startup(&outside).is_err());
    assert!(outside.exists());
}

#[test]
fn corrupt_or_missing_session_state_fails_closed() {
    let directory = tempdir().unwrap();
    let store = SessionStore::at(directory.path());
    store.ensure().unwrap();
    fs::write(store.current_path(), "missing\n").unwrap();
    assert!(store.read_session(None).is_err());

    fs::write(store.session_path("broken"), "{not-json").unwrap();
    assert!(store.read_session(Some("broken")).is_err());
}
