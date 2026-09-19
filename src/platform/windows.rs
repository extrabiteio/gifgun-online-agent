use std::fs::OpenOptions;
use std::io;
use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::Command;

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

pub(super) fn configure_detached_process(command: &mut Command) {
    command.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
}
