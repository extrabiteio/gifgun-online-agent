use std::ffi::OsStr;
use std::path::Path;
use std::process::{Child, Command, Stdio};

use crate::error::AgentResult;
use crate::platform;

pub fn spawn_owned_process<S: AsRef<OsStr>>(
    executable: &Path,
    arguments: &[S],
    log_path: &Path,
) -> AgentResult<Child> {
    let stdout = platform::open_private_append(log_path)?;
    let stderr = stdout.try_clone()?;
    let mut command = Command::new(executable);
    command
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr));
    platform::configure_detached(&mut command);
    Ok(command.spawn()?)
}

pub fn start_bridge_process(
    executable: &Path,
    startup_path: &Path,
    log_path: &Path,
) -> AgentResult<Child> {
    spawn_owned_process(
        executable,
        &[
            OsStr::new("serve"),
            OsStr::new("--startup"),
            startup_path.as_os_str(),
        ],
        log_path,
    )
}

pub fn stop_owned_process(child: &mut Child) -> AgentResult<()> {
    if child.try_wait()?.is_some() {
        return Ok(());
    }
    child.kill()?;
    child.wait()?;
    Ok(())
}
