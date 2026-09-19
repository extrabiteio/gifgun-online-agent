use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::Serialize;
use tokio::fs::File;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};

use crate::error::{AgentError, AgentResult};
use crate::platform;
use crate::session::random_secret;

const DEFAULT_TRANSFER_LIFETIME: Duration = Duration::from_secs(10 * 60);
const DEFAULT_MAX_RESERVATIONS: usize = 64;

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UploadMetadata {
    pub transfer_id: String,
    pub name: String,
    pub size: u64,
    #[serde(rename = "type")]
    pub content_type: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OutputMetadata {
    pub transfer_id: String,
    pub overwrite: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct OutputCompletion {
    pub size: u64,
}

pub struct UploadStream {
    pub metadata: UploadMetadata,
    pub file: File,
}

#[derive(Debug)]
struct UploadReservation {
    metadata: UploadMetadata,
    path: PathBuf,
    expires_at: Instant,
}

#[derive(Debug)]
struct OutputReservation {
    metadata: OutputMetadata,
    path: PathBuf,
    expires_at: Instant,
}

#[derive(Debug)]
enum Reservation {
    Upload(UploadReservation),
    Output(OutputReservation),
}

impl Reservation {
    fn expires_at(&self) -> Instant {
        match self {
            Self::Upload(reservation) => reservation.expires_at,
            Self::Output(reservation) => reservation.expires_at,
        }
    }
}

#[derive(Debug)]
pub struct TransferManager {
    reservations: Mutex<HashMap<String, Reservation>>,
    lifetime: Duration,
    max_reservations: usize,
}

impl Default for TransferManager {
    fn default() -> Self {
        Self::with_limits(DEFAULT_TRANSFER_LIFETIME, DEFAULT_MAX_RESERVATIONS)
    }
}

impl TransferManager {
    pub fn with_limits(lifetime: Duration, max_reservations: usize) -> Self {
        Self {
            reservations: Mutex::new(HashMap::new()),
            lifetime,
            max_reservations: max_reservations.max(1),
        }
    }

    pub async fn reserve_upload(&self, path: impl AsRef<Path>) -> AgentResult<UploadMetadata> {
        let resolved = tokio::fs::canonicalize(path.as_ref()).await?;
        let details = tokio::fs::metadata(&resolved).await?;
        if !details.is_file() {
            return Err(AgentError::Transfer(
                "the upload path is not a regular file".into(),
            ));
        }
        let name = display_name(resolved.file_name())?;
        let metadata = UploadMetadata {
            transfer_id: random_secret(18)?,
            content_type: mime_guess::from_path(&resolved)
                .first_or_octet_stream()
                .essence_str()
                .to_owned(),
            name,
            size: details.len(),
        };
        self.insert(
            metadata.transfer_id.clone(),
            Reservation::Upload(UploadReservation {
                metadata: metadata.clone(),
                path: resolved,
                expires_at: Instant::now() + self.lifetime,
            }),
        )?;
        Ok(metadata)
    }

    pub async fn reserve_output(
        &self,
        path: impl AsRef<Path>,
        overwrite: bool,
    ) -> AgentResult<OutputMetadata> {
        let requested = absolute_path(path.as_ref())?;
        let name = requested
            .file_name()
            .map(OsString::from)
            .ok_or_else(|| AgentError::Transfer("an output filename is required".into()))?;
        let parent = requested
            .parent()
            .ok_or_else(|| AgentError::Transfer("an output parent is required".into()))?;
        let canonical_parent = tokio::fs::canonicalize(parent).await?;
        if !tokio::fs::metadata(&canonical_parent).await?.is_dir() {
            return Err(AgentError::Transfer(
                "the output parent is not a directory".into(),
            ));
        }
        let destination = canonical_parent.join(name);
        if !overwrite && tokio::fs::try_exists(&destination).await? {
            return Err(AgentError::Transfer(
                "the output file already exists; use explicit overwrite".into(),
            ));
        }
        let metadata = OutputMetadata {
            transfer_id: random_secret(18)?,
            overwrite,
        };
        self.insert(
            metadata.transfer_id.clone(),
            Reservation::Output(OutputReservation {
                metadata: metadata.clone(),
                path: destination,
                expires_at: Instant::now() + self.lifetime,
            }),
        )?;
        Ok(metadata)
    }

    pub async fn take_upload(&self, transfer_id: &str) -> AgentResult<UploadStream> {
        let reservation = self.take(transfer_id, Direction::Upload)?;
        let Reservation::Upload(upload) = reservation else {
            unreachable!("direction was checked before removal")
        };
        let file = File::open(&upload.path).await?;
        let details = file.metadata().await?;
        if !details.is_file() || details.len() != upload.metadata.size {
            return Err(AgentError::Transfer(
                "the reserved upload changed before transfer".into(),
            ));
        }
        Ok(UploadStream {
            metadata: upload.metadata,
            file,
        })
    }

    pub async fn write_output<R>(
        &self,
        transfer_id: &str,
        source: R,
        expected_size: u64,
    ) -> AgentResult<OutputCompletion>
    where
        R: AsyncRead + Unpin,
    {
        let reservation = self.take(transfer_id, Direction::Output)?;
        let Reservation::Output(output) = reservation else {
            unreachable!("direction was checked before removal")
        };
        let temporary = temporary_output_path(&output.path, transfer_id)?;
        let result = async {
            let standard_file = platform::open_private_new(&temporary)?;
            let mut destination = File::from_std(standard_file);
            let maximum = expected_size
                .checked_add(1)
                .ok_or_else(|| AgentError::Transfer("declared output is too large".into()))?;
            let mut limited = source.take(maximum);
            let written = tokio::io::copy(&mut limited, &mut destination).await?;
            destination.flush().await?;
            drop(destination);
            if written != expected_size {
                return Err(AgentError::Transfer(format!(
                    "rendered output was truncated ({written} of {expected_size} bytes)"
                )));
            }
            publish_output(&temporary, &output.path, output.metadata.overwrite)?;
            Ok(OutputCompletion { size: written })
        }
        .await;
        if result.is_err() {
            let _ = tokio::fs::remove_file(&temporary).await;
        }
        result
    }

    pub fn cancel(&self, transfer_id: &str) -> bool {
        self.reservations
            .lock()
            .map(|mut reservations| reservations.remove(transfer_id).is_some())
            .unwrap_or(false)
    }

    pub fn clear(&self) {
        if let Ok(mut reservations) = self.reservations.lock() {
            reservations.clear();
        }
    }

    fn insert(&self, id: String, reservation: Reservation) -> AgentResult<()> {
        let mut reservations = self
            .reservations
            .lock()
            .map_err(|_| AgentError::Transfer("reservation state is unavailable".into()))?;
        let now = Instant::now();
        reservations.retain(|_, known| known.expires_at() > now);
        if reservations.len() >= self.max_reservations {
            return Err(AgentError::Transfer(
                "too many local transfers are already reserved".into(),
            ));
        }
        reservations.insert(id, reservation);
        Ok(())
    }

    fn take(&self, transfer_id: &str, direction: Direction) -> AgentResult<Reservation> {
        let mut reservations = self
            .reservations
            .lock()
            .map_err(|_| AgentError::Transfer("reservation state is unavailable".into()))?;
        let now = Instant::now();
        reservations.retain(|_, known| known.expires_at() > now);
        let known = reservations.get(transfer_id).ok_or_else(|| {
            AgentError::Transfer("the transfer reservation is unavailable".into())
        })?;
        let matches = matches!(
            (direction, known),
            (Direction::Upload, Reservation::Upload(_))
                | (Direction::Output, Reservation::Output(_))
        );
        if !matches {
            return Err(AgentError::Transfer(
                "the transfer reservation has the wrong direction".into(),
            ));
        }
        Ok(reservations
            .remove(transfer_id)
            .expect("reservation existed while locked"))
    }
}

#[derive(Clone, Copy)]
enum Direction {
    Upload,
    Output,
}

fn display_name(name: Option<&std::ffi::OsStr>) -> AgentResult<String> {
    let name = name
        .map(|value| value.to_string_lossy().into_owned())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| AgentError::Transfer("the local file has no filename".into()))?;
    Ok(name)
}

fn absolute_path(path: &Path) -> AgentResult<PathBuf> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}

fn temporary_output_path(destination: &Path, transfer_id: &str) -> AgentResult<PathBuf> {
    let parent = destination
        .parent()
        .ok_or_else(|| AgentError::Transfer("the output parent is unavailable".into()))?;
    let name = display_name(destination.file_name())?;
    Ok(parent.join(format!(".{name}.{transfer_id}.part")))
}

fn publish_output(temporary: &Path, destination: &Path, overwrite: bool) -> AgentResult<()> {
    if overwrite {
        platform::replace_file(temporary, destination)?;
    } else {
        std::fs::hard_link(temporary, destination)?;
        std::fs::remove_file(temporary)?;
    }
    Ok(())
}
