use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Waker};

use istmo_core::{CancelToken, Runtime};
use istmo_share::{
    IncomingFile, IncomingShare, Share, ShareCapabilities, ShareClient, ShareError, ShareFile,
    ShareFileSource, ShareHost, ShareInbox, ShareOutcome, SharePreview, ShareRequest, ShareTarget,
};

/// Records every request and answers with a fixed outcome. Requests
/// with the subject `"hang"` wait for cancellation instead.
#[derive(Debug, Clone, Default)]
struct RecordingBackend {
    seen: Arc<Mutex<Vec<ShareRequest>>>,
    targets: Arc<Mutex<Vec<ShareTarget>>>,
    cancelled: Arc<AtomicBool>,
}

impl Share for RecordingBackend {
    async fn share(
        &self,
        cancel: CancelToken,
        request: ShareRequest,
    ) -> Result<ShareOutcome, ShareError> {
        request.validate()?;
        if request.subject.as_deref() == Some("hang") {
            cancel.cancelled().await;
            self.cancelled.store(true, Ordering::SeqCst);
            return Ok(ShareOutcome::Dismissed);
        }
        let target = request.subject.clone();
        self.seen.lock().expect("seen").push(request);
        Ok(ShareOutcome::Shared(target))
    }

    async fn staging_dir(&self) -> Result<String, ShareError> {
        Ok(std::env::temp_dir().display().to_string())
    }

    async fn set_share_targets(&self, targets: Vec<ShareTarget>) -> Result<(), ShareError> {
        *self.targets.lock().expect("targets") = targets;
        Ok(())
    }

    async fn capabilities(&self) -> Result<ShareCapabilities, ShareError> {
        Ok(ShareCapabilities {
            send: true,
            files: true,
            mixed_content: true,
            ..ShareCapabilities::default()
        })
    }
}

fn build_runtime(backend: RecordingBackend) -> (Arc<Runtime>, ShareClient) {
    let init = Runtime::mock()
        .expects::<ShareClient>()
        .host(ShareHost::new(backend))
        .finish();
    let client = ShareClient::from_runtime(&init.runtime).expect("client declared");
    (init.runtime, client)
}

#[test]
fn mixed_request_round_trips_through_the_wire() {
    let backend = RecordingBackend::default();
    let (_rt, client) = build_runtime(backend.clone());
    let request = ShareRequest::text("look at this")
        .with_url("https://istmo.dev")
        .with_subject("istmo")
        .with_file(ShareFile::bytes("notes.txt", b"hi".to_vec()))
        .with_file(ShareFile::path("/tmp/photo.png").with_mime_type("image/png"))
        .with_preview(SharePreview {
            title: Some("Preview".to_owned()),
            thumbnail: None,
            fetch_link_metadata: true,
        });

    let outcome = pollster::block_on(client.share(request.clone())).expect("share");
    assert_eq!(outcome, ShareOutcome::Shared(Some("istmo".to_owned())));
    assert_eq!(*backend.seen.lock().expect("seen"), [request]);

    let caps = pollster::block_on(client.capabilities()).expect("capabilities");
    assert!(caps.send && caps.files && caps.mixed_content);
    assert!(!caps.receive);
}

#[test]
fn domain_errors_decode_into_share_error() {
    let (_rt, client) = build_runtime(RecordingBackend::default());
    let err = pollster::block_on(client.share(ShareRequest::default())).expect_err("empty");
    assert!(
        matches!(ShareError::from(err), ShareError::InvalidRequest(_)),
        "empty requests are rejected by validate()"
    );
}

#[test]
fn unnamed_bytes_are_invalid() {
    let request = ShareRequest::files([ShareFile {
        source: ShareFileSource::Bytes(vec![1, 2, 3]),
        name: None,
        mime_type: None,
    }]);
    assert!(matches!(
        request.validate(),
        Err(ShareError::InvalidRequest(_))
    ));
}

#[test]
fn text_and_url_merge_without_duplicates() {
    let both = ShareRequest::text("hi").with_url("https://a.b");
    assert_eq!(both.text_with_url().as_deref(), Some("hi\nhttps://a.b"));
    let already = ShareRequest::text("see https://a.b").with_url("https://a.b");
    assert_eq!(already.text_with_url().as_deref(), Some("see https://a.b"));
    assert_eq!(
        ShareRequest::url("https://a.b").text_with_url().as_deref(),
        Some("https://a.b")
    );
}

#[test]
fn file_metadata_defaults() {
    let file = ShareFile::path("/data/report.PDF");
    assert_eq!(file.display_name(), Some("report.PDF"));
    assert_eq!(file.effective_mime_type(), "application/pdf");
    let named = ShareFile::bytes("blob", vec![]).with_mime_type("application/x-custom");
    assert_eq!(named.effective_mime_type(), "application/x-custom");
    assert_eq!(
        ShareFile::bytes("blob", vec![]).effective_mime_type(),
        "application/octet-stream"
    );
}

#[test]
fn inbox_buffers_until_first_subscriber_and_cleans_up() {
    let (rt, _client) = build_runtime(RecordingBackend::default());
    let inbox = ShareInbox::from_runtime(&rt).expect("inbox");

    let file_path =
        std::env::temp_dir().join(format!("istmo-share-inbox-{}.txt", std::process::id()));
    std::fs::write(&file_path, b"shared").expect("write");
    inbox
        .publish(IncomingShare {
            text: Some("cold start".to_owned()),
            files: vec![IncomingFile {
                path: file_path.display().to_string(),
                name: "a.txt".to_owned(),
                mime_type: Some("text/plain".to_owned()),
                size: Some(6),
            }],
            ..IncomingShare::default()
        })
        .expect("publish before subscribe");

    let stream = inbox.stream();
    let share = stream.try_recv().expect("buffered").expect("decoded");
    assert_eq!(share.text.as_deref(), Some("cold start"));
    assert!(share.received_at_ms.is_some(), "publish stamps the time");

    ShareInbox::cleanup(&share).expect("cleanup");
    assert!(!file_path.exists());
    ShareInbox::cleanup(&share).expect("cleanup is idempotent");
}

#[test]
fn inbox_requires_declared_plugin() {
    let init = Runtime::mock().finish();
    assert!(ShareInbox::from_runtime(&init.runtime).is_err());
}

#[cfg(target_os = "linux")]
#[test]
fn linux_backend_declines_and_reports_no_capabilities() {
    let backend = istmo_share::DesktopShare::new(Arc::new(istmo_window::WindowRegistry::default()))
        .expect("backend");
    let caps = pollster::block_on(backend.capabilities()).expect("capabilities");
    assert_eq!(caps, ShareCapabilities::default());
    let err = pollster::block_on(backend.share(CancelToken::new(), ShareRequest::text("hi")))
        .expect_err("unsupported");
    assert!(matches!(err, ShareError::Unsupported(_)));
}

#[test]
fn dropping_the_client_future_cancels_the_backend() {
    let backend = RecordingBackend::default();
    let (_rt, client) = build_runtime(backend.clone());
    {
        let mut call = std::pin::pin!(client.share(ShareRequest::text("x").with_subject("hang")));
        let mut cx = Context::from_waker(Waker::noop());
        assert!(call.as_mut().poll(&mut cx).is_pending());
    }
    for _ in 0..200 {
        if backend.cancelled.load(Ordering::SeqCst) {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    panic!("backend never observed the cancellation");
}

#[test]
fn share_targets_round_trip() {
    let backend = RecordingBackend::default();
    let (_rt, client) = build_runtime(backend.clone());
    let targets = vec![
        ShareTarget::new("chat-1", "Family"),
        ShareTarget::new("chat-2", "Work").with_icon(ShareFile::bytes("w.png", vec![1])),
    ];
    pollster::block_on(client.set_share_targets(targets.clone())).expect("set");
    assert_eq!(*backend.targets.lock().expect("targets"), targets);
}
