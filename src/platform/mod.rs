use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::Path;
use std::process::Command;

#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;

pub fn create_private_dir(path: &Path) -> io::Result<()> {
    fs::create_dir_all(path)?;
    set_private_dir_permissions(path)
}

pub fn open_private_new(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    configure_private_file(&mut options);
    let file = options.open(path)?;
    set_private_file_permissions(path)?;
    Ok(file)
}

pub fn open_private_write(path: &Path) -> io::Result<File> {
    if let Some(parent) = path.parent() {
        create_private_dir(parent)?;
    }
    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(true);
    configure_private_file(&mut options);
    let file = options.open(path)?;
    set_private_file_permissions(path)?;
    Ok(file)
}

pub fn open_private_append(path: &Path) -> io::Result<File> {
    if let Some(parent) = path.parent() {
        create_private_dir(parent)?;
    }
    let mut options = OpenOptions::new();
    options.write(true).create(true).append(true);
    configure_private_file(&mut options);
    let file = options.open(path)?;
    set_private_file_permissions(path)?;
    Ok(file)
}

pub fn replace_file(source: &Path, destination: &Path) -> io::Result<()> {
    fs::rename(source, destination)
}

pub fn configure_detached(command: &mut Command) {
    configure_detached_process(command);
}

#[cfg(unix)]
use unix::{
    configure_detached_process, configure_private_file, set_private_dir_permissions,
    set_private_file_permissions,
};
#[cfg(windows)]
use windows::{
    configure_detached_process, configure_private_file, set_private_dir_permissions,
    set_private_file_permissions,
};
