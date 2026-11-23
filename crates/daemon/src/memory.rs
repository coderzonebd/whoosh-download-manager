// SPDX-License-Identifier: GPL-3.0-or-later
//! Giving memory back. While files download, each connection's buffers come
//! from the allocator; once every connection is gone the allocator keeps
//! the freed space for later, so a daemon that sat in the tray after a few
//! big downloads held twice what it did before them (8 downloads: 6.8 MB
//! before, 12.9 MB after, docs/bench/apps-vs-idm.ps1). When the engine has
//! been quiet for a moment (no download left running, finishing included),
//! and once more a little later, the free space goes back to the system: the
//! process heap is compacted on Windows (`HeapCompact`), glibc trims its
//! arenas on Linux (`malloc_trim`). On Windows the pages it no longer
//! touches also leave RAM (`SetProcessWorkingSetSize(-1, -1)`, what Windows
//! does to an app whose window is minimized; the daemon has no window, so
//! it does it itself); they come back on use. Also once shortly after
//! start, for what starting up touched.
#![allow(unsafe_code)]

use std::sync::Arc;
use std::time::Duration;

/// How often to look, and how long the engine stays quiet first.
const LOOK: Duration = Duration::from_secs(5);
/// Quiet looks after which memory goes back again: what the end of the
/// downloads still touched (the last records saved, the sessions that
/// closed with them) is freed by then too.
const AGAIN_AFTER: [u32; 2] = [1, 4];

/// Gives the allocator's free space back to the system.
pub fn release() {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Memory::{GetProcessHeap, HeapCompact};
        use windows_sys::Win32::System::Threading::{GetCurrentProcess, SetProcessWorkingSetSize};
        // SAFETY: the process heap and current-process handles are always
        // valid; HeapCompact only coalesces and decommits free blocks, and
        // (-1, -1) asks Windows to trim this process's working set.
        unsafe {
            HeapCompact(GetProcessHeap(), 0);
            SetProcessWorkingSetSize(GetCurrentProcess(), usize::MAX, usize::MAX);
        }
    }
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    {
        unsafe extern "C" {
            fn malloc_trim(pad: usize) -> i32;
        }
        // SAFETY: glibc's malloc_trim takes a padding size and only returns
        // free memory at the top of the heap and in its arenas.
        unsafe {
            malloc_trim(0);
        }
    }
}

/// Whether the `quiet`-th quiet look in a row gives memory back.
fn releases_at(quiet: u32) -> bool {
    AGAIN_AFTER.contains(&quiet)
}

/// Watches `busy` (the engine's downloads and connections at work) and
/// gives memory back once it has dropped to zero and stayed there for a
/// moment, and again a little later.
pub fn watch(busy: Arc<dyn Fn() -> usize + Send + Sync>) {
    tokio::spawn(async move {
        // As if busy just before: the first quiet looks also give back what
        // starting took.
        let mut quiet: u32 = 0;
        let mut tick = tokio::time::interval(LOOK);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tick.tick().await;
            if busy() > 0 {
                quiet = 0;
                continue;
            }
            quiet = quiet.saturating_add(1);
            if releases_at(quiet) {
                release();
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gives_back_soon_and_once_more_then_rests() {
        let at: Vec<u32> = (1..=20).filter(|&q| releases_at(q)).collect();
        assert_eq!(at, vec![1, 4]);
    }
}
