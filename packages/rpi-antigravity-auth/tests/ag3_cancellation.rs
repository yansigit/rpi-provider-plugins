//! AG-3 cancellation: the approved body-read exception (timeout/cancel returns
//! the error and releases the request) and lease cancellation cleanup.
use rpi_antigravity_auth::{
    transport::network::Options,
    vault::{
        ceremony::Lease,
        file_lease::{FileLease, Options as LeaseOptions},
    },
};
use std::{
    fs,
    os::unix::fs::DirBuilderExt,
    path::PathBuf,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

#[path = "common/tls.rs"]
mod tls_fixture;
use tls_fixture::tls;

async fn head(socket: &mut (impl AsyncRead + Unpin)) -> Vec<u8> {
    let mut bytes = Vec::new();
    while !bytes.ends_with(b"\r\n\r\n") {
        bytes.push(socket.read_u8().await.unwrap());
    }
    bytes
}

/// Approved exception (user approval 2026-10-21): the original Node body read
/// stays pending after an idle timeout or cancellation; the native transport
/// instead returns the error and releases its request. Pin both halves: the
/// error text and the released socket.
#[tokio::test]
async fn body_read_timeout_and_cancel_return_the_error_and_release_the_request() {
    let (client, acceptor) = tls();
    for scenario in ["cancel", "idle"] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let acceptor = acceptor.clone();
        let released = Arc::new(tokio::sync::Notify::new());
        let done = released.clone();
        let server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let mut socket = acceptor.accept(socket).await.unwrap();
            head(&mut socket).await;
            assert_eq!(socket.read_u8().await.unwrap(), b'x');
            socket.write_all(b"HTTP/1.1 200 OK\r\n\r\nfirst").await.unwrap();
            // The read returns only once the native request drops its socket.
            let _ = socket.read_to_end(&mut Vec::new()).await;
            done.notify_one();
        });
        let opts = Options {
            timeout_ms: 2_000,
            idle_timeout_ms: if scenario == "cancel" { 0 } else { 100 },
            environment: Default::default(),
            ..Default::default()
        };
        let signal = opts.signal.clone();
        let mut response = client
            .fetch(
                &format!("https://localhost:{}/", address.port()),
                None,
                &[],
                b"x",
                opts,
            )
            .await
            .unwrap();
        assert_eq!(response.body.next().await.unwrap().unwrap(), b"first");
        if scenario == "cancel" {
            signal.cancel();
        }
        assert_eq!(
            response.body.next().await.err().unwrap(),
            if scenario == "cancel" {
                "The operation was aborted"
            } else {
                "Antigravity response stalled: no data for 100ms"
            },
            "{scenario}"
        );
        assert!(response.body.next().await.unwrap().is_none(), "{scenario}");
        drop(response);
        tokio::time::timeout(Duration::from_secs(2), released.notified())
            .await
            .unwrap_or_else(|_| panic!("{scenario}: the request did not release its socket"));
        tokio::time::timeout(Duration::from_secs(2), server)
            .await
            .unwrap()
            .unwrap();
    }
}

struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Cancellation releases the lock: stopping a renewing lease cancels its
/// renewal task, removes the lease and any staged write, and frees the path.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelling_a_renewing_lease_releases_the_lock_and_staged_files() {
    let scratch = Scratch(
        std::env::temp_dir().join(format!("ag3-cancel-{}", uuid::Uuid::new_v4())),
    );
    fs::DirBuilder::new().mode(0o700).create(&scratch.0).unwrap();
    let path = scratch.0.join("state.json");
    let clock = Arc::new(AtomicU64::new(100000));
    let now = clock.clone();
    let mut options = LeaseOptions::new(path.clone(), "ceremony", 30_000, true);
    options.now = Arc::new(move || now.load(Ordering::SeqCst) as f64);
    options.renewal_interval = Some(Duration::from_millis(10));
    let (sent, mut received) = tokio::sync::mpsc::unbounded_channel();
    options.step = Arc::new(move |step| {
        if step == "renewal-finished" {
            let _ = sent.send(());
        }
    });
    let lease = FileLease::acquire(options).await.unwrap().unwrap();
    clock.fetch_add(10_000, Ordering::SeqCst);
    tokio::time::timeout(Duration::from_secs(3), received.recv())
        .await
        .unwrap()
        .expect("a renewal commits before cancellation");
    lease.release().await.unwrap();
    let names: Vec<String> = fs::read_dir(&scratch.0)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        names.is_empty(),
        "cancellation left lease or staged files: {names:?}"
    );
    let successor = FileLease::acquire(LeaseOptions::new(path, "ceremony", 30_000, false))
        .await
        .unwrap()
        .expect("the cancelled lease frees the path");
    successor.assert_owned().await.unwrap();
    successor.release().await.unwrap();
    assert_eq!(fs::read_dir(&scratch.0).unwrap().count(), 0);
}
