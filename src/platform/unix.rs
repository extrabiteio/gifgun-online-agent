use std::fs::{self, OpenOptions, Permissions};
use std::io;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::Command;

pub(super) fn configure_private_file(options: &mut OpenOptions) {
    options.mode(0o600);
}

pub(super) fn set_private_file_permissions(path: &Path) -> io::Result<()> {
    fs::set_permissions(path, Permissions::from_mode(0o600))
}

pub(super) fn set_private_dir_permissions(path: &Path) -> io::Result<()> {
    fs::set_permissions(path, Permissions::from_mode(0o700))
}

pub(super) fn configure_detached_process(command: &mut Command) {
    command.process_group(0);
}
