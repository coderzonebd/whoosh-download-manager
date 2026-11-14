// SPDX-License-Identifier: GPL-3.0-or-later
//! `whoosh://` links, e.g. `whoosh://add?url=https%3A%2F%2Fexample.com%2Ff.zip`.
//!
//! The OS hands them over in one of three ways: as the launch arguments of
//! the first instance ([`DeepLinkExt::get_current`]), from a second launch
//! through the single-instance plugin (its `deep-link` feature), or, on
//! macOS, as an Apple event to the running app. All three end in
//! [`receive`], which keeps the links until the UI takes them
//! ([`take_deep_links`]) and tells it to look ([`EVENT_DEEP_LINK`]). The UI
//! may not be listening yet at launch, so the links wait instead of riding
//! on the event. What a link means is decided by the UI, which checks it
//! (`parseDeepLink` in `@whoosh/ui`): this side only filters the scheme.

use std::sync::Mutex;

use tauri::{AppHandle, Emitter, Manager, Runtime, State};
use tauri_plugin_deep_link::DeepLinkExt;

/// Sent to the UI when links are waiting; it answers with `take_deep_links`.
pub const EVENT_DEEP_LINK: &str = "desktop://deep-link";

const SCHEME: &str = "whoosh:";
/// Longer links are dropped: no real download address needs more.
const MAX_LINK_BYTES: usize = 16 * 1024;
/// Links waiting for the UI; the oldest go first if something floods us.
const MAX_PENDING: usize = 16;

/// Links received but not yet taken by the UI.
#[derive(Debug, Default)]
pub struct Pending(Mutex<Vec<String>>);

/// Whether `link` is one of ours and of a sane length.
fn acceptable(link: &str) -> bool {
    link.len() <= MAX_LINK_BYTES
        && link
            .get(..SCHEME.len())
            .is_some_and(|s| s.eq_ignore_ascii_case(SCHEME))
}

/// Queue `links` for the UI. Returns how many were kept.
fn push(pending: &Pending, links: impl IntoIterator<Item = String>) -> usize {
    let mut queue = pending.0.lock().unwrap_or_else(|e| e.into_inner());
    let mut kept = 0;
    for link in links.into_iter().filter(|l| acceptable(l)) {
        queue.push(link);
        kept += 1;
    }
    let excess = queue.len().saturating_sub(MAX_PENDING);
    queue.drain(..excess);
    kept
}

/// Keep `links` for the UI, bring the window forward and tell the UI.
pub fn receive<R: Runtime>(app: &AppHandle<R>, links: impl IntoIterator<Item = String>) {
    if push(&app.state::<Pending>(), links) == 0 {
        return;
    }
    crate::show_main_window(app);
    let _ = app.emit(EVENT_DEEP_LINK, ());
}

/// Wires the plugin up: links from now on, and the ones we were launched with.
pub fn setup<R: Runtime>(app: &tauri::App<R>) {
    app.manage(Pending::default());
    // Installers register the scheme; a portable Windows zip, an AppImage or
    // a development build has to do it at run time.
    #[cfg(any(windows, target_os = "linux"))]
    if (cfg!(windows) || cfg!(debug_assertions) || std::env::var_os("APPIMAGE").is_some())
        && let Err(err) = app.deep_link().register_all()
    {
        eprintln!("whoosh-desktop: could not register whoosh:// links: {err}");
    }
    let handle = app.handle().clone();
    app.deep_link().on_open_url(move |event| {
        receive(&handle, event.urls().iter().map(|u| u.to_string()));
    });
    if let Ok(Some(urls)) = app.deep_link().get_current() {
        push(&app.state::<Pending>(), urls.iter().map(|u| u.to_string()));
    }
}

/// The links waiting for the UI, oldest first; each is returned once.
#[tauri::command]
pub fn take_deep_links(state: State<'_, Pending>) -> Vec<String> {
    std::mem::take(&mut *state.0.lock().unwrap_or_else(|e| e.into_inner()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_only_whoosh_links() {
        let p = Pending::default();
        let n = push(
            &p,
            [
                "whoosh://add?url=https%3A%2F%2Fexample.com%2Fa.zip".to_owned(),
                "WHOOSH://add?url=x".to_owned(),
                "https://example.com/".to_owned(),
                "whoo".to_owned(),
                format!("whoosh://add?url={}", "a".repeat(MAX_LINK_BYTES)),
            ],
        );
        assert_eq!(n, 2);
        assert_eq!(p.0.lock().unwrap().len(), 2);
    }

    #[test]
    fn drops_the_oldest_when_flooded() {
        let p = Pending::default();
        push(
            &p,
            (0..MAX_PENDING + 3).map(|i| format!("whoosh://add?n={i}")),
        );
        let queue = p.0.lock().unwrap();
        assert_eq!(queue.len(), MAX_PENDING);
        assert_eq!(queue[0], "whoosh://add?n=3");
    }
}
