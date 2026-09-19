use std::fs;
use std::time::Duration;

use gifgun_agent::process::spawn_owned_process;
use tempfile::tempdir;

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
