//! Exercise the real package-to-host C emit boundary, not a replacement sink.
use super::*;
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

struct BlockedHost {
    entered: mpsc::Sender<()>,
    release: Mutex<mpsc::Receiver<()>>,
}

extern "C" fn blocked_emit(frame: StbString, data: *mut c_void) -> i32 {
    // SAFETY: Sink hands this callback an owned frame and the test keeps data
    // alive until the emitter thread is joined.
    unsafe {
        sdk::callbacks::free_string(frame);
        let host = &*(data as *const BlockedHost);
        let _ = host.entered.send(());
        match host
            .release
            .lock()
            .unwrap()
            .recv_timeout(Duration::from_secs(5))
        {
            Ok(()) => 1,
            Err(_) => 0,
        }
    }
}

#[test]
fn cancellation_while_in_synchronous_c_emit_propagates_host_rejection() {
    let (entered, waiting) = mpsc::channel();
    let (release, resume) = mpsc::channel();
    let host = Arc::new(BlockedHost {
        entered,
        release: Mutex::new(resume),
    });
    let signal = CancellationToken::new();
    let caller = signal.clone();
    let (finished, result) = mpsc::channel();
    let thread = std::thread::spawn(move || {
        let mut sink = Sink {
            emit: Some(blocked_emit),
            context: Arc::as_ptr(&host) as usize,
            signal,
        };
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let emitted = runtime.block_on(sink.emit(serde_json::json!({"type":"start"})));
        finished
            .send((emitted, sink.signal.is_cancelled()))
            .unwrap();
    });
    waiting.recv_timeout(Duration::from_secs(5)).unwrap();
    caller.cancel();
    assert!(
        matches!(result.try_recv(), Err(mpsc::TryRecvError::Empty)),
        "a synchronous C callback cannot be preempted by cancelling its token"
    );
    release.send(()).unwrap();
    let (emitted, cancelled) = result.recv_timeout(Duration::from_secs(5)).unwrap();
    thread.join().unwrap();
    assert_eq!(emitted, Err("This operation was aborted".into()));
    assert!(cancelled);
}
