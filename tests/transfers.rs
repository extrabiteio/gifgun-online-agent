use std::io::Cursor;
use std::time::Duration;

use gifgun_agent::transfers::TransferManager;
use tempfile::tempdir;
use tokio::fs;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[tokio::test]
async fn reserves_and_streams_local_upload_without_exposing_path_or_bytes() {
    let directory = tempdir().unwrap();
    let source = directory.path().join("source.mp4");
    let bytes = b"private-video-byte-sentinel";
    fs::write(&source, bytes).await.unwrap();
    let manager = TransferManager::default();

    let metadata = manager.reserve_upload(&source).await.unwrap();
    assert_eq!(metadata.name, "source.mp4");
    assert_eq!(metadata.size, bytes.len() as u64);
    assert_eq!(metadata.content_type, "video/mp4");
    let serialized = serde_json::to_string(&metadata).unwrap();
    assert!(serialized.contains("\"type\":\"video/mp4\""));
    assert!(!serialized.contains(source.to_string_lossy().as_ref()));
    assert!(!serialized.contains("private-video-byte-sentinel"));

    let mut upload = manager.take_upload(&metadata.transfer_id).await.unwrap();
    let mut contents = Vec::new();
    upload.file.read_to_end(&mut contents).await.unwrap();
    assert_eq!(contents, bytes);
    assert!(manager.take_upload(&metadata.transfer_id).await.is_err());
}

#[tokio::test]
async fn writes_output_atomically_and_requires_explicit_overwrite() {
    let directory = tempdir().unwrap();
    let destination = directory.path().join("result.gif");
    let manager = TransferManager::default();
    let first = b"rendered-gif-byte-sentinel";

    let reservation = manager.reserve_output(&destination, false).await.unwrap();
    let completed = manager
        .write_output(
            &reservation.transfer_id,
            Cursor::new(first),
            first.len() as u64,
        )
        .await
        .unwrap();
    assert_eq!(completed.size, first.len() as u64);
    assert_eq!(fs::read(&destination).await.unwrap(), first);
    assert!(
        !serde_json::to_string(&completed)
            .unwrap()
            .contains(destination.to_string_lossy().as_ref())
    );

    assert!(manager.reserve_output(&destination, false).await.is_err());
    let replacement = b"replacement";
    let overwrite = manager.reserve_output(&destination, true).await.unwrap();
    manager
        .write_output(
            &overwrite.transfer_id,
            Cursor::new(replacement),
            replacement.len() as u64,
        )
        .await
        .unwrap();
    assert_eq!(fs::read(&destination).await.unwrap(), replacement);
}

#[tokio::test]
async fn truncated_or_failed_output_leaves_existing_destination_intact() {
    let directory = tempdir().unwrap();
    let destination = directory.path().join("result.webm");
    fs::write(&destination, b"prior-result").await.unwrap();
    let manager = TransferManager::default();
    let reservation = manager.reserve_output(&destination, true).await.unwrap();

    assert!(
        manager
            .write_output(&reservation.transfer_id, Cursor::new(b"tiny"), 20)
            .await
            .is_err()
    );
    assert_eq!(fs::read(&destination).await.unwrap(), b"prior-result");
    let entries: Vec<_> = std::fs::read_dir(directory.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(entries, vec![std::ffi::OsString::from("result.webm")]);
}

#[tokio::test]
async fn streams_large_and_empty_outputs_without_whole_file_buffering() {
    let directory = tempdir().unwrap();
    let manager = TransferManager::default();

    let empty_path = directory.path().join("empty.gif");
    let empty = manager.reserve_output(&empty_path, false).await.unwrap();
    manager
        .write_output(&empty.transfer_id, Cursor::new(Vec::<u8>::new()), 0)
        .await
        .unwrap();
    assert_eq!(fs::metadata(&empty_path).await.unwrap().len(), 0);

    let output = directory.path().join("large.webm");
    let reservation = manager.reserve_output(&output, false).await.unwrap();
    let total = 4 * 1024 * 1024_u64;
    let (mut writer, reader) = tokio::io::duplex(1024);
    let producer = tokio::spawn(async move {
        let chunk = vec![0x5a; 16 * 1024];
        for _ in 0..(total / chunk.len() as u64) {
            writer.write_all(&chunk).await.unwrap();
        }
        writer.shutdown().await.unwrap();
    });
    let completed = manager
        .write_output(&reservation.transfer_id, reader, total)
        .await
        .unwrap();
    producer.await.unwrap();
    assert_eq!(completed.size, total);
    assert_eq!(fs::metadata(output).await.unwrap().len(), total);
}

#[tokio::test]
async fn expires_bounds_and_cancels_one_use_reservations() {
    let directory = tempdir().unwrap();
    let source = directory.path().join("image.png");
    fs::write(&source, b"png").await.unwrap();
    let manager = TransferManager::with_limits(Duration::from_millis(20), 1);

    let first = manager.reserve_upload(&source).await.unwrap();
    assert!(manager.reserve_upload(&source).await.is_err());
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert!(manager.take_upload(&first.transfer_id).await.is_err());

    let second = manager.reserve_upload(&source).await.unwrap();
    assert!(manager.cancel(&second.transfer_id));
    assert!(manager.take_upload(&second.transfer_id).await.is_err());
}

#[tokio::test]
async fn rejects_directories_missing_parents_and_wrong_transfer_directions() {
    let directory = tempdir().unwrap();
    let manager = TransferManager::default();
    assert!(manager.reserve_upload(directory.path()).await.is_err());
    assert!(
        manager
            .reserve_output(&directory.path().join("missing/result.gif"), false)
            .await
            .is_err()
    );

    let source = directory.path().join("image.png");
    fs::write(&source, b"png").await.unwrap();
    let upload = manager.reserve_upload(&source).await.unwrap();
    assert!(
        manager
            .write_output(&upload.transfer_id, Cursor::new(b"png"), 3)
            .await
            .is_err()
    );
}
