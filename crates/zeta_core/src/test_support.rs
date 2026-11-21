//! Helpers shared by unit tests in this crate.

use std::future::Future;
use std::pin::pin;
use std::task::{Context, Poll, Waker};

/// Runs a future that completes without waiting on any input or output.
///
/// Test providers in this crate answer from memory, so their futures are ready on the
/// first poll. This avoids an async runtime dependency in `zeta_core`.
pub(crate) fn block_on<F: Future>(future: F) -> F::Output {
    let mut future = pin!(future);
    let mut context = Context::from_waker(Waker::noop());
    match future.as_mut().poll(&mut context) {
        Poll::Ready(output) => output,
        Poll::Pending => panic!("test future waited on input or output"),
    }
}
