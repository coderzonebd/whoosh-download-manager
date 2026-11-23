// SPDX-License-Identifier: GPL-3.0-or-later
//! Giving memory back. While files download, each connection's buffers come
//! from the allocator; once every connection is gone the allocator keeps
//! the freed space for later, so a daemon that sat in the tray after a few
//! big downloads held twice what it did before them (8 downloads: 6.8 MB
//! before, 12.9 MB after, docs/bench/apps-vs-idm.ps1). When the network has
//! been quiet for a moment, the free space goes back to the system: the
//! process heap is compacted on Windows (`HeapCompact`), glibc trims its
//! arenas on Linux (`malloc_trim`).
#![allow(unsafe_code)]

use std::sync::Arc;
use std::time::Duration;

/// How often to look, and how long the network stays quiet first.
const LOOK: Duration = Duration::from_secs(5);

/// Gives the allocator's free space back to the system.
pub fn release() {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Memory::{GetProcessHeap, HeapCompact};
        // SAFETY: the process heap handle is always valid; HeapCompact only
        // coalesces and decommits free blocks.
        unsafe {
            HeapCompact(GetProcessHeap(), 0);
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

/// Watches `working` (the engine's connections at work) and gives memory
/// back each time it has dropped to zero and stayed there for a moment.
pub fn watch(working: Arc<dyn Fn() -> usize + Send + Sync>) {
    tokio::spawn(async move {
        let mut was_busy = false;
        let mut tick = tokio::time::interval(LOOK);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tick.tick().await;
            let busy = working() > 0;
            if was_busy && !busy {
                release();
            }
            was_busy = busy;
        }
    });
}
