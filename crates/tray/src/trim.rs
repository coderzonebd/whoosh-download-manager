// SPDX-License-Identifier: GPL-3.0-or-later
//! Giving memory back, for the tray: following the downloads (an event a
//! second, the tooltip redrawn) and showing a notification touch memory the
//! tray then holds for nothing, so once the downloads are over it hands
//! that back, as the daemon does (`wdm-daemon`'s `memory`): the free heap
//! goes back to the system and, on Windows, the pages it no longer touches
//! leave RAM (`SetProcessWorkingSetSize(-1, -1)`, what Windows does to a
//! minimized window; the tray has none). They come back on use.
#![allow(unsafe_code)]

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
