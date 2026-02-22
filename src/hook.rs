// hook.rs — Manages the wglSwapBuffers detour lifecycle.
//
// This module hooks OpenGL's wglSwapBuffers function using MinHook.
// Every time the game finishes rendering a frame and calls wglSwapBuffers,
// our detour runs first, drawing the ESP overlay on top of the scene,
// then calls the original wglSwapBuffers to actually swap the buffers.
//
// Flow:
//   install()   -> Initialize MinHook -> Hook client.dll!Initialize -> Hook wglSwapBuffers
//   uninstall() -> Remove hooks -> Uninitialize MinHook
//   detour()    -> Called every frame -> esp::on_frame() -> original wglSwapBuffers

use crate::entities;
use crate::esp;
use minhook_sys::{
    MH_CreateHook, MH_DisableHook, MH_EnableHook,
    MH_Initialize, MH_OK, MH_RemoveHook, MH_Uninitialize,
};
use once_cell::sync::OnceCell;
use std::ffi::c_void;
use std::ptr;
use winapi::shared::minwindef::BOOL;
use winapi::shared::windef::HDC;
use winapi::um::libloaderapi::{GetModuleHandleA, GetProcAddress};

/// Function signature for the real wglSwapBuffers.
type WglSwapBuffersFn = unsafe extern "system" fn(HDC) -> BOOL;

/// Stores the original (unhooked) wglSwapBuffers function pointer.
static ORIGINAL: OnceCell<WglSwapBuffersFn> = OnceCell::new();

/// Stores the address of the hook target (for cleanup).
static TARGET: OnceCell<usize> = OnceCell::new();

/// Install all hooks: engine Initialize hook + wglSwapBuffers detour.
pub unsafe fn install() -> Result<(), i32> {
    // Initialize the MinHook library
    let s = MH_Initialize();
    if s != MH_OK { return Err(s); }

    // Hook client.dll's Initialize export to capture the engine function table.
    // This gives us access to engine APIs like GetLocalPlayer, GetEntityByIndex, etc.
    entities::install_initialize_hook();

    // Locate wglSwapBuffers in the already-loaded opengl32.dll
    let ogl = GetModuleHandleA(b"opengl32.dll\0".as_ptr() as _);
    if ogl.is_null() { return Err(-1); }
    let swap = GetProcAddress(ogl, b"wglSwapBuffers\0".as_ptr() as _);
    if swap.is_null() { return Err(-2); }

    // Create a MinHook detour: swap -> our detour, saving the original
    let mut original = ptr::null_mut::<c_void>();
    let s = MH_CreateHook(swap as *mut c_void, detour as *mut c_void, &mut original);
    if s != MH_OK { return Err(s); }

    // Save the original function pointer and target address
    let _ = ORIGINAL.set(std::mem::transmute::<*mut c_void, WglSwapBuffersFn>(original));
    let _ = TARGET.set(swap as usize);

    // Activate the hook (starts redirecting calls)
    let s = MH_EnableHook(swap as *mut c_void);
    if s != MH_OK { return Err(s); }

    Ok(())
}

/// Remove all hooks and shut down MinHook.
pub unsafe fn uninstall() {
    if let Some(&addr) = TARGET.get() {
        let p = addr as *mut c_void;
        MH_DisableHook(p);  // Stop redirecting calls
        MH_RemoveHook(p);   // Free the trampoline
    }
    MH_Uninitialize();
}

/// Our detour function — called every frame instead of the real wglSwapBuffers.
/// Draws the ESP overlay, then calls the original to actually swap buffers.
unsafe extern "system" fn detour(hdc: HDC) -> BOOL {
    // catch_unwind prevents panics in our overlay code from crashing the game
    let _ = std::panic::catch_unwind(|| {
        esp::on_frame(hdc);
    });

    // Call the original wglSwapBuffers to display the frame
    match ORIGINAL.get() {
        Some(f) => f(hdc),
        None    => 1, // Fallback: pretend success
    }
}
