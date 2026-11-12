//! Run blocking file-system calls off the async runtime.

use std::io;

/// Run `f` on tokio's blocking pool when called from inside a tokio runtime,
/// or inline otherwise (e.g. under `futures::executor::block_on`), so the
/// crate works on any executor but never blocks a tokio worker.
pub(crate) async fn run<T, F>(f: F) -> io::Result<T>
where
    F: FnOnce() -> io::Result<T> + Send + 'static,
    T: Send + 'static,
{
    match tokio::runtime::Handle::try_current() {
        Ok(h) => match h.spawn_blocking(f).await {
            Ok(r) => r,
            Err(e) if e.is_panic() => std::panic::resume_unwind(e.into_panic()),
            Err(_) => Err(io::Error::other("the runtime is shutting down")),
        },
        Err(_) => f(),
    }
}

/// Start `f` on tokio's blocking pool without waiting for it, or run it
/// inline outside a tokio runtime.
pub(crate) fn spawn<F>(f: F)
where
    F: FnOnce() + Send + 'static,
{
    match tokio::runtime::Handle::try_current() {
        Ok(h) => drop(h.spawn_blocking(f)),
        Err(_) => f(),
    }
}
