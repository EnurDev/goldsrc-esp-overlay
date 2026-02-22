// lib.rs — DLL entry point for the GoldSrc diagnostic overlay.
//
// When this DLL is injected into hl.exe via LoadLibraryA, DllMain fires
// with DLL_PROCESS_ATTACH. It spawns a background worker thread that:
//   1. Installs a hook on the engine's Initialize function (to capture the engine table)
//   2. Installs a detour on wglSwapBuffers (to draw the ESP overlay each frame)
//   3. Stays alive until DLL_PROCESS_DETACH signals shutdown
//
// Must be compiled as a 32-bit cdylib (i686-pc-windows-msvc).

#![allow(non_snake_case)]

// Compile-time guard: only allow 32-bit x86 builds
#[cfg(not(target_arch = "x86"))]
compile_error!("Build with i686-pc-windows-msvc (32-bit x86).");

// Internal modules
mod entities; // Engine API access, memory reading, player data
mod esp;      // ESP drawing logic (bounding boxes, labels)
mod hook;     // wglSwapBuffers hook install/uninstall
mod math;     // Vector math (Vec3, distance)
mod render;   // OpenGL 2D drawing primitives (lines, text, boxes)

use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use winapi::shared::minwindef::{BOOL, DWORD, HINSTANCE, LPVOID, TRUE};
use winapi::um::handleapi::CloseHandle;
use winapi::um::libloaderapi::DisableThreadLibraryCalls;
use winapi::um::processthreadsapi::CreateThread;
use winapi::um::winnt::{DLL_PROCESS_ATTACH, DLL_PROCESS_DETACH};

/// Flag to keep the worker thread alive. Set to false on DLL_PROCESS_DETACH.
static RUNNING: AtomicBool = AtomicBool::new(false);

/// Background worker thread entry point.
/// Installs hooks, then loops until RUNNING is set to false (on DLL unload).
unsafe extern "system" fn worker(_: LPVOID) -> DWORD {
    // Brief delay to let the engine finish initializing
    std::thread::sleep(Duration::from_millis(500));

    // Install the wglSwapBuffers hook (which also triggers the Initialize hook)
    match hook::install() {
        Ok(()) => entities::log("hook installed"),
        Err(e) => {
            entities::logf(format!("hook install failed: err={}", e));
            entities::flush_log();
            return 1; // Exit thread on failure
        }
    }

    // Keep thread alive until DLL is unloaded
    RUNNING.store(true, Ordering::Release);
    while RUNNING.load(Ordering::Acquire) {
        std::thread::sleep(Duration::from_millis(50));
    }

    // Cleanup: remove hooks before thread exits
    hook::uninstall();
    0
}

/// DLL entry point — called by Windows when the DLL is loaded/unloaded.
#[no_mangle]
pub unsafe extern "system" fn DllMain(
    hinst: HINSTANCE,
    reason: DWORD,
    _reserved: LPVOID,
) -> BOOL {
    match reason {
        DLL_PROCESS_ATTACH => {
            // Prevent DLL_THREAD_ATTACH/DETACH notifications (we don't need them)
            DisableThreadLibraryCalls(hinst);

            // Save the DLL's module handle (used for resolving the log file path)
            entities::set_dll_hinst(hinst as usize);
            entities::log("DLL attached");
            entities::flush_log();

            // Spawn the worker thread that installs hooks
            let h = CreateThread(
                ptr::null_mut(), 0, Some(worker),
                ptr::null_mut(), 0, ptr::null_mut(),
            );
            if !h.is_null() {
                CloseHandle(h); // We don't need the thread handle
            }
        }
        DLL_PROCESS_DETACH => {
            // Signal the worker thread to stop
            RUNNING.store(false, Ordering::Release);
            entities::flush_log();
        }
        _ => {}
    }
    TRUE
}
