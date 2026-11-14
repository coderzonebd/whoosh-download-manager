//! Ctrl-C, the way `get` and `watch` see it.

/// Resolves when the user interrupts the command: Ctrl-C (`SIGINT`)
/// everywhere, and on Windows Ctrl-Break too. In a Windows console
/// Ctrl-Break is the harder interrupt (by default it kills the program
/// outright, leaving a `get` download running in the daemon), and the only
/// one that can be sent to a single process group, so it gets the same
/// orderly answer as Ctrl-C. If no handler can be installed, it never
/// resolves.
pub async fn interrupted() {
    #[cfg(windows)]
    if let Ok(mut ctrl_break) = tokio::signal::windows::ctrl_break() {
        tokio::select! {
            r = tokio::signal::ctrl_c() => {
                if r.is_err() {
                    ctrl_break.recv().await;
                }
            }
            _ = ctrl_break.recv() => {}
        }
        return;
    }
    if tokio::signal::ctrl_c().await.is_err() {
        std::future::pending::<()>().await;
    }
}
