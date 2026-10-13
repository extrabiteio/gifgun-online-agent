#![cfg(windows)]

use std::fs::OpenOptions;
use std::io::{BufRead, BufReader};
use std::os::windows::io::AsRawHandle;
use std::os::windows::process::CommandExt;
use std::process::{Child, Command, Stdio};

use gifgun_agent::session::SessionStore;
use tempfile::tempdir;
use windows_sys::Win32::System::Console::{
    CTRL_C_EVENT, GenerateConsoleCtrlEvent, GetConsoleProcessList, INPUT_RECORD, INPUT_RECORD_0,
    KEY_EVENT, KEY_EVENT_RECORD, KEY_EVENT_RECORD_0, SetConsoleCtrlHandler, WriteConsoleInputW,
};
use windows_sys::Win32::System::Threading::CREATE_NEW_CONSOLE;

const MODE: &str = "GIFGUN_TEST_CONSOLE_MODE";

struct OwnedChild(Child);
impl Drop for OwnedChild {
    fn drop(&mut self) {
        if matches!(self.0.try_wait(), Ok(None)) {
            let _ = self.0.kill();
        }
        let _ = self.0.wait();
    }
}

fn check_console(mode: &str) {
    // The helper gets its own console. Never attach to or signal the test runner's console.
    let mut helper = OwnedChild(
        Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "windows_console_helper", "--nocapture"])
            .env(MODE, mode)
            .creation_flags(CREATE_NEW_CONSOLE)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap(),
    );
    assert!(
        helper.0.wait().unwrap().success(),
        "The private console check failed."
    );
}

#[test]
fn windows_enter_does_not_stop_a_bridge_but_ctrl_c_does() {
    check_console("running");
}

#[test]
fn windows_stopped_console_uses_ctrl_c_instead_of_enter() {
    check_console("stopped");
}

#[test]
fn windows_error_console_uses_ctrl_c_instead_of_enter() {
    check_console("failed");
}

unsafe extern "system" fn keep_helper_open(event: u32) -> i32 {
    i32::from(event == CTRL_C_EVENT)
}

fn read_until(output: &mut impl BufRead, expected: &str, session: &mut Option<String>) {
    let mut line = String::new();
    loop {
        line.clear();
        assert_ne!(
            output.read_line(&mut line).unwrap(),
            0,
            "The console closed before its next state."
        );
        if let Some(id) = line.trim().strip_prefix("Workspace: ") {
            *session = Some(id.to_owned());
        }
        if line.trim() == expected {
            break;
        }
    }
}

#[test]
fn windows_console_helper() {
    let Ok(mode) = std::env::var(MODE) else {
        return;
    };
    assert!(matches!(mode.as_str(), "running" | "stopped" | "failed"));
    assert_ne!(
        unsafe { SetConsoleCtrlHandler(Some(keep_helper_open), 1) },
        0
    );
    // Clear an inherited ignore flag only inside this private helper process.
    assert_ne!(unsafe { SetConsoleCtrlHandler(None, 0) }, 0);
    let directory = tempdir().unwrap();
    let input = OpenOptions::new()
        .read(true)
        .write(true)
        .open("CONIN$")
        .unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_gifgun-agent"));
    command.args(["launch", "--no-browser"]);
    if mode == "failed" {
        command.args(["--app-url", "https://other.test/app"]);
    }
    let mut console = OwnedChild(
        command
            .env("GIFGUN_AGENT_CACHE_DIR", directory.path())
            .stdin(Stdio::from(input.try_clone().unwrap()))
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let mut output = BufReader::new(console.0.stdout.take().unwrap());
    let mut session = None;
    if mode == "failed" {
        read_until(
            &mut output,
            "Press Ctrl+C or close this console.",
            &mut session,
        );
    } else {
        read_until(&mut output, "Waiting for the editor.", &mut session);
    }
    let run = |operation: &str, session: &str| {
        Command::new(env!("CARGO_BIN_EXE_gifgun-agent"))
            .args(["--session", session, operation])
            .env("GIFGUN_AGENT_CACHE_DIR", directory.path())
            .output()
            .unwrap()
    };
    if mode == "stopped" {
        assert!(
            run("disconnect", session.as_deref().unwrap())
                .status
                .success()
        );
        read_until(
            &mut output,
            "Press Ctrl+C or close this console.",
            &mut session,
        );
    }
    let events = [1, 0].map(|pressed| INPUT_RECORD {
        EventType: KEY_EVENT as u16,
        Event: INPUT_RECORD_0 {
            KeyEvent: KEY_EVENT_RECORD {
                bKeyDown: pressed,
                wRepeatCount: 1,
                // VK_RETURN has the carriage-return value.
                wVirtualKeyCode: u16::from(b'\r'),
                uChar: KEY_EVENT_RECORD_0 {
                    UnicodeChar: u16::from(b'\r'),
                },
                ..Default::default()
            },
        },
    });
    let mut written = 0;
    assert_ne!(
        unsafe {
            WriteConsoleInputW(
                input.as_raw_handle(),
                events.as_ptr(),
                events.len() as u32,
                &mut written,
            )
        },
        0
    );
    assert_eq!(written as usize, events.len());
    if mode == "running" {
        assert!(run("status", session.as_deref().unwrap()).status.success());
    }
    assert!(console.0.try_wait().unwrap().is_none());
    // Only this helper and its owned launcher may receive the control event.
    let mut clients = [0u32; 2];
    assert_eq!(
        unsafe { GetConsoleProcessList(clients.as_mut_ptr(), clients.len() as u32) } as usize,
        clients.len()
    );
    assert!(clients.contains(&std::process::id()) && clients.contains(&console.0.id()));
    assert_ne!(unsafe { GenerateConsoleCtrlEvent(CTRL_C_EVENT, 0) }, 0);
    let status = console.0.wait().unwrap();
    assert_eq!(status.code(), Some(if mode == "failed" { 1 } else { 0 }));
    if let Some(session) = session {
        assert!(
            !SessionStore::at(directory.path())
                .session_path(&session)
                .exists()
        );
    }
}
