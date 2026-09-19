use std::fs::OpenOptions;
use std::io;
use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command};

use windows_sys::Win32::Foundation::{
    GetHandleInformation, HANDLE, HANDLE_FLAG_INHERIT, INVALID_HANDLE_VALUE, SetHandleInformation,
};
use windows_sys::Win32::System::Console::{
    GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
};
use windows_sys::Win32::System::Threading::{
    CREATE_NEW_PROCESS_GROUP, CREATE_NO_WINDOW, DETACHED_PROCESS,
};

pub(super) fn configure_private_file(_options: &mut OpenOptions) {}

pub(super) fn set_private_file_permissions(_path: &Path) -> io::Result<()> {
    Ok(())
}

pub(super) fn set_private_dir_permissions(_path: &Path) -> io::Result<()> {
    Ok(())
}

pub(super) fn spawn_detached_process(command: &mut Command) -> io::Result<Child> {
    command.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
    // Do not let the bridge inherit CLI capture pipes and keep the CLI open.
    let _inheritance = StandardHandleInheritance::disable()?;
    command.spawn()
}

struct StandardHandleInheritance {
    handles: Vec<(HANDLE, u32)>,
}

impl StandardHandleInheritance {
    fn disable() -> io::Result<Self> {
        let mut guard = Self {
            handles: Vec::new(),
        };
        for standard in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
            // SAFETY: GetStdHandle reads the calling process standard-handle table.
            let handle = unsafe { GetStdHandle(standard) };
            if handle.is_null() || handle == INVALID_HANDLE_VALUE {
                continue;
            }

            let mut flags = 0;
            // SAFETY: `flags` is writable, and `handle` was returned for this process.
            if unsafe { GetHandleInformation(handle, &mut flags) } == 0 {
                return Err(io::Error::last_os_error());
            }
            if flags & HANDLE_FLAG_INHERIT == 0 {
                continue;
            }

            // SAFETY: The mask changes only the inheritance bit on this process handle.
            if unsafe { SetHandleInformation(handle, HANDLE_FLAG_INHERIT, 0) } == 0 {
                return Err(io::Error::last_os_error());
            }
            guard.handles.push((handle, flags));
        }
        Ok(guard)
    }
}

impl Drop for StandardHandleInheritance {
    fn drop(&mut self) {
        for (handle, flags) in self.handles.drain(..) {
            // SAFETY: The handle remains owned by this process. Restore its prior bit.
            unsafe {
                SetHandleInformation(handle, HANDLE_FLAG_INHERIT, flags & HANDLE_FLAG_INHERIT);
            }
        }
    }
}
