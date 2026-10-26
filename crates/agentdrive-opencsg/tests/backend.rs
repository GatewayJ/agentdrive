mod support;
use agentdrive_model::*;
use agentdrive_ports::*;
use bytes::Bytes;
use sha2::Digest;
use support::Server;
use tokio::io::AsyncReadExt;

fn stream(bytes: Vec<u8>) -> ObjectStream {
    ObjectStream {
        reference: object_ref(ObjectKind::File, &bytes),
        reader: Box::pin(std::io::Cursor::new(bytes)),
    }
}

#[tokio::test]
async fn binary_objects_survive_chunking_restart_and_repeated_uploads() {
    let server = Server::new().await;
    let backend = server.backend();
    let workspace = WorkspaceId::new();
    let bytes: Vec<u8> = (0..4 * 1024 * 1024 + 37)
        .map(|index| (index % 256) as u8)
        .collect();
    let reference = object_ref(ObjectKind::File, &bytes);
    assert!(!backend.contains(workspace, &reference).await.unwrap());
    backend
        .upload(workspace, stream(bytes.clone()))
        .await
        .unwrap();
    assert!(backend.contains(workspace, &reference).await.unwrap());
    let before = server.data.lock().unwrap().generation;
    backend
        .upload(workspace, stream(bytes.clone()))
        .await
        .unwrap();
    assert_eq!(server.data.lock().unwrap().generation, before);
    let second = server.backend();
    let mut restored = Vec::new();
    second
        .download(workspace, &reference)
        .await
        .unwrap()
        .reader
        .read_to_end(&mut restored)
        .await
        .unwrap();
    assert_eq!(restored, bytes);
    let empty = stream(Vec::new());
    let reference = empty.reference.clone();
    backend.upload(workspace, empty).await.unwrap();
    let mut output = Vec::new();
    second
        .download(workspace, &reference)
        .await
        .unwrap()
        .reader
        .read_to_end(&mut output)
        .await
        .unwrap();
    assert!(output.is_empty());
}

#[tokio::test]
async fn invalid_uploads_do_not_publish_or_poison_object_identities() {
    let server = Server::new().await;
    let backend = server.backend();
    let workspace = WorkspaceId::new();
    let mut invalid = stream(b"expected".to_vec());
    let reference = invalid.reference.clone();
    invalid.reader = Box::pin(std::io::Cursor::new(b"modified".to_vec()));
    assert_eq!(
        backend.upload(workspace, invalid).await.unwrap_err().code,
        ErrorCode::Integrity
    );
    assert!(!backend.contains(workspace, &reference).await.unwrap());
    backend
        .upload(workspace, stream(b"expected".to_vec()))
        .await
        .unwrap();
    {
        let mut data = server.data.lock().unwrap();
        let mut files = data.files().clone();
        let id = format!("{:x}", sha2::Sha256::digest(b"expected"));
        let chunk = files
            .keys()
            .find(|path| path.ends_with(&id))
            .unwrap()
            .clone();
        files.insert(chunk, "invalid data".into());
        data.publish(files);
    }
    let mut output = Vec::new();
    assert!(
        backend
            .download(workspace, &reference)
            .await
            .unwrap()
            .reader
            .read_to_end(&mut output)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn conditional_references_reject_stale_writers_and_recover_lost_acknowledgements() {
    let server = Server::new().await;
    let first = server.backend();
    let second = server.backend();
    first.check_connection().await.unwrap();
    second.check_connection().await.unwrap();
    let key = RefKey::Workspace(WorkspaceId::new());
    let outcome = first
        .compare_exchange(RefUpdate {
            key: key.clone(),
            expected_version: None,
            bytes: Bytes::from_static(b"initial"),
        })
        .await
        .unwrap();
    assert!(matches!(outcome, CasOutcome::Applied { .. }));
    let version = first.get(&key).await.unwrap().unwrap().version;
    let (a, b) = tokio::join!(
        first.compare_exchange(RefUpdate {
            key: key.clone(),
            expected_version: Some(version.clone()),
            bytes: Bytes::from_static(b"first")
        }),
        second.compare_exchange(RefUpdate {
            key: key.clone(),
            expected_version: Some(version),
            bytes: Bytes::from_static(b"second")
        }),
    );
    let outcomes = [a.unwrap(), b.unwrap()];
    assert_eq!(
        outcomes
            .iter()
            .filter(|v| matches!(v, CasOutcome::Applied { .. }))
            .count(),
        1
    );
    assert_eq!(
        outcomes
            .iter()
            .filter(|v| matches!(v, CasOutcome::Conflict))
            .count(),
        1
    );
    let version = first.get(&key).await.unwrap().unwrap().version;
    server.data.lock().unwrap().lose_write_response = true;
    let outcome = first
        .compare_exchange(RefUpdate {
            key: key.clone(),
            expected_version: Some(version),
            bytes: Bytes::from_static(b"confirmed"),
        })
        .await
        .unwrap();
    assert!(matches!(outcome, CasOutcome::Applied { .. }));
    assert_eq!(second.get(&key).await.unwrap().unwrap().bytes, "confirmed");

    let version = first.get(&key).await.unwrap().unwrap().version;
    server.data.lock().unwrap().lose_confirmation = true;
    let outcome = first
        .compare_exchange(RefUpdate {
            key: key.clone(),
            expected_version: Some(version),
            bytes: Bytes::from_static(b"uncertain"),
        })
        .await
        .unwrap();
    assert!(matches!(outcome, CasOutcome::Unknown));
    assert_eq!(second.get(&key).await.unwrap().unwrap().bytes, "uncertain");

    let version = first.get(&key).await.unwrap().unwrap().version;
    server.data.lock().unwrap().advance_after_write = true;
    let outcome = first
        .compare_exchange(RefUpdate {
            key: key.clone(),
            expected_version: Some(version),
            bytes: Bytes::from_static(b"intermediate"),
        })
        .await
        .unwrap();
    assert!(matches!(outcome, CasOutcome::Unknown));
    assert_eq!(second.get(&key).await.unwrap().unwrap().bytes, "advanced");
}

#[tokio::test]
async fn incompatible_services_are_rejected_before_publishing_data() {
    for (ignore_start_sha, overwrite_created, ignore_create_start_sha) in [
        (true, false, false),
        (false, true, false),
        (false, false, true),
    ] {
        let server = Server::new().await;
        {
            let mut data = server.data.lock().unwrap();
            data.ignore_start_sha = ignore_start_sha;
            data.overwrite_created = overwrite_created;
            data.ignore_create_start_sha = ignore_create_start_sha;
        }
        let error = server.backend().check_connection().await.unwrap_err();
        assert_eq!(error.code, ErrorCode::Unsupported);
    }
}

#[tokio::test]
async fn connection_probe_retries_concurrent_commits_and_rejects_inconclusive_checks() {
    let server = Server::new().await;
    server.data.lock().unwrap().probe_conflicts = 2;
    server.backend().check_connection().await.unwrap();
    assert_eq!(server.data.lock().unwrap().probe_conflicts, 0);

    let backend = server.backend();
    server.data.lock().unwrap().fail_stale_probe = true;
    assert_eq!(
        backend.check_connection().await.unwrap_err().code,
        ErrorCode::Unavailable
    );
    server.data.lock().unwrap().fail_stale_probe = false;
    backend.check_connection().await.unwrap();
}

#[tokio::test]
async fn redacted_commit_errors_preserve_conditional_write_verification() {
    let server = Server::new().await;
    server.data.lock().unwrap().redact_commit_errors = true;
    server.backend().check_connection().await.unwrap();

    for (ignore_start_sha, overwrite_created, ignore_create_start_sha) in [
        (true, false, false),
        (false, true, false),
        (false, false, true),
    ] {
        {
            let mut data = server.data.lock().unwrap();
            data.ignore_start_sha = ignore_start_sha;
            data.overwrite_created = overwrite_created;
            data.ignore_create_start_sha = ignore_create_start_sha;
        }
        assert_eq!(
            server.backend().check_connection().await.unwrap_err().code,
            ErrorCode::Unsupported
        );
    }
}

#[tokio::test]
async fn branch_discovery_reads_all_pages_and_errors_do_not_expose_tokens() {
    let server = Server::new().await;
    let backend = server.backend();
    let workspace = WorkspaceId::new();
    let mut expected = std::collections::BTreeSet::new();
    {
        let mut data = server.data.lock().unwrap();
        let mut files = data.files().clone();
        for _ in 0..235 {
            let branch = BranchId::new();
            expected.insert(branch);
            files.insert(
                format!("agentdrive/v1/{workspace}/branches/{branch}.json"),
                "{}".into(),
            );
        }
        data.publish(files);
    }
    assert_eq!(
        backend.list_branches(workspace).await.unwrap(),
        expected.into_iter().collect::<Vec<_>>()
    );
    server.data.lock().unwrap().unavailable = true;
    let error = backend
        .get(&RefKey::Workspace(workspace))
        .await
        .unwrap_err();
    assert!(!error.to_string().contains("private-test-token"));
}

#[tokio::test]
async fn credentials_are_not_forwarded_to_redirects() {
    let server = Server::new().await;
    let destination = Server::new().await;
    server.data.lock().unwrap().redirect = Some(destination.config.endpoint.clone());
    assert!(server.backend().check_connection().await.is_err());
    assert_eq!(destination.data.lock().unwrap().requests, 0);
}
