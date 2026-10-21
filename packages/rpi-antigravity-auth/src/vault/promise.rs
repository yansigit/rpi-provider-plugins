//! Start a compatibility operation before returning its owned background completion.
use std::{
    future::Future,
    panic::{catch_unwind, AssertUnwindSafe},
    task::{Context, Poll},
};

pub(super) fn spawn_started<F>(future: F) -> tokio::task::JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    let mut future = Box::pin(future);
    let waker = futures_util::task::noop_waker();
    let first = catch_unwind(AssertUnwindSafe(|| {
        future.as_mut().poll(&mut Context::from_waker(&waker))
    }));
    tokio::spawn(async move {
        match first {
            Ok(Poll::Ready(value)) => value,
            Ok(Poll::Pending) => future.await,
            Err(panic) => std::panic::resume_unwind(panic),
        }
    })
}
