use super::*;
use crate::sampling::ConversationItem;
use crate::session::info::Info;
use crate::session::persistence::default_model_id;
use crate::session::storage::{SessionUpdate, StorageAdapter};

fn info() -> Info {
    Info {
        id: acp::SessionId::new("durable-jsonl"),
        cwd: "/test".into(),
    }
}

fn update(info: &Info, text: String) -> SessionUpdate {
    SessionUpdate::Acp(Box::new(acp::SessionNotification::new(
        info.id.clone(),
        acp::SessionUpdate::AgentMessageChunk(acp::ContentChunk::new(acp::ContentBlock::Text(
            acp::TextContent::new(text),
        ))),
    )))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ordinary_and_durable_appends_keep_every_physical_line_parseable() {
    const N: usize = 100;
    let dir = tempfile::tempdir().unwrap();
    let info = info();
    let adapter = JsonlStorageAdapter::with_explicit_session_dir(dir.path().to_path_buf());
    adapter
        .init_session(&info, default_model_id())
        .await
        .unwrap();
    let ordinary = adapter.clone();
    let durable = adapter.clone();
    let info_a = info.clone();
    let info_b = info.clone();
    let ordinary = tokio::spawn(async move {
        for index in 0..N {
            ordinary
                .append_update(&info_a, &update(&info_a, format!("ordinary-{index}")))
                .await
                .unwrap();
        }
    });
    let durable = tokio::spawn(async move {
        for index in 0..N {
            durable
                .append_update_durable_commit_aware(
                    &info_b,
                    &update(&info_b, format!("durable-{index}")),
                )
                .await
                .unwrap();
        }
    });
    ordinary.await.unwrap();
    durable.await.unwrap();

    let bytes = std::fs::read(dir.path().join("updates.jsonl")).unwrap();
    let parsed = bytes
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(serde_json::from_slice::<SessionUpdateEnvelope>)
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(parsed.len(), N * 2);
}

#[tokio::test]
async fn strip_rewrite_summary_failure_preserves_live_history() {
    for malformed in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let info = info();
        let adapter = JsonlStorageAdapter::with_explicit_session_dir(dir.path().to_path_buf());
        let chat = adapter.chat_file(&info);
        let original = br#"{"type":"user","content":[{"type":"text","text":"original without trailing newline"}]}"#;
        std::fs::write(&chat, original).unwrap();
        if malformed {
            std::fs::write(adapter.summary_file(&info), b"invalid summary").unwrap();
        }
        let error = crate::session::storage::strip_rewrite_gated(
            &adapter,
            &info,
            &[ConversationItem::user("stripped")],
        )
        .await
        .unwrap_err();
        eprintln!(
            "strip summary failure: malformed={malformed}, kind={:?}, error={error}",
            error.kind()
        );
        assert_eq!(
            error.kind(),
            if malformed {
                io::ErrorKind::InvalidData
            } else {
                io::ErrorKind::NotFound
            }
        );
        assert_eq!(std::fs::read(&chat).unwrap(), original);
        assert_eq!(
            std::fs::read(chat.with_extension("jsonl.pre-strip")).unwrap(),
            original
        );
    }
}

#[tokio::test]
async fn strip_rewrite_late_write_failure_restores_chat_and_summary() {
    for existing_chat in [true, false] {
        let dir = tempfile::tempdir().unwrap();
        let info = info();
        let adapter = JsonlStorageAdapter::with_explicit_session_dir(dir.path().to_path_buf());
        adapter
            .init_session(&info, default_model_id())
            .await
            .unwrap();
        let chat = adapter.chat_file(&info);
        let original =
            br#"{"type":"user","content":[{"type":"text","text":"current unstripped preimage"}]}"#;
        let backup = chat.with_extension("jsonl.pre-strip");
        if existing_chat {
            std::fs::write(&chat, original).unwrap();
            std::fs::write(&backup, b"oldest pre-strip backup").unwrap();
        }
        let summary_path = adapter.summary_file(&info);
        let original_summary = std::fs::read(&summary_path).unwrap();
        let messages = [
            ConversationItem::user("stripped"),
            ConversationItem::user("second"),
        ];
        let bytes = xai_chat_state::persistence::native::history_jsonl(&messages).unwrap();
        let error = adapter
            .replace_chat_history_for_strip_sync_with(
                &info,
                &bytes,
                messages.len(),
                |path, bytes| {
                    crate::session::storage::write_bytes_atomic_with(
                        path,
                        bytes,
                        crate::session::storage::sync_file_durable,
                        || Err(io::Error::other("strip live directory barrier failed")),
                    )
                },
            )
            .unwrap_err();
        assert_eq!(error.to_string(), "strip live directory barrier failed");
        if existing_chat {
            assert_eq!(std::fs::read(&chat).unwrap(), original);
            assert_eq!(std::fs::read(&backup).unwrap(), b"oldest pre-strip backup");
        } else {
            assert!(!chat.exists());
            assert!(!backup.exists());
        }
        assert_eq!(std::fs::read(&summary_path).unwrap(), original_summary);
        crate::session::storage::strip_rewrite_gated(&adapter, &info, &messages)
            .await
            .unwrap();
        assert_eq!(std::fs::read(&chat).unwrap(), bytes);
        let summary: Summary =
            serde_json::from_slice(&std::fs::read(&summary_path).unwrap()).unwrap();
        assert_eq!(summary.num_chat_messages, messages.len());
        assert_eq!(summary.chat_format_version, CHAT_FORMAT_VERSION);
        if existing_chat {
            assert_eq!(std::fs::read(&backup).unwrap(), b"oldest pre-strip backup");
        }
    }
}

#[cfg(unix)]
#[tokio::test]
async fn strip_rewrite_absent_chat_read_error_still_restores_absence() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    let info = info();
    let adapter = JsonlStorageAdapter::with_explicit_session_dir(dir.path().to_path_buf());
    adapter
        .init_session(&info, default_model_id())
        .await
        .unwrap();
    let chat = adapter.chat_file(&info);
    assert!(!chat.exists());
    let original_summary = std::fs::read(adapter.summary_file(&info)).unwrap();
    let bytes =
        xai_chat_state::persistence::native::history_jsonl(&[ConversationItem::user("stripped")])
            .unwrap();
    let error = adapter
        .replace_chat_history_for_strip_sync_with(&info, &bytes, 1, |path, bytes| {
            crate::session::storage::write_bytes_atomic(path, bytes)?;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0))?;
            let read_error = std::fs::read(path).unwrap_err();
            eprintln!("new chat rollback read: kind={:?}", read_error.kind());
            Err(io::Error::other(
                "late strip error with unreadable new chat",
            ))
        })
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        "late strip error with unreadable new chat"
    );
    assert!(
        !chat.exists(),
        "failed reads must not be mistaken for an absent preimage"
    );
    assert_eq!(
        std::fs::read(adapter.summary_file(&info)).unwrap(),
        original_summary
    );
}

#[tokio::test]
async fn append_commit_is_reported_when_bookkeeping_fails() {
    let dir = tempfile::tempdir().unwrap();
    let info = info();
    let adapter = JsonlStorageAdapter::with_explicit_session_dir(dir.path().to_path_buf());
    adapter
        .init_session(&info, default_model_id())
        .await
        .unwrap();
    let summary = dir.path().join("summary.json");
    std::fs::remove_file(&summary).unwrap();
    std::fs::create_dir(&summary).unwrap();

    assert!(matches!(
        adapter
            .append_update_durable_commit_aware(&info, &update(&info, "committed".into()))
            .await,
        Err(crate::session::storage::AppendUpdateError::Committed(_))
    ));
    assert_eq!(
        std::fs::read_to_string(dir.path().join("updates.jsonl"))
            .unwrap()
            .lines()
            .count(),
        1
    );
}

#[tokio::test]
async fn durable_append_reports_committed_when_file_barrier_fails() {
    let dir = tempfile::tempdir().unwrap();
    let info = info();
    let adapter = JsonlStorageAdapter::with_explicit_session_dir(dir.path().to_path_buf())
        .with_file_sync_probe(|| Err(io::Error::other("file barrier failed")));
    adapter
        .init_session(&info, default_model_id())
        .await
        .unwrap();

    assert!(matches!(
        adapter
            .append_update_durable_commit_aware(&info, &update(&info, "terminal".into()))
            .await,
        Err(crate::session::storage::AppendUpdateError::Committed(_))
    ));
    assert_eq!(
        std::fs::read_to_string(dir.path().join("updates.jsonl"))
            .unwrap()
            .lines()
            .count(),
        1
    );
}

#[tokio::test]
async fn chat_append_commit_is_reported_when_bookkeeping_fails() {
    let dir = tempfile::tempdir().unwrap();
    let info = info();
    let adapter = JsonlStorageAdapter::with_explicit_session_dir(dir.path().to_path_buf());
    adapter
        .init_session(&info, default_model_id())
        .await
        .unwrap();
    let summary = dir.path().join("summary.json");
    std::fs::remove_file(&summary).unwrap();
    std::fs::create_dir(&summary).unwrap();

    assert!(matches!(
        adapter
            .append_chat_message_commit_aware(&info, &ConversationItem::user("committed"))
            .await,
        Err(crate::session::storage::AppendChatError::Committed(_))
    ));
    assert_eq!(
        std::fs::read_to_string(dir.path().join("chat_history.jsonl"))
            .unwrap()
            .lines()
            .count(),
        1
    );
}

#[test]
fn directory_barrier_failure_is_retried_even_after_file_exists() {
    let mut attempts = 0;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("updates.jsonl");
    let mut flaky_parent = || {
        attempts += 1;
        if attempts == 1 {
            Err(io::Error::other("directory barrier failed"))
        } else {
            Ok(())
        }
    };
    assert!(
        JsonlStorageAdapter::append_jsonl_line_sync_with(
            &path,
            b"{\"record\":1}\n".to_vec(),
            AppendDurability::Durable,
            std::fs::File::sync_all,
            &mut flaky_parent,
        )
        .is_err()
    );
    JsonlStorageAdapter::append_jsonl_line_sync_with(
        &path,
        b"{\"record\":1}\n".to_vec(),
        AppendDurability::Durable,
        std::fs::File::sync_all,
        &mut flaky_parent,
    )
    .unwrap();
    assert_eq!(attempts, 2);
}

#[test]
fn file_barrier_error_after_write_is_committed_and_keeps_the_line() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("updates.jsonl");
    let error = JsonlStorageAdapter::append_jsonl_line_sync_with(
        &path,
        b"{\"record\":1}\n".to_vec(),
        AppendDurability::Durable,
        |_| Err(io::Error::other("file barrier failed")),
        || Ok(()),
    )
    .unwrap_err();
    assert!(
        matches!(error, AppendLineError::Committed(_)),
        "a file barrier failure after write_all must report the record as committed: {error}"
    );
    assert_eq!(error.to_string(), "file barrier failed");
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "{\"record\":1}\n",
        "the JSONL record must remain in the file so a later fsync can still make it durable"
    );
}

#[test]
fn cwd_switch_retry_after_post_append_barrier_failure_is_already_present() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("chat_history.jsonl");
    let item = ConversationItem::working_directory_switch("moved", 3);
    let mut line = serde_json::to_vec(&item).unwrap();
    line.push(b'\n');

    assert!(matches!(
        JsonlStorageAdapter::append_cwd_switch_line_sync_with(
            &path,
            line.clone(),
            3,
            |_| Err(io::Error::other("file barrier failed")),
            || Ok(()),
        ),
        Err(crate::session::storage::AppendCwdSwitchError::Committed {
            acknowledgement: xai_chat_state::StrictAppendAck::Appended,
            ..
        })
    ));
    assert!(matches!(
        JsonlStorageAdapter::append_cwd_switch_line_sync_with(
            &path,
            line,
            3,
            |_| Ok(()),
            || Ok(()),
        )
        .unwrap(),
        xai_chat_state::StrictAppendAck::AlreadyPresent(item)
            if item.text_content() == "moved"
    ));
    assert_eq!(std::fs::read_to_string(path).unwrap().lines().count(), 1);
}

#[tokio::test]
async fn cwd_switch_retry_repairs_bookkeeping_without_duplicate() {
    let dir = tempfile::tempdir().unwrap();
    let info = info();
    let adapter = JsonlStorageAdapter::with_explicit_session_dir(dir.path().to_path_buf());
    adapter
        .init_session(&info, default_model_id())
        .await
        .unwrap();
    let item = ConversationItem::working_directory_switch("moved", 4);

    let summary_path = dir.path().join("summary.json");
    let original_summary = std::fs::read(&summary_path).unwrap();
    std::fs::write(&summary_path, b"invalid summary").unwrap();
    assert!(matches!(
        adapter.append_cwd_switch_commit_aware(&info, &item).await,
        Err(crate::session::storage::AppendCwdSwitchError::Committed {
            acknowledgement: xai_chat_state::StrictAppendAck::Appended,
            ..
        })
    ));
    assert!(matches!(
        adapter
            .append_cwd_switch_commit_aware(
                &info,
                &ConversationItem::working_directory_switch("retry", 4),
            )
            .await,
        Err(crate::session::storage::AppendCwdSwitchError::Committed {
            acknowledgement: xai_chat_state::StrictAppendAck::AlreadyPresent(authoritative),
            ..
        }) if authoritative.text_content() == "moved"
    ));
    std::fs::write(&summary_path, original_summary).unwrap();
    assert_eq!(
        adapter.read_summary_sync(&info).unwrap().num_chat_messages,
        0
    );

    assert!(matches!(
        adapter
            .append_cwd_switch_commit_aware(&info, &item)
            .await
            .unwrap(),
        xai_chat_state::StrictAppendAck::AlreadyPresent(item)
            if item.text_content() == "moved"
    ));
    let summary = adapter.read_summary_sync(&info).unwrap();
    assert_eq!(summary.num_chat_messages, 1);
    assert_eq!(summary.cwd_switch_bookkeeping_generation, 4);

    assert!(matches!(
        adapter
            .append_cwd_switch_commit_aware(&info, &item)
            .await
            .unwrap(),
        xai_chat_state::StrictAppendAck::AlreadyPresent(item)
            if item.text_content() == "moved"
    ));
    let retried = adapter.read_summary_sync(&info).unwrap();
    assert_eq!(retried.num_chat_messages, 1);
    assert_eq!(retried.cwd_switch_bookkeeping_generation, 4);
    assert_eq!(
        std::fs::read_to_string(dir.path().join("chat_history.jsonl"))
            .unwrap()
            .lines()
            .count(),
        1
    );
}

#[tokio::test]
async fn cwd_switch_retained_by_history_replacement_is_not_recounted() {
    let dir = tempfile::tempdir().unwrap();
    let info = info();
    let adapter = JsonlStorageAdapter::with_explicit_session_dir(dir.path().to_path_buf());
    adapter
        .init_session(&info, default_model_id())
        .await
        .unwrap();
    let item = ConversationItem::working_directory_switch("retained", 6);

    adapter
        .replace_chat_history(&info, std::slice::from_ref(&item))
        .await
        .unwrap();
    let replaced = adapter.read_summary_sync(&info).unwrap();
    assert_eq!(replaced.num_chat_messages, 1);
    assert_eq!(replaced.cwd_switch_bookkeeping_generation, 6);

    assert!(matches!(
        adapter
            .append_cwd_switch_commit_aware(
                &info,
                &ConversationItem::working_directory_switch("retry", 6),
            )
            .await
            .unwrap(),
        xai_chat_state::StrictAppendAck::AlreadyPresent(authoritative)
            if authoritative.text_content() == "retained"
    ));
    let summary = adapter.read_summary_sync(&info).unwrap();
    assert_eq!(summary.num_chat_messages, 1);
    assert_eq!(summary.cwd_switch_bookkeeping_generation, 6);
    assert_eq!(
        adapter
            .read_chat_history_sync(adapter.chat_file(&info), CHAT_FORMAT_VERSION)
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn cwd_switch_reappend_after_history_replacement_restores_message_count() {
    let dir = tempfile::tempdir().unwrap();
    let info = info();
    let adapter = JsonlStorageAdapter::with_explicit_session_dir(dir.path().to_path_buf());
    adapter
        .init_session(&info, default_model_id())
        .await
        .unwrap();
    let item = ConversationItem::working_directory_switch("moved", 7);

    assert!(matches!(
        adapter
            .append_cwd_switch_commit_aware(&info, &item)
            .await
            .unwrap(),
        xai_chat_state::StrictAppendAck::Appended
    ));
    adapter.replace_chat_history(&info, &[]).await.unwrap();
    let replaced = adapter.read_summary_sync(&info).unwrap();
    assert_eq!(replaced.cwd_switch_bookkeeping_generation, 7);
    assert_eq!(replaced.num_chat_messages, 0);

    assert!(matches!(
        adapter
            .append_cwd_switch_commit_aware(&info, &item)
            .await
            .unwrap(),
        xai_chat_state::StrictAppendAck::Appended
    ));
    let summary = adapter.read_summary_sync(&info).unwrap();
    assert_eq!(summary.cwd_switch_bookkeeping_generation, 7);
    assert_eq!(summary.num_chat_messages, 1);
    assert_eq!(
        adapter
            .read_chat_history_sync(adapter.chat_file(&info), CHAT_FORMAT_VERSION)
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn init_session_retries_parent_sync_when_only_the_first_summary_exists() {
    let dir = tempfile::tempdir().unwrap();
    let info = info();
    JsonlStorageAdapter::with_explicit_session_dir(dir.path().to_path_buf())
        .init_session(&info, default_model_id())
        .await
        .unwrap();

    let parent_syncs = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counted = parent_syncs.clone();
    JsonlStorageAdapter::with_explicit_session_dir(dir.path().to_path_buf())
        .with_parent_sync_probe(move || {
            counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        })
        .init_session(&info, default_model_id())
        .await
        .unwrap();
    assert_eq!(parent_syncs.load(std::sync::atomic::Ordering::SeqCst), 1);
}

#[tokio::test]
async fn init_session_does_not_ack_a_first_summary_whose_parent_sync_still_fails() {
    let dir = tempfile::tempdir().unwrap();
    let info = info();
    JsonlStorageAdapter::with_explicit_session_dir(dir.path().to_path_buf())
        .init_session(&info, default_model_id())
        .await
        .unwrap();

    let error = JsonlStorageAdapter::with_explicit_session_dir(dir.path().to_path_buf())
        .with_parent_sync_probe(|| Err(io::Error::other("directory barrier failed")))
        .init_session(&info, default_model_id())
        .await
        .unwrap_err();
    assert_eq!(error.to_string(), "directory barrier failed");
}

#[tokio::test]
async fn init_session_skips_parent_sync_on_a_populated_resume() {
    let dir = tempfile::tempdir().unwrap();
    let info = info();
    JsonlStorageAdapter::with_explicit_session_dir(dir.path().to_path_buf())
        .init_session(&info, default_model_id())
        .await
        .unwrap();
    std::fs::write(dir.path().join("chat_history.jsonl"), b"").unwrap();

    let parent_syncs = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counted = parent_syncs.clone();
    JsonlStorageAdapter::with_explicit_session_dir(dir.path().to_path_buf())
        .with_parent_sync_probe(move || {
            counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        })
        .init_session(&info, default_model_id())
        .await
        .unwrap();
    assert_eq!(parent_syncs.load(std::sync::atomic::Ordering::SeqCst), 0);
}

#[cfg(target_os = "macos")]
#[test]
fn darwin_fullfsync_seam_reports_invalid_descriptor() {
    assert!(super::super::fullfsync_raw(-1).is_err());
}

struct NativePublicationBarrier {
    released: std::sync::Mutex<bool>,
    changed: std::sync::Condvar,
}
impl NativePublicationBarrier {
    fn release(&self) {
        *self.released.lock().unwrap() = true;
        self.changed.notify_all();
    }
    fn wait(&self) {
        let mut released = self.released.lock().unwrap();
        while !*released {
            released = self.changed.wait(released).unwrap();
        }
    }
}
struct NativePublicationRelease(std::sync::Arc<NativePublicationBarrier>);
impl Drop for NativePublicationRelease {
    fn drop(&mut self) {
        self.0.release();
    }
}

#[tokio::test(flavor = "current_thread")]
async fn native_projection_compound_publication_recovers_full_and_light() {
    use crate::session::acp_session::native_history_policy_tests::native_projection_disk_fixture;
    use xai_chat_state::persistence::native::{digest, strict_history_jsonl};
    use xai_grok_sampling_types::conversation::native_search_history::NativeUnavailable;
    tokio::task::LocalSet::new().run_until(async {
        let fixture = native_projection_disk_fixture().await;
        let native = fixture.directory.path().join("native_history");
        let ordinary = fixture.directory.path().join("chat_history.jsonl");
        let before = std::fs::read(&ordinary).unwrap();
        let before_head = std::fs::read(native.join("authority.json")).unwrap();
        for phase in ["before_history_file_sync", "before_history_directory_sync"] {
            let adapter = JsonlStorageAdapter::with_explicit_session_dir(fixture.directory.path().to_path_buf())
                .with_native_projection_probe(move |actual| if actual == phase { Err(io::Error::other("held pre-before barrier failure")) } else { Ok(()) });
            assert_eq!(adapter.persist_native_projection_and_ack(&fixture.info, fixture.write.clone()).await, Err(NativeUnavailable::Persistence));
            assert_eq!(std::fs::read(&ordinary).unwrap(), before);
            assert_eq!(std::fs::read(native.join("authority.json")).unwrap(), before_head);
        }
        let barrier = std::sync::Arc::new(NativePublicationBarrier { released: std::sync::Mutex::new(false), changed: std::sync::Condvar::new() });
        let release = NativePublicationRelease(barrier.clone());
        let (entered, mut observed) = tokio::sync::mpsc::unbounded_channel();
        let held = barrier.clone();
        let adapter = JsonlStorageAdapter::with_explicit_session_dir(fixture.directory.path().to_path_buf())
            .with_native_projection_probe(move |phase| {
                if phase == "head_parent_sync" { entered.send(()).unwrap(); held.wait(); }
                Ok(())
            });
        let info = fixture.info.clone(); let write = fixture.write.clone(); let writer = adapter.clone();
        let mut operation = tokio::task::spawn_local(async move { writer.persist_native_projection_and_ack(&info, write).await });
        tokio::select! {
            result = &mut operation => panic!("compound operation completed before its real held head-directory barrier: {result:?}"),
            entered = observed.recv() => assert!(entered.is_some()),
        }
        assert!(!operation.is_finished(), "ACK must not precede the held real parent barrier");
        assert_eq!(std::fs::read(&ordinary).unwrap(), before, "selected head is not yet materialized");
        let reader = JsonlStorageAdapter::with_explicit_session_dir(fixture.directory.path().to_path_buf());
        let timeout = reader.load_session(&fixture.info).await.unwrap_err();
        assert_eq!(timeout.kind(), io::ErrorKind::TimedOut);
        assert_eq!(reader.read_native_history(&fixture.info).await.unwrap_err(), NativeUnavailable::Indeterminate);
        barrier.release();
        let ack = tokio::time::timeout(std::time::Duration::from_secs(30), operation).await.unwrap().unwrap().unwrap();
        drop(release);
        assert_eq!(ack.transaction(), fixture.write.commit.transaction);
        assert_eq!(ack.generation(), fixture.write.commit.high_water);
        assert_eq!(ack.checkpoint().digest, fixture.write.commit.checkpoint.digest);
        assert_eq!(ack.authority_digest(), digest(&fixture.write.authority_bytes));
        let full = reader.load_session(&fixture.info).await.unwrap();
        let light = reader.load_session_without_updates(&fixture.info).await.unwrap();
        let native_read = reader.read_native_history(&fixture.info).await.unwrap().checkpoint.unwrap();
        assert_eq!(serde_json::to_value(&full.chat_history).unwrap(), serde_json::to_value(&light.chat_history).unwrap());
        assert_eq!(serde_json::to_value(&full.chat_history).unwrap(), serde_json::to_value(strict_history_jsonl(&fixture.write.image_bytes).unwrap()).unwrap());
        native_read.matches_history(&full.chat_history).unwrap();
        let head: serde_json::Value = serde_json::from_slice(&std::fs::read(native.join("authority.json")).unwrap()).unwrap();
        assert_eq!(head.as_object().unwrap().len(), 16);
        assert_eq!(head["record_kind"], "native_compound_commit");
        assert_eq!(std::fs::read(native.join("blobs").join(format!("{}.json", ack.authority_digest()))).unwrap(), std::fs::read(native.join("authority.json")).unwrap());
        let marker: serde_json::Value = serde_json::from_slice(&std::fs::read(native.join("materialized.json")).unwrap()).unwrap();
        assert_eq!(marker.as_object().unwrap().len(), 4);
        assert_eq!(marker["authority_digest"], ack.authority_digest());
        assert_eq!(marker["history_digest"], digest(&fixture.write.image_bytes));
        native_projection_crash_matrix().await;
        native_projection_io_matrix().await;
    }).await;
}

async fn native_projection_io_matrix() {
    use crate::session::acp_session::native_history_policy_tests::native_projection_disk_fixture;
    use xai_grok_sampling_types::conversation::native_search_history::NativeUnavailable;
    let fixture = native_projection_disk_fixture().await;
    for phase in [
        "before_history_file_sync",
        "before_history_directory_sync",
        "dependency_file_sync",
        "dependency_directory_sync",
        "backup",
        "staging",
        "authority_rename",
        "head_parent_sync",
        "materialization",
        "materialized_marker",
        "current_checkpoint_mirror",
        "required_marker",
        "summary",
        "acknowledgement",
    ] {
        let directory = tempfile::tempdir().unwrap();
        native_crash_copy_tree(fixture.directory.path(), directory.path());
        let head = std::fs::read(directory.path().join("native_history/authority.json")).unwrap();
        let mirror = std::fs::read(directory.path().join("chat_history.jsonl")).unwrap();
        let fired = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let observed = fired.clone();
        let adapter =
            JsonlStorageAdapter::with_explicit_session_dir(directory.path().to_path_buf())
                .with_native_projection_probe(move |actual| {
                    if phase == actual {
                        observed.store(true, std::sync::atomic::Ordering::SeqCst);
                        return Err(io::Error::other("selected real IO barrier failure"));
                    }
                    Ok(())
                });
        let result = adapter
            .persist_native_projection_and_ack(&fixture.info, fixture.write.clone())
            .await;
        assert!(
            fired.load(std::sync::atomic::Ordering::SeqCst),
            "the exact {phase} barrier must fire"
        );
        if matches!(
            phase,
            "before_history_file_sync"
                | "before_history_directory_sync"
                | "dependency_file_sync"
                | "dependency_directory_sync"
                | "backup"
                | "staging"
                | "authority_rename"
        ) {
            assert!(matches!(
                result,
                Err(NativeUnavailable::Persistence | NativeUnavailable::Indeterminate)
            ));
            assert_eq!(
                std::fs::read(directory.path().join("native_history/authority.json")).unwrap(),
                head
            );
            assert_eq!(
                std::fs::read(directory.path().join("chat_history.jsonl")).unwrap(),
                mirror
            );
        } else {
            assert!(
                result.is_err(),
                "no barrier failure may return a completed ACK"
            );
        }
        let restored =
            JsonlStorageAdapter::with_explicit_session_dir(directory.path().to_path_buf());
        let ack = restored
            .persist_native_projection_and_ack(&fixture.info, fixture.write.clone())
            .await
            .unwrap();
        assert_eq!(ack.transaction(), fixture.write.commit.transaction);
        assert_eq!(ack.generation(), fixture.write.commit.high_water);
        assert_eq!(
            std::fs::read(directory.path().join("native_history/current.json")).unwrap(),
            fixture.write.checkpoint.bytes.as_ref()
        );
        let full = restored.load_session(&fixture.info).await.unwrap();
        let light = restored
            .load_session_without_updates(&fixture.info)
            .await
            .unwrap();
        assert_eq!(
            serde_json::to_value(&full.chat_history).unwrap(),
            serde_json::to_value(&light.chat_history).unwrap()
        );
        restored
            .read_native_history(&fixture.info)
            .await
            .unwrap()
            .checkpoint
            .unwrap()
            .matches_history(&full.chat_history)
            .unwrap();
    }
}

#[tokio::test(flavor = "current_thread")]
async fn native_projection_selected_reference_temp_is_recovered_exactly() {
    use crate::session::acp_session::native_history_policy_tests::native_projection_disk_fixture;
    use xai_chat_state::persistence::native::digest;
    use xai_grok_sampling_types::conversation::native_search_history::*;
    tokio::task::LocalSet::new().run_until(async {
        let fixture = native_projection_disk_fixture().await;
        let references = [fixture.write.commit.checkpoint.clone(),fixture.write.commit.projection.clone().unwrap(),
            fixture.write.commit.after_image.as_ref().unwrap().blob.clone(),fixture.write.checkpoint.record.source.clone(),
            NativeBlobRef {kind:"compound_commit".into(),byte_len:fixture.write.authority_bytes.len() as u64,digest:digest(&fixture.write.authority_bytes)}];
        for reference in references {
            let directory = tempfile::tempdir().unwrap();native_crash_copy_tree(fixture.directory.path(),directory.path());
            let adapter = JsonlStorageAdapter::with_explicit_session_dir(directory.path().to_path_buf());
            adapter.persist_native_projection_and_ack(&fixture.info,fixture.write.clone()).await.unwrap();
            let (folder,kind,extension) = match reference.kind.as_str() {
                "ordinary_history_jsonl" => ("history","image","jsonl"),"projection_metadata" => ("blobs","metadata","json"),
                "compound_commit" => ("blobs","commit","json"),"checkpoint" => ("blobs","checkpoint","json"),"source" => ("blobs","source","json"),_ => unreachable!(),
            };
            let root = directory.path().join("native_history").join(folder);
            let canonical = root.join(format!("{}.{}",reference.digest,extension));
            let temporary = root.join(format!(".s3-{kind}-{}-{}.tmp",reference.digest,"f".repeat(32)));
            std::fs::rename(&canonical,&temporary).unwrap();
            #[cfg(unix)] {use std::os::unix::fs::PermissionsExt;std::fs::set_permissions(&temporary,std::fs::Permissions::from_mode(0o600)).unwrap();}
            println!("{}",serde_json::json!({"selected_temp_diagnosis":true,"kind":reference.kind,"digest":reference.digest,"temporary":temporary.file_name().unwrap().to_string_lossy(),"length":std::fs::metadata(&temporary).unwrap().len()}));
            let full = adapter.load_session(&fixture.info).await.expect("exact selected temporary must recover");
            let light = adapter.load_session_without_updates(&fixture.info).await.unwrap();
            assert!(canonical.exists(),"exact selected dependency is durably recovered from its own verified temporary");
            assert_eq!(serde_json::to_value(&full.chat_history).unwrap(),serde_json::to_value(&light.chat_history).unwrap());
            adapter.read_native_history(&fixture.info).await.unwrap().checkpoint.unwrap().matches_history(&full.chat_history).unwrap();
            assert_eq!(digest(&std::fs::read(canonical).unwrap()),reference.digest);
        }
    }).await;
}

fn native_crash_copy_tree(source: &std::path::Path, target: &std::path::Path) {
    std::fs::create_dir_all(target).unwrap();
    for entry in std::fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let metadata = std::fs::symlink_metadata(entry.path()).unwrap();
        assert!(!metadata.file_type().is_symlink());
        let destination = target.join(entry.file_name());
        if metadata.is_dir() {
            native_crash_copy_tree(&entry.path(), &destination);
        } else {
            std::fs::copy(entry.path(), destination).unwrap();
        }
    }
}

fn native_crash_write_payload(
    path: &std::path::Path,
    write: &xai_chat_state::persistence::native::NativeProjectionWrite,
    info: &Info,
) {
    let blobs: std::collections::BTreeMap<_, _> = write
        .checkpoint
        .blobs
        .iter()
        .map(|(hash, bytes)| (hash.clone(), bytes.to_vec()))
        .collect();
    let value = serde_json::json!({"id":info.id.to_string(),"cwd":info.cwd,"commit":write.commit,
        "checkpoint":write.checkpoint.bytes.to_vec(),"blobs":blobs,"authority":write.authority_bytes.to_vec(),
        "metadata":write.metadata_bytes.to_vec(),"image":write.image_bytes.to_vec(),
        "pending_transaction":write.pending.transaction,"dependencies":write.pending.dependencies,"temporaries":write.pending.temporaries});
    std::fs::write(path, serde_json::to_vec(&value).unwrap()).unwrap();
}

fn native_crash_read_payload(
    path: &std::path::Path,
) -> (
    Info,
    xai_chat_state::persistence::native::NativeProjectionWrite,
) {
    use xai_chat_state::persistence::native::*;
    use xai_grok_sampling_types::conversation::native_search_history::*;
    let value: serde_json::Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    let bytes = |key: &str| -> std::sync::Arc<[u8]> {
        serde_json::from_value::<Vec<u8>>(value[key].clone())
            .unwrap()
            .into()
    };
    let commit: NativeCompoundCommit = serde_json::from_value(value["commit"].clone()).unwrap();
    let authority_bytes = bytes("authority");
    let metadata_bytes = bytes("metadata");
    let image_bytes = bytes("image");
    let blobs: std::collections::BTreeMap<String, Vec<u8>> =
        serde_json::from_value(value["blobs"].clone()).unwrap();
    let checkpoint = NativeCheckpoint::decode_with_compound_proof(
        bytes("checkpoint").to_vec(),
        NativeCompoundProofInput {
            commit: commit.clone(),
            authority_bytes: authority_bytes.clone(),
            metadata_bytes: metadata_bytes.clone(),
            image_bytes: image_bytes.clone(),
        },
        |reference| {
            blobs
                .get(&reference.digest)
                .cloned()
                .ok_or(NativeUnavailable::InvalidReference)
        },
    )
    .unwrap();
    let info = Info {
        id: acp::SessionId::new(value["id"].as_str().unwrap()),
        cwd: value["cwd"].as_str().unwrap().into(),
    };
    let write = NativeProjectionWrite {
        commit,
        authority_bytes,
        checkpoint: std::sync::Arc::new(checkpoint),
        metadata_bytes,
        image_bytes,
        pending: NativePendingProtection {
            transaction: serde_json::from_value(value["pending_transaction"].clone()).unwrap(),
            dependencies: serde_json::from_value(value["dependencies"].clone()).unwrap(),
            temporaries: serde_json::from_value(value["temporaries"].clone()).unwrap(),
        },
    };
    write.validate().unwrap();
    (info, write)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_projection_crash_child() {
    use std::io::Write;
    let Some(payload) = std::env::var_os("S3_CRASH_PAYLOAD") else {
        return;
    };
    let target = std::env::var_os("S3_CRASH_TARGET").unwrap();
    let phase = std::env::var("S3_CRASH_PHASE").unwrap();
    let (info, write) = native_crash_read_payload(std::path::Path::new(&payload));
    let adapter = JsonlStorageAdapter::with_explicit_session_dir(target.into())
        .with_native_projection_probe(move |actual| {
            if actual == phase {
                println!("S3-CRASH-PHASE");
                std::io::stdout().flush().unwrap();
                loop {
                    std::thread::park();
                }
            }
            Ok(())
        });
    let result = adapter
        .persist_native_projection_and_ack(&info, write)
        .await;
    panic!("the selected real phase was not reached: {result:?}");
}

async fn native_projection_crash_matrix() {
    use tokio::io::AsyncBufReadExt;
    use crate::session::acp_session::native_history_policy_tests::native_projection_disk_fixture;
    use xai_chat_state::persistence::native::strict_history_jsonl;
    let fixture = native_projection_disk_fixture().await;
    let before = std::fs::read(fixture.directory.path().join("chat_history.jsonl")).unwrap();
    let phases = [
        "before_history_file_sync",
        "before_history_directory_sync",
        "backup",
        "staging",
        "authority_rename",
        "head_parent_sync",
        "materialization",
        "materialized_marker",
        "current_checkpoint_mirror",
        "required_marker",
        "summary",
        "acknowledgement",
    ];
    for phase in phases {
        let target = tempfile::tempdir().unwrap();
        native_crash_copy_tree(fixture.directory.path(), target.path());
        let payload = target.path().join("S3-crash-payload.json");
        native_crash_write_payload(&payload, &fixture.write, &fixture.info);
        let scope = xai_tty_utils::ProcessScope::new();
        let mut command = tokio::process::Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "session::storage::jsonl::durable_tests::native_projection_crash_child",
                "--nocapture",
            ])
            .env("S3_CRASH_PAYLOAD", &payload)
            .env("S3_CRASH_TARGET", target.path())
            .env("S3_CRASH_PHASE", phase)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true);
        let (mut child, _owner) = scope.spawn(command).unwrap();
        let mut lines = tokio::io::BufReader::new(child.stdout.take().unwrap()).lines();
        let reached = tokio::time::timeout(std::time::Duration::from_secs(30), async {
            while let Some(line) = lines.next_line().await.unwrap() {
                if line == "S3-CRASH-PHASE" {
                    return true;
                }
            }
            false
        })
        .await;
        scope.kill_all();
        let status = tokio::time::timeout(std::time::Duration::from_secs(10), child.wait())
            .await
            .unwrap()
            .unwrap();
        assert!(
            reached.unwrap(),
            "the child must reach the exact real {phase} barrier"
        );
        assert!(!status.success());
        let adapter = JsonlStorageAdapter::with_explicit_session_dir(target.path().to_path_buf());
        let full = adapter.load_session(&fixture.info).await.unwrap();
        let light = adapter
            .load_session_without_updates(&fixture.info)
            .await
            .unwrap();
        let native = adapter
            .read_native_history(&fixture.info)
            .await
            .unwrap()
            .checkpoint
            .unwrap();
        assert_eq!(
            serde_json::to_value(&full.chat_history).unwrap(),
            serde_json::to_value(&light.chat_history).unwrap()
        );
        let expected: &[u8] =
            if native.record.generation == fixture.write.commit.previous_generation {
                before.as_slice()
            } else {
                assert_eq!(native.record.generation, fixture.write.commit.high_water);
                fixture.write.image_bytes.as_ref()
            };
        assert_eq!(
            serde_json::to_value(&full.chat_history).unwrap(),
            serde_json::to_value(strict_history_jsonl(expected).unwrap()).unwrap()
        );
        native.matches_history(&full.chat_history).unwrap();
        assert_eq!(native.source, fixture.write.checkpoint.source);
        assert_eq!(native.identity, fixture.write.checkpoint.identity);
        assert_eq!(native.ledger.usage, fixture.write.checkpoint.ledger.usage);
        let ack = adapter
            .persist_native_projection_and_ack(&fixture.info, fixture.write.clone())
            .await
            .unwrap();
        assert_eq!(ack.generation(), fixture.write.commit.high_water);
        assert_eq!(
            std::fs::read(target.path().join("native_history/current.json")).unwrap(),
            fixture.write.checkpoint.bytes.as_ref()
        );
        println!(
            "{}",
            serde_json::json!({"S3_crash_phase":phase,"child_exit":status.code(),"selected_generation":native.record.generation,"retry_generation":ack.generation()})
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn native_projection_lost_ack_reconciles_identical_transaction() {
    use crate::session::acp_session::native_history_policy_tests::native_projection_disk_fixture;
    use xai_chat_state::persistence::native::{NativePendingProtection, NativeWrite, digest};
    use xai_grok_sampling_types::conversation::native_search_history::NativeUnavailable;
    tokio::task::LocalSet::new().run_until(async {
        let mut fixture = native_projection_disk_fixture().await;
        let adapter = JsonlStorageAdapter::with_explicit_session_dir(fixture.directory.path().to_path_buf());
        let first = adapter.persist_native_projection_and_ack(&fixture.info, fixture.write.clone()).await.unwrap();
        fixture.append_ordinary("ordinary tail after projection ACK loss").await;
        let tail = std::fs::read(fixture.directory.path().join("chat_history.jsonl")).unwrap();
        let retry = adapter.persist_native_projection_and_ack(&fixture.info, fixture.write.clone()).await.unwrap();
        assert_eq!(retry, first, "the frozen projection key returns its original ACK");
        assert_eq!(std::fs::read(fixture.directory.path().join("chat_history.jsonl")).unwrap(), tail);
        let later = fixture.later_native_checkpoint().await;
        assert!(later.checkpoint.compound_lineage);
        assert!(later.checkpoint.source.responses.len() > fixture.write.checkpoint.source.responses.len());
        let native_ack = adapter.persist_native_checkpoint_and_ack(&fixture.info, later.clone(), Some(NativePendingProtection::default())).await.unwrap();
        adapter.append_chat_message(&fixture.info, &ConversationItem::user("tail after later capture ACK loss")).await.unwrap();
        let later_tail = std::fs::read(fixture.directory.path().join("chat_history.jsonl")).unwrap();
        let repeat = adapter.persist_native_checkpoint_and_ack(&fixture.info, later.clone(), Some(NativePendingProtection::default())).await.unwrap();
        assert_eq!(repeat, native_ack, "later native_checkpoint keeps the unchanged caller transaction and original generation/ACK");
        assert_eq!(std::fs::read(fixture.directory.path().join("chat_history.jsonl")).unwrap(), later_tail);
        let mut wrong_predecessor = later.clone(); wrong_predecessor.previous_generation += 1;
        assert!(adapter.persist_native_checkpoint_and_ack(&fixture.info, wrong_predecessor, Some(NativePendingProtection::default())).await.is_err());
        let binding = later.checkpoint.source.producing_bindings[0].origin_ref.clone();
        let different = later.checkpoint.with_snapshot(br#"{"model":"u5a","tools":[]}"#, binding.clone()).unwrap();
        let mut wrong_checkpoint = later.clone(); wrong_checkpoint.checkpoint = std::sync::Arc::new(different.clone());
        assert!(adapter.persist_native_checkpoint_and_ack(&fixture.info, wrong_checkpoint, Some(NativePendingProtection::default())).await.is_err());
        let successor = NativeWrite { previous_generation: later.checkpoint.record.generation,
            transaction: "later-snapshot-successor".into(), checkpoint: std::sync::Arc::new(different) };
        adapter.persist_native_checkpoint_and_ack(&fixture.info, successor, Some(NativePendingProtection::default())).await.unwrap();
        assert_eq!(adapter.persist_native_checkpoint_and_ack(&fixture.info, later.clone(), Some(NativePendingProtection::default())).await, Err(NativeUnavailable::StaleHistory));
        assert_eq!(adapter.persist_native_projection_and_ack(&fixture.info, fixture.write.clone()).await, Err(NativeUnavailable::StaleHistory));
        assert_eq!(std::fs::read(fixture.directory.path().join("chat_history.jsonl")).unwrap(), later_tail);
        let authority: serde_json::Value = serde_json::from_slice(&std::fs::read(fixture.directory.path().join("native_history/authority.json")).unwrap()).unwrap();
        assert_eq!(authority["operation"], "native_checkpoint");
        assert!(authority["after_image"].is_null() && authority["projection"].is_null());
        let native = adapter.read_native_history(&fixture.info).await.unwrap().checkpoint.unwrap();
        assert!(native.ledger.usage.starts_with(&later.checkpoint.ledger.usage));
        assert_eq!(authority["checkpoint"]["digest"], digest(&native.bytes));
        let full = adapter.load_session(&fixture.info).await.unwrap();
        let light = adapter.load_session_without_updates(&fixture.info).await.unwrap();
        assert_eq!(serde_json::to_value(&full.chat_history).unwrap(), serde_json::to_value(&light.chat_history).unwrap());
    }).await;
}

#[tokio::test(flavor = "current_thread")]
async fn native_projection_metadata_only_is_acknowledged() {
    use crate::session::acp_session::native_history_policy_tests::native_projection_disk_fixture;
    use xai_chat_state::persistence::native::digest;
    tokio::task::LocalSet::new()
        .run_until(async {
            let mut fixture = native_projection_disk_fixture().await;
            let adapter = JsonlStorageAdapter::with_explicit_session_dir(
                fixture.directory.path().to_path_buf(),
            );
            let first = adapter
                .persist_native_projection_and_ack(&fixture.info, fixture.write.clone())
                .await
                .unwrap();
            let next = fixture.next_projection().await;
            assert_eq!(
                next.image_bytes, fixture.write.image_bytes,
                "ordinary image is identical on this provenance-only successor"
            );
            assert_ne!(next.metadata_bytes, fixture.write.metadata_bytes);
            let acknowledgement = adapter
                .persist_native_projection_and_ack(&fixture.info, next.clone())
                .await
                .unwrap();
            assert_eq!(acknowledgement.generation(), first.generation() + 1);
            assert_eq!(acknowledgement.transaction(), next.commit.transaction);
            adapter
                .retire_native_projection_recovery(&fixture.info, next.pending.clone())
                .await
                .unwrap();
            let history = fixture.directory.path().join("native_history/history");
            let images: Vec<_> = std::fs::read_dir(&history)
                .unwrap()
                .map(|entry| entry.unwrap().file_name())
                .collect();
            assert_eq!(
                images,
                vec![std::ffi::OsString::from(format!(
                    "{}.jsonl",
                    digest(&next.image_bytes)
                ))]
            );
            assert!(
                fixture
                    .directory
                    .path()
                    .join("native_history/blobs")
                    .join(format!("{}.json", acknowledgement.authority_digest()))
                    .exists(),
                "selected commit is a protected root without a self reference"
            );
            let full = adapter.load_session(&fixture.info).await.unwrap();
            let light = adapter
                .load_session_without_updates(&fixture.info)
                .await
                .unwrap();
            let native = adapter
                .read_native_history(&fixture.info)
                .await
                .unwrap()
                .checkpoint
                .unwrap();
            native.matches_history(&full.chat_history).unwrap();
            assert_eq!(
                serde_json::to_value(&full.chat_history).unwrap(),
                serde_json::to_value(&light.chat_history).unwrap()
            );
            assert_eq!(native.source, fixture.write.checkpoint.source);
            assert_eq!(native.identity, fixture.write.checkpoint.identity);
            assert_eq!(
                adapter
                    .persist_native_projection_and_ack(&fixture.info, next.clone())
                    .await
                    .unwrap(),
                acknowledgement
            );
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn native_projection_corrupt_head_or_image_never_uses_backup() {
    use crate::session::acp_session::native_history_policy_tests::native_projection_disk_fixture;
    use xai_chat_state::persistence::native::digest;
    tokio::task::LocalSet::new()
        .run_until(async {
            for fault in [
                "unknown-head",
                "missing-image",
                "changed-image",
                "wrong-ordinal",
                "duplicate-member",
                "wrong-f-digest",
                "unknown-inner",
            ] {
                let fixture = native_projection_disk_fixture().await;
                let native = fixture.directory.path().join("native_history");
                let adapter = JsonlStorageAdapter::with_explicit_session_dir(
                    fixture.directory.path().to_path_buf(),
                );
                adapter
                    .persist_native_projection_and_ack(&fixture.info, fixture.write.clone())
                    .await
                    .unwrap();
                let mut commit = fixture.write.commit.clone();
                match fault {
                    "unknown-head" => {
                        let mut head = serde_json::to_value(&commit).unwrap();
                        head["unapproved"] = true.into();
                        std::fs::write(
                            native.join("authority.json"),
                            serde_json::to_vec(&head).unwrap(),
                        )
                        .unwrap();
                    }
                    "missing-image" => {
                        let image = native.join("history").join(format!(
                            "{}.jsonl",
                            commit.after_image.as_ref().unwrap().blob.digest
                        ));
                        std::fs::rename(&image, image.with_extension("aside")).unwrap();
                    }
                    "changed-image" => {
                        let image = native.join("history").join(format!(
                            "{}.jsonl",
                            commit.after_image.as_ref().unwrap().blob.digest
                        ));
                        std::fs::write(image, b"{\"type\":\"user\",\"content\":[]}\n").unwrap();
                    }
                    "unknown-inner" => {
                        let original = native
                            .join("blobs")
                            .join(format!("{}.json", commit.checkpoint.digest));
                        let mut checkpoint: serde_json::Value =
                            serde_json::from_slice(&std::fs::read(&original).unwrap()).unwrap();
                        checkpoint["schema_version"] = 3.into();
                        checkpoint["reader_floor"] = 3.into();
                        checkpoint["limits_revision"] = 2.into();
                        let bytes = serde_json::to_vec(&checkpoint).unwrap();
                        commit.checkpoint.digest = digest(&bytes);
                        commit.checkpoint.byte_len = bytes.len() as u64;
                        std::fs::write(
                            native
                                .join("blobs")
                                .join(format!("{}.json", commit.checkpoint.digest)),
                            bytes,
                        )
                        .unwrap();
                        commit.transaction = commit.projection_transaction().unwrap();
                        let bytes = serde_json::to_vec(&commit).unwrap();
                        std::fs::write(
                            native
                                .join("blobs")
                                .join(format!("{}.json", digest(&bytes))),
                            &bytes,
                        )
                        .unwrap();
                        std::fs::write(native.join("authority.json"), bytes).unwrap();
                    }
                    _ => {
                        let mut metadata: serde_json::Value =
                            serde_json::from_slice(&fixture.write.metadata_bytes).unwrap();
                        match fault {
                            "wrong-ordinal" => {
                                metadata["members"][0]["source"] = u64::from(u32::MAX).into()
                            }
                            "duplicate-member" => {
                                metadata["members"][1] = metadata["members"][0].clone()
                            }
                            "wrong-f-digest" => {
                                metadata["f_provenance"]["groups"][0]["call_digest"] =
                                    "b".repeat(64).into()
                            }
                            _ => unreachable!(),
                        }
                        let bytes = serde_json::to_vec(&metadata).unwrap();
                        let reference = commit.projection.as_mut().unwrap();
                        reference.digest = digest(&bytes);
                        reference.byte_len = bytes.len() as u64;
                        std::fs::write(
                            native
                                .join("blobs")
                                .join(format!("{}.json", reference.digest)),
                            bytes,
                        )
                        .unwrap();
                        commit.transaction = commit.projection_transaction().unwrap();
                        let bytes = serde_json::to_vec(&commit).unwrap();
                        std::fs::write(
                            native
                                .join("blobs")
                                .join(format!("{}.json", digest(&bytes))),
                            &bytes,
                        )
                        .unwrap();
                        std::fs::write(native.join("authority.json"), bytes).unwrap();
                    }
                }
                assert!(
                    adapter.load_session(&fixture.info).await.is_err(),
                    "full loader must refuse {fault}, never old backup/ACP"
                );
                assert!(
                    adapter
                        .load_session_without_updates(&fixture.info)
                        .await
                        .is_err(),
                    "light loader must refuse {fault}"
                );
                assert!(
                    adapter.read_native_history(&fixture.info).await.is_err(),
                    "native loader must refuse {fault}"
                );
                assert!(
                    native.join("backup.json").exists(),
                    "a real available backup cannot rescue the required corrupt head"
                );
            }
        })
        .await;
}

fn native_section7_encoder_fixture(
    corrected: bool,
) -> (
    xai_grok_sampling_types::conversation::native_search_history::NativeLedgerV2,
    xai_grok_sampling_types::conversation::native_search_history::NativeProjectionMetadata,
) {
    use xai_grok_sampling_types::conversation::native_search_history::*;
    use std::collections::BTreeMap;
    let lineage = "00000000-0000-7000-8000-000000000000";
    let digest = "a".repeat(64);
    let request = |index: usize| format!("00000000-0000-4000-8000-{index:012}");
    let coordinate = |index: usize| NativeSourceCoordinate {
        lineage: lineage.into(),
        producing_binding: request(index / 8),
        turn: u64::MAX,
        item: u64::MAX,
        response: (index / 8) as u64,
        block: (index % 8) as u64,
        submember: None,
        occurrence: index as u64,
    };
    let mut ledger = NativeLedger {
        source_digest: digest.clone(),
        active_digest: digest.clone(),
        ..Default::default()
    };
    let mut references = Vec::new();
    let mut members = Vec::new();
    let mut groups = Vec::new();
    let mut qualification = Vec::new();
    let mut provenance = Vec::new();
    let domain = NativeProjectionDomain {
        backend: if corrected {
            NativeDomainBackend::ChatCompletions
        } else {
            NativeDomainBackend::Responses
        },
        dialect: NativeDomainDialect::Responses(NativeResponseDialect::Codex),
        model: "m".repeat(64),
        row: "r".repeat(64),
        provider: "p".repeat(64),
        deployment: "d".repeat(64),
        deployment_revision: "v".repeat(64),
        endpoint: format!("https://{}", "e".repeat(120)),
        principal: "n".repeat(64),
    };
    for group in 0..2048usize {
        let key = format!(
            "{}:{}:{}:{}",
            request(group / 2),
            u64::MAX,
            u64::MAX,
            group + 1000
        );
        assert_eq!(key.len(), 83);
        let raw_members: Vec<_> = (group * 4..group * 4 + 4).map(coordinate).collect();
        ledger.groups.push(NativeGroupRecord {
            key: key.clone(),
            family: "Responses".into(),
            execution: "client".into(),
            origin_ref: NativeBlobRef {
                kind: "producing_binding".into(),
                byte_len: if corrected { 693 } else { 686 },
                digest: digest.clone(),
            },
            validated_join: Some("j".repeat(64)),
            keyless_server_fifo: false,
            members: raw_members.clone(),
            duplicate_observations: Vec::new(),
            state: "Completed".into(),
        });
        ledger.lifecycle.groups.insert(key, "Completed".into());
        groups.push(NativeProjectionGroup {
            key: group as u32,
            from: "Unrecorded".into(),
            to: "strip".into(),
            reason: "prior_disposition".into(),
        });
        let shape = NativeProjectionShape {
            family: "responses_client".into(),
            keyless_server: false,
            output_id_present: false,
            completed: true,
            result_error: false,
            referenced_members: 3,
        };
        qualification.push(NativeProjectionQualification {
            group: group as u32,
            source: Some(domain.clone()),
            target: domain.clone(),
            shape,
            receipt: None,
        });
        provenance.push(NativeProjectionFGroup {
            key: group as u32,
            call: (group * 4) as u32,
            call_digest: digest.clone(),
            result: (group * 4 + 3) as u32,
            result_digest: digest.clone(),
            disposition: "Strip".into(),
        });
        for raw in raw_members {
            ledger.accounting.push(NativeViewLink {
                source: raw.clone(),
                source_digest: digest.clone(),
                view_item: None,
                view_digest: None,
                role: "ProjectedText".into(),
                source_subrange: None,
            });
            members.push(NativeProjectionMember {
                source: raw.occurrence as u32,
                source_digest: digest.clone(),
                group: Some(group as u32),
                action: "Strip".into(),
                output_items: Vec::new(),
                output_digests: Vec::new(),
                loss: true,
            });
            if raw.occurrence == (group * 4 + 3) as u64 {
                for submember in 0..3u32 {
                    let mut source = raw.clone();
                    source.submember = Some(u64::from(submember));
                    let ordinal = references.len();
                    references.push(NativeEligibleMemberAccounting {
                        source,
                        source_digest: digest.clone(),
                    });
                    members.push(NativeProjectionMember {
                        source: (8192 + ordinal) as u32,
                        source_digest: digest.clone(),
                        group: Some(group as u32),
                        action: "Strip".into(),
                        output_items: Vec::new(),
                        output_digests: Vec::new(),
                        loss: true,
                    });
                }
            }
        }
    }
    for index in 0..1024usize {
        let request = request(index);
        ledger
            .lifecycle
            .exchanges
            .insert(request.clone(), "Terminal".into());
        ledger.usage.push(NativeResponseAccounting {
            request: request.clone(),
            lease: u64::MAX,
            attempt: u64::MAX,
            response_occurrence: Some(index as u64),
            continuation: u64::MAX,
            observed: BTreeMap::new(),
            source_stop_reason: Some("stop_sequence".into()),
            outcome: NativeResponseStatus::Accepted,
            latency_ms: Some(u64::MAX),
            http_dispatches: u64::MAX,
        });
        ledger.observations.push(NativeObservation {
            request,
            lease: u64::MAX,
            attempt: u64::MAX,
            digest: digest.clone(),
        });
    }
    let metadata = NativeProjectionMetadata {
        record_kind: "native_projection_metadata".into(),
        format_version: 1,
        owner_class: NativeProjectionOwner::OwningPrimary,
        issued_request: request(0),
        binding_generation: u64::MAX,
        original_admission: NativeProjectionAdmission {
            supports_search_tool: true,
            has_searchable_tools: true,
        },
        target: domain,
        versions: NativeProjectionVersions {
            schema: 2,
            policy: 1,
            projection: 1,
            reader: 2,
            limits: 1,
        },
        qualification,
        input_history: NativeHistoryBoundary {
            items: 8192,
            digest: digest.clone(),
        },
        post_history: NativeHistoryBoundary { items: 0, digest },
        groups,
        members,
        f_provenance: NativeProjectionFProvenance {
            version: 1,
            groups: provenance,
        },
        loaded_invalidations: (0..2048u32).collect(),
    };
    (NativeLedgerV2::from_parts(ledger, references), metadata)
}

#[test]
fn native_projection_section7_production_encoder_measurement() {
    use xai_grok_sampling_types::conversation::native_search_history::*;
    for corrected in [false, true] {
        let (ledger, metadata) = native_section7_encoder_fixture(corrected);
        assert_eq!(ledger.accounting.len(), 8192);
        assert_eq!(ledger.eligible_member_accounting.len(), 6144);
        assert_eq!(ledger.usage.len(), 1024);
        assert_eq!(ledger.observations.len(), 1024);
        assert_eq!(ledger.groups.len(), 2048);
        assert_eq!(metadata.members.len(), 14336);
        assert!(
            ledger.lifecycle.waiting_clients.is_empty() && ledger.lifecycle.terminal.is_empty()
        );
        assert!(ledger.usage.iter().all(|usage| usage.observed.is_empty()));
        assert!(
            metadata
                .qualification
                .iter()
                .all(|qualification| qualification.receipt.is_none())
        );
        assert!(
            metadata
                .members
                .iter()
                .all(|member| member.output_items.is_empty()
                    && member.output_digests.is_empty()
                    && member.action == "Strip")
        );
        let ledger_bytes = encode_native(&ledger, NATIVE_LEDGER_BYTES).unwrap();
        let metadata_bytes = encode_native(&metadata, NATIVE_LEDGER_BYTES).unwrap();
        let total = ledger_bytes.len() + metadata_bytes.len();
        println!(
            "{}",
            serde_json::json!({"corrected":corrected,"ledger_bytes":ledger_bytes.len(),"metadata_bytes":metadata_bytes.len(),"total":total,"members":16384,"groups":2048})
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn native_projection_section7_native_checkpoint_measurement() {
    use crate::session::acp_session::native_history_policy_tests::native_fullcount_encoder_checkpoint;
    tokio::task::LocalSet::new().run_until(async {
        let checkpoint = native_fullcount_encoder_checkpoint().await;
        println!("{}", serde_json::json!({"record_bytes":checkpoint.bytes.len(),"source_bytes":checkpoint.record.source.byte_len,
            "ledger_bytes":checkpoint.record.ledger.byte_len,"blocks":checkpoint.source.ordered_blocks.len(),
            "responses":checkpoint.source.responses.len(),"groups":checkpoint.ledger.groups.len(),
            "members":checkpoint.ledger.accounting.len()+checkpoint.eligible_members().unwrap().len()+checkpoint.ledger.usage.len()+checkpoint.ledger.observations.len()}));
    }).await;
}

fn native_section7_prepared_projection(
    checkpoint: &xai_chat_state::persistence::native::NativeCheckpoint,
    corrected: bool,
    width: usize,
    principal_extra: usize,
    request_extra: usize,
) -> xai_chat_state::persistence::native::NativePreparedProjection {
    use xai_grok_sampling_types::conversation::native_search_history::*;
    use xai_grok_sampling_types::conversation::projection::{Boundary, TargetRoute};
    let issuer = NativeHistoryAuthorityIssuer::default();
    let owner = issuer
        .issue_owner(
            NativeHistorySealPurpose::SwitchStore,
            1,
            checkpoint.record.lineage.id.clone(),
            1,
        )
        .unwrap();
    let admission = xai_grok_sampling_types::SearchAdmission {
        supports_search_tool: false,
        has_searchable_tools: false,
    };
    let domain = NativeReplayDomain {
        backend: if corrected {
            xai_grok_sampling_types::ApiBackend::ChatCompletions
        } else {
            xai_grok_sampling_types::ApiBackend::Responses
        },
        dialect: NativeReplayDialect::Responses(
            xai_grok_sampling_types::ResponsesReplayDialect::Codex,
        ),
        model: "m".repeat(width),
        row: "r".repeat(width),
        provider: "p".repeat(width),
        deployment: "d".repeat(width),
        deployment_revision: "v".repeat(width),
        endpoint: if width == 1 {
            "http://a".into()
        } else {
            format!("https://{}", "e".repeat(120))
        },
        principal: "n".repeat(width + principal_extra),
    };
    let capture = NativeHistoryCapture {
        history: checkpoint.record.history_boundary.clone(),
        history_revision: 1,
        lineage: checkpoint.record.lineage.clone(),
        native_generation: checkpoint.record.generation,
        high_water: checkpoint.record.high_water,
        projection_revision: checkpoint.active.projection_revision,
        strict_append_sequence: 1,
        binding_generation: 1,
        install_generation: 1,
        issued_request: format!(
            "00000000-0000-4000-8000-000000000003{}",
            "x".repeat(request_extra)
        ),
        target: domain.clone(),
        execution: NativeHistoryExecution::Server,
        original_admission: admission,
        versions: NativeHistoryVersions {
            schema: 2,
            policy: 1,
            projection: 1,
            reader: 2,
            limits: 1,
        },
    };
    let seal = issuer.issue_bound(&owner, capture.clone()).unwrap();
    let context = NativeHistoryRequestContext::captured(
        NativeHistoryRequirement::Selected,
        capture.clone(),
        NativeHistoryWriteAuthority::SwitchStore {
            lineage: checkpoint.record.lineage.id.clone(),
            binding_generation: 1,
        },
    );
    let preparation = NativeBoundPreparation::issued(
        NativeHistoryRequirement::Selected,
        NativePreparationGrant::SwitchStore,
        capture.issued_request.clone(),
        capture.history.clone(),
        NativeHistoryTransformMapping::checked(
            capture.history.clone(),
            capture.history.clone(),
            Vec::new(),
        )
        .unwrap(),
        Some(1),
        Some(1),
        1,
        1,
    )
    .with_issued_context(context, seal, std::sync::Arc::from(&b"[]"[..]))
    .unwrap();
    let request = xai_grok_sampling_types::ConversationRequest {
        items: Vec::new(),
        model: Some(domain.model.clone()),
        search_admission: Some(admission),
        ..Default::default()
    };
    checkpoint
        .prepare_projection(
            &preparation,
            &request,
            &TargetRoute::new(domain.backend.clone(), Boundary::AzStrict, admission),
        )
        .unwrap()
}

fn native_section7_pooled_bytes(
    write: &xai_chat_state::persistence::native::NativeProjectionWrite,
) -> u64 {
    let ledger: u64 = write
        .checkpoint
        .blobs
        .iter()
        .filter(|(hash, _)| {
            write
                .checkpoint
                .blob_kinds
                .get(*hash)
                .is_some_and(|kind| kind == "ledger")
        })
        .map(|(_, bytes)| bytes.len() as u64)
        .sum();
    ledger
        + write.metadata_bytes.len() as u64
        + write.checkpoint.record.capacity["terminal_ledger_bytes"]
}

async fn native_section7_boundary_probe(
    response_count: u64,
    corrected: bool,
    exact_extra: Option<u64>,
) {
    use crate::session::acp_session::native_history_policy_tests::native_counted_encoder_checkpoint;
    use xai_chat_state::persistence::native::{
        NativeCheckpoint, NativeWrite, digest, strict_history_jsonl,
    };
    use xai_grok_sampling_types::conversation::native_search_history::*;
    let checkpoint = native_counted_encoder_checkpoint(response_count).await;
    let directory = tempfile::tempdir().unwrap();
    let info = Info {
        id: acp::SessionId::new(checkpoint.record.lineage.id.clone()),
        cwd: directory.path().to_string_lossy().into_owned(),
    };
    let adapter = JsonlStorageAdapter::with_explicit_session_dir(directory.path().to_path_buf());
    adapter
        .init_session(&info, default_model_id())
        .await
        .unwrap();
    adapter.replace_chat_history(&info, &[]).await.unwrap();
    let previous_ref = checkpoint.record.previous_checkpoint.as_ref().unwrap();
    let previous = NativeCheckpoint::decode(
        checkpoint.blobs[&previous_ref.digest].to_vec(),
        |reference| {
            checkpoint
                .blobs
                .get(&reference.digest)
                .map(|bytes| bytes.to_vec())
                .ok_or(NativeUnavailable::InvalidReference)
        },
    )
    .unwrap();
    for current in [&previous, &checkpoint] {
        adapter
            .persist_native_generation_and_ack(
                &info,
                NativeWrite {
                    previous_generation: current.record.generation - 1,
                    transaction: format!("section7-capture-{}", current.record.generation),
                    checkpoint: std::sync::Arc::new(current.clone()),
                },
            )
            .await
            .unwrap();
    }
    let checkpoint = adapter
        .read_native_history(&info)
        .await
        .unwrap()
        .checkpoint
        .unwrap();
    let width = if exact_extra.is_some() { 128 } else { 1 };
    let base = native_section7_prepared_projection(&checkpoint, corrected, width, 0, 0);
    let base_pool = native_section7_pooled_bytes(&base.write);
    let (principal_extra, request_extra) = if let Some(extra) = exact_extra {
        assert!(
            base_pool < NATIVE_LEDGER_BYTES,
            "the measured real-producer base must fit before exact-boundary extension"
        );
        let repeated_targets =
            serde_json::from_slice::<NativeProjectionMetadata>(&base.write.metadata_bytes)
                .unwrap()
                .qualification
                .len() as u64
                + 1;
        let slack = NATIVE_LEDGER_BYTES - base_pool;
        (
            (slack / repeated_targets) as usize,
            (slack % repeated_targets + extra) as usize,
        )
    } else {
        (0, 0)
    };
    let prepared = native_section7_prepared_projection(
        &checkpoint,
        corrected,
        width,
        principal_extra,
        request_extra,
    );
    prepared.write.validate().unwrap();
    let pooled = native_section7_pooled_bytes(&prepared.write);
    if let Some(extra) = exact_extra {
        assert_eq!(
            pooled,
            NATIVE_LEDGER_BYTES + extra,
            "actual producer/encoder bytes reach the exact requested boundary"
        );
        assert!(
            prepared.write.metadata_bytes.len() as u64 > NATIVE_CONTROL_BYTES,
            "metadata over 4 MiB remains charged to the existing ledger pool"
        );
    }
    assert_eq!(prepared.write.checkpoint.source, checkpoint.source);
    assert_eq!(prepared.write.checkpoint.identity, checkpoint.identity);
    assert_eq!(
        prepared.write.checkpoint.ledger.usage,
        checkpoint.ledger.usage
    );
    assert_eq!(
        prepared.write.checkpoint.ledger.observations,
        checkpoint.ledger.observations
    );
    assert_eq!(
        prepared.write.checkpoint.source.ordered_blocks.len() as u64,
        response_count * 8
    );
    assert_eq!(
        prepared.write.checkpoint.eligible_members().unwrap().len() as u64,
        response_count * 6
    );
    assert_eq!(
        prepared.write.checkpoint.ledger.groups.len() as u64,
        response_count * 2
    );
    assert_eq!(
        prepared.write.checkpoint.record.capacity["terminal_ledger_bytes"],
        0
    );
    let head_path = directory.path().join("native_history/authority.json");
    let mirror_path = directory.path().join("chat_history.jsonl");
    let head_before = std::fs::read(&head_path).unwrap();
    let mirror_before = std::fs::read(&mirror_path).unwrap();
    let result = adapter
        .persist_native_projection_and_ack(&info, prepared.write.clone())
        .await;
    let unchanged = std::fs::read(&head_path).unwrap() == head_before
        && std::fs::read(&mirror_path).unwrap() == mirror_before;
    if pooled <= NATIVE_LEDGER_BYTES {
        let ack = result
            .as_ref()
            .expect("a valid real-producer state at the measured ledger ceiling must be accepted");
        assert_eq!(ack.generation(), checkpoint.record.generation + 1);
        assert_eq!(
            ack.authority_digest(),
            digest(&prepared.write.authority_bytes)
        );
        let full = adapter.load_session(&info).await.unwrap();
        let light = adapter.load_session_without_updates(&info).await.unwrap();
        assert_eq!(
            serde_json::to_value(&full.chat_history).unwrap(),
            serde_json::to_value(&light.chat_history).unwrap()
        );
        assert_eq!(
            serde_json::to_value(&full.chat_history).unwrap(),
            serde_json::to_value(strict_history_jsonl(&prepared.write.image_bytes).unwrap())
                .unwrap()
        );
        adapter
            .read_native_history(&info)
            .await
            .unwrap()
            .checkpoint
            .unwrap()
            .matches_history(&full.chat_history)
            .unwrap();
    } else {
        assert_eq!(result, Err(NativeUnavailable::Capacity));
        assert!(
            unchanged,
            "Capacity refuses before changing the authoritative head, mirror or ordinary history"
        );
        assert_eq!(
            adapter
                .read_native_history(&info)
                .await
                .unwrap()
                .checkpoint
                .unwrap()
                .record
                .generation,
            checkpoint.record.generation
        );
    }
    println!(
        "{}",
        serde_json::json!({"R11f_section7":true,"corrected":corrected,"responses":response_count,
        "members":response_count * 16,"groups":response_count * 2,"approved_base":if corrected {15583232} else {15547392},
        "measured_base_pool":base_pool,"ledger_limit":NATIVE_LEDGER_BYTES,"metadata_bytes":prepared.write.metadata_bytes.len(),
        "pooled_bytes":pooled,"principal_extra":principal_extra,"issued_request_extra":request_extra,
        "result":format!("{result:?}"),"head_and_mirror_unchanged":unchanged,"previous_head_sha256":digest(&head_before),
        "previous_mirror_sha256":digest(&mirror_before)})
    );
}

#[tokio::test(flavor = "current_thread")]
async fn native_projection_a3_section7_full_count_ledger_fits() {
    tokio::task::LocalSet::new()
        .run_until(async {
            for corrected in [false, true] {
                native_section7_boundary_probe(750, corrected, Some(0)).await;
                native_section7_boundary_probe(1024, corrected, None).await;
            }
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn native_projection_a3_section7_ledger_plus_one_refuses() {
    tokio::task::LocalSet::new()
        .run_until(async {
            for corrected in [false, true] {
                native_section7_boundary_probe(750, corrected, Some(1)).await;
            }
        })
        .await;
}

#[test]
fn native_projection_section7_lower_bound_encoder_widths() {
    use xai_grok_sampling_types::conversation::native_search_history::*;
    use std::collections::BTreeMap;
    let uuid = "00000000-0000-4000-8000-000000000000";
    let digest = "a".repeat(64);
    let coordinate = NativeSourceCoordinate {
        lineage: uuid.into(),
        producing_binding: uuid.into(),
        turn: 0,
        item: 0,
        response: 0,
        block: 0,
        submember: None,
        occurrence: 0,
    };
    let mut witness_coordinate = coordinate.clone();
    witness_coordinate.submember = Some(0);
    let key = format!("{uuid}:0:0:0");
    let domain = NativeProjectionDomain {
        backend: NativeDomainBackend::Responses,
        dialect: NativeDomainDialect::Responses(NativeResponseDialect::Codex),
        model: "m".into(),
        row: "r".into(),
        provider: "p".into(),
        deployment: "d".into(),
        deployment_revision: "v".into(),
        endpoint: "http://a".into(),
        principal: "n".into(),
    };
    let shape = NativeProjectionShape {
        family: "messages_server".into(),
        keyless_server: true,
        output_id_present: true,
        completed: true,
        result_error: true,
        referenced_members: 0,
    };
    let values = [
        (
            "raw",
            serde_json::to_value(NativeViewLink {
                source: coordinate.clone(),
                source_digest: digest.clone(),
                view_item: None,
                view_digest: None,
                role: String::new(),
                source_subrange: None,
            })
            .unwrap(),
        ),
        (
            "witness",
            serde_json::to_value(NativeEligibleMemberAccounting {
                source: witness_coordinate,
                source_digest: digest.clone(),
            })
            .unwrap(),
        ),
        (
            "group",
            serde_json::to_value(NativeGroupRecord {
                key: key.clone(),
                family: "Messages".into(),
                execution: "server".into(),
                origin_ref: NativeBlobRef {
                    kind: "producing_binding".into(),
                    byte_len: 0,
                    digest: digest.clone(),
                },
                validated_join: Some(String::new()),
                keyless_server_fifo: true,
                members: vec![coordinate.clone(), coordinate],
                duplicate_observations: Vec::new(),
                state: "Completed".into(),
            })
            .unwrap(),
        ),
        (
            "usage",
            serde_json::to_value(NativeResponseAccounting {
                request: uuid.into(),
                lease: 0,
                attempt: 0,
                response_occurrence: Some(0),
                continuation: 0,
                observed: BTreeMap::new(),
                source_stop_reason: Some(String::new()),
                outcome: NativeResponseStatus::Accepted,
                latency_ms: Some(0),
                http_dispatches: 0,
            })
            .unwrap(),
        ),
        (
            "observation",
            serde_json::to_value(NativeObservation {
                request: uuid.into(),
                lease: 0,
                attempt: 0,
                digest: digest.clone(),
            })
            .unwrap(),
        ),
        (
            "member",
            serde_json::to_value(NativeProjectionMember {
                source: 0,
                source_digest: digest.clone(),
                group: Some(0),
                action: "Strip".into(),
                output_items: Vec::new(),
                output_digests: Vec::new(),
                loss: true,
            })
            .unwrap(),
        ),
        (
            "transition",
            serde_json::to_value(NativeProjectionGroup {
                key: 0,
                from: "F".into(),
                to: "strip".into(),
                reason: "kept".into(),
            })
            .unwrap(),
        ),
        (
            "qualification",
            serde_json::to_value(NativeProjectionQualification {
                group: 0,
                source: None,
                target: domain,
                shape,
                receipt: None,
            })
            .unwrap(),
        ),
        (
            "f_provenance",
            serde_json::to_value(NativeProjectionFGroup {
                key: 0,
                call: 0,
                call_digest: digest.clone(),
                result: 0,
                result_digest: digest,
                disposition: "F".into(),
            })
            .unwrap(),
        ),
    ];
    let mut measured: BTreeMap<_, _> = values
        .into_iter()
        .map(|(name, value)| {
            (
                name,
                encode_native(&value, NATIVE_LEDGER_BYTES).unwrap().len(),
            )
        })
        .collect();
    measured.insert(
        "lifecycle_group",
        encode_native(&BTreeMap::from([(key, "Completed")]), NATIVE_LEDGER_BYTES)
            .unwrap()
            .len()
            - 2,
    );
    let expected = BTreeMap::from([
        ("raw", 345),
        ("witness", 273),
        ("group", 697),
        ("usage", 207),
        ("observation", 148),
        ("lifecycle_group", 56),
        ("member", 172),
        ("transition", 49),
        ("qualification", 382),
        ("f_provenance", 211),
    ]);
    assert_eq!(measured, expected);
    println!(
        "{}",
        serde_json::json!({"production_encoder_widths":measured,"symbolic_underestimate_only":true})
    );
}

fn native_test_owner_file(path: &std::path::Path, bytes: &[u8]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
}

#[tokio::test(flavor = "current_thread")]
async fn native_projection_reserves_all_images_before_backup() {
    use crate::session::acp_session::native_history_policy_tests::native_projection_disk_fixture;
    use xai_chat_state::persistence::native::{digest, history_jsonl};
    use xai_grok_sampling_types::conversation::native_search_history::*;
    tokio::task::LocalSet::new()
        .run_until(async {
            let fixture = native_projection_disk_fixture().await;
            let native = fixture.directory.path().join("native_history");
            let before_head = std::fs::read(native.join("authority.json")).unwrap();
            let before_mirror =
                std::fs::read(fixture.directory.path().join("chat_history.jsonl")).unwrap();
            for class in ["control", "ledger-temp", "image", "mirror-temp"] {
                let mut write = fixture.write.clone();
                let bytes = match class {
                    "control" => vec![b'c'; NATIVE_CONTROL_BYTES as usize],
                    "ledger-temp" => vec![b'l'; NATIVE_LEDGER_BYTES as usize + 1],
                    _ => history_jsonl(&[ConversationItem::assistant(
                        "physical image".repeat((NATIVE_ROLE_BYTES as usize / 14) + 1),
                    )])
                    .unwrap(),
                };
                let hash = digest(&bytes);
                let path = match class {
                    "control" => native.join("unrelated-control.evidence"),
                    "ledger-temp" => native
                        .join("blobs")
                        .join(format!(".s3-ledger-{hash}-{}.tmp", "a".repeat(32))),
                    "image" => native.join("history").join(format!("{hash}.jsonl")),
                    "mirror-temp" => fixture
                        .directory
                        .path()
                        .join(format!(".s3-mirror-{hash}-{}.tmp", "b".repeat(32))),
                    _ => unreachable!(),
                };
                native_test_owner_file(&path, &bytes);
                if class != "control" {
                    write.pending.dependencies.insert(hash);
                    write
                        .pending
                        .temporaries
                        .insert(path.file_name().unwrap().to_string_lossy().into_owned());
                }
                let copied = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
                let probe = copied.clone();
                let adapter = JsonlStorageAdapter::with_explicit_session_dir(
                    fixture.directory.path().to_path_buf(),
                )
                .with_native_projection_probe(move |phase| {
                    if phase == "backup" {
                        probe.store(true, std::sync::atomic::Ordering::SeqCst);
                    }
                    Ok(())
                });
                assert_eq!(
                    adapter
                        .persist_native_projection_and_ack(&fixture.info, write)
                        .await,
                    Err(NativeUnavailable::Capacity),
                    "all physical {class} bytes must be reserved"
                );
                assert!(
                    !copied.load(std::sync::atomic::Ordering::SeqCst),
                    "capacity must refuse before backup/staging"
                );
                assert_eq!(
                    std::fs::read(native.join("authority.json")).unwrap(),
                    before_head
                );
                assert_eq!(
                    std::fs::read(fixture.directory.path().join("chat_history.jsonl")).unwrap(),
                    before_mirror
                );
                std::fs::rename(
                    &path,
                    fixture
                        .directory
                        .path()
                        .join(format!("{class}-preserved-aside")),
                )
                .unwrap();
            }
            let adapter = JsonlStorageAdapter::with_explicit_session_dir(
                fixture.directory.path().to_path_buf(),
            );
            adapter
                .persist_native_projection_and_ack(&fixture.info, fixture.write.clone())
                .await
                .unwrap();
            let selected_head = std::fs::read(native.join("authority.json")).unwrap();
            let selected_commit = native
                .join("blobs")
                .join(format!("{}.json", digest(&selected_head)));
            let selected_image = native
                .join("history")
                .join(format!("{}.jsonl", digest(&fixture.write.image_bytes)));
            let selected_metadata = native
                .join("blobs")
                .join(format!("{}.json", digest(&fixture.write.metadata_bytes)));
            let unknown = native.join("unknown-owner.tmp");
            native_test_owner_file(&unknown, b"unrelated evidence stays");
            let mut own = Vec::new();
            for (location, kinds) in [
                (
                    native.join("blobs"),
                    vec![
                        "source",
                        "active",
                        "ledger",
                        "snapshot",
                        "checkpoint",
                        "commit",
                        "metadata",
                    ],
                ),
                (native.join("history"), vec!["image"]),
                (
                    native.clone(),
                    vec![
                        "authority",
                        "materialized",
                        "current",
                        "staging",
                        "backup",
                        "required",
                    ],
                ),
                (fixture.directory.path().to_path_buf(), vec!["mirror"]),
            ] {
                for kind in kinds {
                    let bytes = format!("{{\"own_leftover_kind\":\"{kind}\"}}").into_bytes();
                    let path = location.join(format!(
                        ".s3-{kind}-{}-{}.tmp",
                        digest(&bytes),
                        "c".repeat(32)
                    ));
                    native_test_owner_file(&path, &bytes);
                    own.push(path);
                }
            }
            let source_ref = &fixture.write.checkpoint.record.source;
            let source_bytes = &fixture.write.checkpoint.blobs[&source_ref.digest];
            let source_temp = native.join("blobs").join(format!(
                ".s3-source-{}-{}.tmp",
                source_ref.digest,
                "d".repeat(32)
            ));
            native_test_owner_file(&source_temp, source_bytes);
            let pending_bytes = b"pending owned image";
            let pending_hash = digest(pending_bytes);
            let pending_temp = native
                .join("history")
                .join(format!(".s3-image-{pending_hash}-{}.tmp", "e".repeat(32)));
            native_test_owner_file(&pending_temp, pending_bytes);
            let mut pending = fixture.write.pending.clone();
            pending.dependencies.insert(pending_hash);
            pending.temporaries.insert(
                pending_temp
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned(),
            );
            assert_eq!(
                adapter.s3_cleanup(&native, None),
                Err(NativeUnavailable::Incomplete)
            );
            adapter.s3_cleanup(&native, Some(&pending)).unwrap();
            assert!(
                own.iter().all(|path| !path.exists()),
                "every exact own temporary kind is reconciled"
            );
            for path in [
                &unknown,
                &pending_temp,
                &source_temp,
                &selected_commit,
                &selected_image,
                &selected_metadata,
            ] {
                assert!(
                    path.exists(),
                    "selected/pending/unknown ownership stays protected: {path:?}"
                );
            }
            let full = adapter.load_session(&fixture.info).await.unwrap();
            let light = adapter
                .load_session_without_updates(&fixture.info)
                .await
                .unwrap();
            assert_eq!(
                serde_json::to_value(&full.chat_history).unwrap(),
                serde_json::to_value(&light.chat_history).unwrap()
            );
            adapter
                .read_native_history(&fixture.info)
                .await
                .unwrap()
                .checkpoint
                .unwrap()
                .matches_history(&full.chat_history)
                .unwrap();
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn native_projection_retirement_failure_never_revokes_ack() {
    use crate::session::acp_session::native_history_policy_tests::native_projection_disk_fixture;
    use xai_chat_state::persistence::native::digest;
    let fail = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let refusing = fail.clone();
    let retired = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let observed = retired.clone();
    crate::session::persistence::set_next_native_projection_probe(std::sync::Arc::new(
        move |phase| {
            if phase == "retirement" {
                observed.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                if refusing.load(std::sync::atomic::Ordering::SeqCst) {
                    return Err(io::Error::other("real post-ACK retirement failure"));
                }
            }
            Ok(())
        },
    ));
    tokio::task::LocalSet::new()
        .run_until(async {
            let mut fixture = native_projection_disk_fixture().await;
            let first = fixture
                .fifo_projection_ack(fixture.write.clone())
                .await
                .unwrap();
            fixture.fifo_flush().await;
            let next = fixture.next_projection().await;
            let ack = fixture.fifo_projection_ack(next.clone()).await.unwrap();
            fixture.fifo_flush().await;
            assert_eq!(ack.generation(), first.generation() + 1);
            let old_commit = fixture
                .directory
                .path()
                .join("native_history/blobs")
                .join(format!("{}.json", first.authority_digest()));
            let old_metadata = fixture
                .directory
                .path()
                .join("native_history/blobs")
                .join(format!("{}.json", digest(&fixture.write.metadata_bytes)));
            assert!(
                old_commit.exists() && old_metadata.exists(),
                "failed retirement retains the physical charge"
            );
            assert_eq!(
                fixture.fifo_projection_ack(next.clone()).await.unwrap(),
                ack
            );
            fixture.fifo_flush().await;
            assert!(old_commit.exists() && old_metadata.exists());
            assert!(retired.load(std::sync::atomic::Ordering::SeqCst) >= 3);
            fail.store(false, std::sync::atomic::Ordering::SeqCst);
            let third = fixture.next_projection().await;
            fixture.fifo_projection_ack(third.clone()).await.unwrap();
            fixture.fifo_flush().await;
            assert!(
                !old_commit.exists() && !old_metadata.exists(),
                "next new candidate reconciles unreferenced copies"
            );
            let adapter = JsonlStorageAdapter::with_explicit_session_dir(
                fixture.directory.path().to_path_buf(),
            );
            let full = adapter.load_session(&fixture.info).await.unwrap();
            let light = adapter
                .load_session_without_updates(&fixture.info)
                .await
                .unwrap();
            assert_eq!(
                serde_json::to_value(&full.chat_history).unwrap(),
                serde_json::to_value(&light.chat_history).unwrap()
            );
            adapter
                .read_native_history(&fixture.info)
                .await
                .unwrap()
                .checkpoint
                .unwrap()
                .matches_history(&full.chat_history)
                .unwrap();
        })
        .await;
}

#[derive(Clone)]
struct NativeLockEventLayer(
    std::sync::Arc<std::sync::Mutex<Vec<std::collections::BTreeMap<String, String>>>>,
);

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for NativeLockEventLayer {
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _context: tracing_subscriber::layer::Context<'_, S>,
    ) {
        struct Fields(std::collections::BTreeMap<String, String>);
        impl tracing::field::Visit for Fields {
            fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
                self.0.insert(field.name().into(), format!("{value:?}"));
            }
            fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
                self.0.insert(field.name().into(), value.into());
            }
            fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
                self.0.insert(field.name().into(), value.to_string());
            }
        }
        let mut fields = Fields(Default::default());
        event.record(&mut fields);
        if fields
            .0
            .get("classification")
            .is_some_and(|value| value == "native_lock_busy")
        {
            self.0.lock().unwrap().push(fields.0);
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn native_projection_serializes_ordinary_append_and_rewrite() {
    use crate::session::acp_session::native_history_policy_tests::native_projection_disk_fixture;
    use crate::session::persistence::PersistenceMsg;
    use xai_grok_sampling_types::conversation::native_search_history::*;
    use tracing_subscriber::prelude::*;
    tokio::task::LocalSet::new()
        .run_until(async {
            let fixture = native_projection_disk_fixture().await;
            let adapter = JsonlStorageAdapter::with_explicit_session_dir(
                fixture.directory.path().to_path_buf(),
            );
            fixture
                .fifo_projection_ack(fixture.write.clone())
                .await
                .unwrap();
            fixture.fifo_flush().await;
            let native = fixture.directory.path().join("native_history");
            let chat = fixture.directory.path().join("chat_history.jsonl");
            for lock_name in ["transaction", "chat"] {
                let path = if lock_name == "transaction" {
                    native.join("transaction.lock")
                } else {
                    chat.with_extension("jsonl.lock")
                };
                let lock = std::fs::OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(&path)
                    .unwrap();
                fs2::FileExt::lock_exclusive(&lock).unwrap();
                let events = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
                let dispatcher = tracing::Dispatch::new(
                    tracing_subscriber::registry().with(NativeLockEventLayer(events.clone())),
                );
                let held_native = native.clone();
                let held_chat = chat.clone();
                let error = tokio::task::spawn_blocking(move || {
                    tracing::dispatcher::with_default(&dispatcher, || {
                        NativeDiskLocks::acquire(
                            &held_native,
                            &held_chat,
                            "projection",
                            Some(
                                std::time::Instant::now() + std::time::Duration::from_millis(1000),
                            ),
                        )
                        .err()
                        .unwrap()
                    })
                })
                .await
                .unwrap();
                assert_eq!(native_io_error(error), NativeUnavailable::Indeterminate);
                let fields = events.lock().unwrap().last().unwrap().clone();
                assert_eq!(fields["classification"], "native_lock_busy");
                assert_eq!(fields["operation"], "projection");
                assert_eq!(fields["lock"], lock_name);
                assert_eq!(fields["timeout_ms"], "1000");
                assert_eq!(
                    adapter
                        .persist_native_projection_and_ack(&fixture.info, fixture.write.clone())
                        .await,
                    Err(NativeUnavailable::Indeterminate)
                );
                assert_eq!(
                    adapter
                        .load_session(&fixture.info)
                        .await
                        .unwrap_err()
                        .kind(),
                    io::ErrorKind::TimedOut
                );
                assert_eq!(
                    adapter
                        .load_session_without_updates(&fixture.info)
                        .await
                        .unwrap_err()
                        .kind(),
                    io::ErrorKind::TimedOut
                );
                assert_eq!(
                    adapter
                        .read_native_history(&fixture.info)
                        .await
                        .unwrap_err(),
                    NativeUnavailable::Indeterminate
                );
                fs2::FileExt::unlock(&lock).unwrap();
                adapter
                    .persist_native_projection_and_ack(&fixture.info, fixture.write.clone())
                    .await
                    .unwrap();
                adapter.load_session(&fixture.info).await.unwrap();
            }
            for class in ["Chat", "Replace", "CWD"] {
                let lock = std::fs::OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(native.join("transaction.lock"))
                    .unwrap();
                fs2::FileExt::lock_exclusive(&lock).unwrap();
                let before = std::fs::read(&chat).unwrap();
                let sender = fixture.fifo_sender();
                let mut expected =
                    xai_chat_state::persistence::native::strict_history_jsonl(&before).unwrap();
                let mut cwd_reply = None;
                match class {
                    "Chat" => {
                        let item = ConversationItem::assistant(
                            "ordinary Chat held beyond native deadline",
                        );
                        expected.push(item.clone());
                        sender.send(PersistenceMsg::Chat(item)).unwrap();
                    }
                    "Replace" => {
                        expected.push(ConversationItem::assistant(
                            "ordinary Replace held beyond native deadline",
                        ));
                        sender
                            .send(PersistenceMsg::ReplaceChatHistory(expected.clone()))
                            .unwrap();
                    }
                    "CWD" => {
                        let item = ConversationItem::working_directory_switch(
                            "ordinary CWD held beyond native deadline",
                            91,
                        );
                        expected.push(item.clone());
                        let (respond_to, reply) = tokio::sync::oneshot::channel();
                        sender
                            .send(PersistenceMsg::AppendCwdSwitchAndAck { item, respond_to })
                            .unwrap();
                        cwd_reply = Some(reply);
                    }
                    _ => unreachable!(),
                }
                let (respond_to, mut reply) = tokio::sync::oneshot::channel();
                sender
                    .send(PersistenceMsg::FlushAndAck { respond_to })
                    .unwrap();
                assert!(
                    tokio::time::timeout(std::time::Duration::from_millis(1100), &mut reply)
                        .await
                        .is_err(),
                    "ordinary {class} is blocking, never dropped at 1000 ms"
                );
                assert_eq!(std::fs::read(&chat).unwrap(), before);
                fs2::FileExt::unlock(&lock).unwrap();
                tokio::time::timeout(std::time::Duration::from_secs(30), reply)
                    .await
                    .unwrap()
                    .unwrap()
                    .unwrap();
                if let Some(reply) = cwd_reply {
                    assert!(reply.await.unwrap().is_ok());
                }
                let full = adapter.load_session(&fixture.info).await.unwrap();
                let light = adapter
                    .load_session_without_updates(&fixture.info)
                    .await
                    .unwrap();
                assert_eq!(
                    serde_json::to_value(&full.chat_history).unwrap(),
                    serde_json::to_value(&expected).unwrap(),
                    "ordinary {class} persists exactly once"
                );
                assert_eq!(
                    serde_json::to_value(&full.chat_history).unwrap(),
                    serde_json::to_value(&light.chat_history).unwrap()
                );
            }
            let before = std::fs::read(&chat).unwrap();
            let sender = fixture.fifo_sender();
            sender
                .send(PersistenceMsg::GuardedLegacyReplace(vec![
                    ConversationItem::user("stale synchronous legacy snapshot"),
                ]))
                .unwrap();
            fixture.fifo_flush().await;
            assert_eq!(
                std::fs::read(&chat).unwrap(),
                before,
                "shared synchronous legacy writer no-ops on native authority"
            );
            assert!(
                super::super::chat_rebuild::rebuild_chat_history(fixture.directory.path()).is_err(),
                "guarded rebuild cannot overwrite native history"
            );
            assert!(
                adapter
                    .copy_session_data_sync(&fixture.info, &fixture.info, Default::default())
                    .is_err(),
                "direct fork reaches the shared target guard"
            );
            assert!(
                adapter
                    .copy_session_data(&fixture.info, &fixture.info, Default::default())
                    .await
                    .is_err(),
                "wrapper reaches the same target guard"
            );
            assert_eq!(std::fs::read(&chat).unwrap(), before);
            let full = adapter.load_session(&fixture.info).await.unwrap();
            adapter
                .read_native_history(&fixture.info)
                .await
                .unwrap()
                .checkpoint
                .unwrap()
                .matches_history(&full.chat_history)
                .unwrap();
        })
        .await;
}
