// entities.rs — Engine API interaction, memory reading, and player data extraction.
//
// This is the core module that interfaces with the GoldSrc engine. It:
//   1. Hooks client.dll's Initialize() export to capture the engine function table
//   2. Reads player entity data directly from process memory
//   3. Manages a debug log file for diagnostics
//
// The "engine function table" (gEngfuncs / cl_enginefunc_t) is a struct of function
// pointers that the engine passes to client.dll. By capturing it, we can call
// engine functions like GetLocalPlayer(), GetEntityByIndex(), world_to_screen(), etc.

#![allow(dead_code)]
#![allow(static_mut_refs)]

use crate::math::Vec3;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use winapi::um::libloaderapi::{GetModuleHandleA, GetModuleFileNameA, GetProcAddress};
use winapi::um::psapi::{GetModuleInformation, MODULEINFO};
use winapi::um::processthreadsapi::GetCurrentProcess;
use winapi::um::memoryapi::{VirtualQuery, VirtualProtect};
use winapi::um::winnt::{
    MEMORY_BASIC_INFORMATION, PAGE_READONLY, PAGE_READWRITE, PAGE_WRITECOPY,
    PAGE_EXECUTE_READ, PAGE_EXECUTE_READWRITE, PAGE_EXECUTE_WRITECOPY, MEM_COMMIT,
    PAGE_EXECUTE_READWRITE as PAGE_RWX,
};

// ============================================================
// Engine Function Table Slot Indices
// ============================================================
// The engine table is an array of function pointers. Each slot index
// corresponds to a specific engine API function.

const SLOT_GET_LOCAL_PLAYER:    usize = 51;  // cl_enginefunc_t::GetLocalPlayer
const SLOT_GET_ENTITY_BY_INDEX: usize = 53;  // cl_enginefunc_t::GetEntityByIndex
const SLOT_GET_PLAYER_INFO:     usize = 21;  // cl_enginefunc_t::pfnGetPlayerInfo
const SLOT_GET_MODEL_BY_INDEX:  usize = 107; // cl_enginefunc_t::pfnGetModelByIndex
const SLOT_PTRIAPI:             usize = 82;  // cl_enginefunc_t::pTriAPI (triangles API, has W2S)


const MAX_CLIENTS: i32 = 32; // Maximum player slots in GoldSrc

// ============================================================
// Entity Structure Offsets
// ============================================================
// These are byte offsets into the engine's cl_entity_t structure.
// They vary by engine build — these are for Build 4554.

const CURSTATE_OFFSET: usize = 0x2B0;  // Offset to entity_state_t (current state)
const ENT_ORIGIN:      usize = 0xB48;  // cl_entity_t::origin (interpolated position)
const ENT_CURPOS:      usize = 0x404;  // Current position history index
const ENT_PH_BASE:     usize = 0x408;  // Start of position history array
const PH_ENTRY_SIZE:   usize = 28;     // Size of one position history entry
const PH_HISTORY_MASK: usize = 63;     // Bitmask for position history ring buffer index

// Entity state sub-offsets (relative to CURSTATE_OFFSET)
const ES_ORIGIN:       usize = 0x10;   // entity_state_t::origin
const ES_WEAPONMODEL:  usize = 0xB4;   // entity_state_t::weaponmodel (model index)
const ES_MAXS:         usize = 0x88;   // entity_state_t::maxs (bounding box top)
const ES_USEHULL:      usize = 0xC8;   // entity_state_t::usehull (0=standing, 1=ducking)

// ============================================================
// Player Extra Info Offsets
// ============================================================
// g_PlayerExtraInfo is client.dll's per-player metadata array.
// Used to get team numbers and alive/dead status.

const EXTRA_OFF_TEAMNUMBER: usize = 0x2A;  // Team number (1=T, 2=CT)
const EXTRA_OFF_DEAD:       usize = 0x3C;  // Dead flag (0=alive, nonzero=dead)
const EXTRA_STRIDE:         usize = 0x68;  // Size of one extra_player_info_t entry

// ============================================================
// Global State
// ============================================================

/// Address of the engine function table (cl_enginefunc_t*).
static ENGINE_TABLE: AtomicUsize = AtomicUsize::new(0);

/// Whether a map has been loaded (engine table is valid).
static MAP_LOADED: AtomicBool = AtomicBool::new(false);

/// Whether our Initialize hook has been installed.
static HOOK_INSTALLED: AtomicBool = AtomicBool::new(false);

/// Hook status code (0 = success, 0xDEAD = not attempted, 0xE0xx = error).
static HOOK_STATUS: AtomicUsize = AtomicUsize::new(0xDEAD);

/// Cached address of g_PlayerExtraInfo array.
static EXTRA_INFO_BASE: AtomicUsize = AtomicUsize::new(0);

/// Frame counter for staleness tracking.
static FRAME_COUNTER: AtomicU32 = AtomicU32::new(0);

/// How many frames before a player's cached origin is considered stale.
const ORIGIN_STALE_FRAMES: u32 = 30;

/// Per-player cached data for origin staleness detection.
static mut LAST_KNOWN_ORIGIN: [Vec3; 33] = [Vec3 { x: 0.0, y: 0.0, z: 0.0 }; 33];
static mut LAST_CURPOS: [usize; 33] = [0usize; 33];
static mut LAST_CURPOS_FRAME: [u32; 33] = [0u32; 33];



// ============================================================
// Initialize Hook (captures the engine function table)
// ============================================================

/// Trampoline buffer for the JMP hook (stores original bytes + jump-back).
static mut TRAMPOLINE: [u8; 16] = [0u8; 16];

/// Address of the original Initialize function (for restoration).
static INIT_TARGET: AtomicUsize = AtomicUsize::new(0);

/// Function signature of client.dll's Initialize export.
type FnInitialize = unsafe extern "C" fn(eng: *mut u8, version: i32) -> i32;

/// Our replacement for Initialize — captures the engine table pointer, then
/// calls the original Initialize so the game continues normally.
unsafe extern "C" fn hk_initialize(eng: *mut u8, version: i32) -> i32 {
    logf(format!("hk_initialize: eng={:08X} ver={}", eng as usize, version));
    // Call the original Initialize via our trampoline
    let tramp: FnInitialize = std::mem::transmute(TRAMPOLINE.as_ptr());
    let ret = tramp(eng, version);

    // Save the engine table pointer for later use
    if !eng.is_null() {
        ENGINE_TABLE.store(eng as usize, Ordering::Release);
        MAP_LOADED.store(true, Ordering::Release);
        flush_log();
    }
    ret
}

/// Write a 5-byte JMP instruction at `from` that jumps to `to`.
/// Used to redirect function calls (inline hooking).
unsafe fn write_jmp(from: usize, to: usize) -> bool {
    // Make the target memory writable
    let mut old: u32 = 0;
    if VirtualProtect(from as *mut _, 5, PAGE_RWX, &mut old) == 0 { return false; }

    let p = from as *mut u8;
    *p = 0xE9; // JMP rel32 opcode
    let rel = (to as i64 - from as i64 - 5) as i32; // Calculate relative offset
    std::ptr::write_unaligned(p.add(1) as *mut i32, rel);

    // Restore original memory protection
    VirtualProtect(from as *mut _, 5, old, &mut old);
    true
}

/// Install the Initialize hook to capture the engine function table.
/// Tries two approaches:
///   1. Memory scan for the engine table (works if already initialized)
///   2. JMP hook on client.dll!Initialize (catches future map loads)
pub unsafe fn install_initialize_hook() {
    if HOOK_INSTALLED.load(Ordering::Relaxed) { return; }

    // Get client.dll's base address
    let client = GetModuleHandleA(b"client.dll\0".as_ptr() as _);
    if client.is_null() {
        log("client.dll not loaded yet");
        flush_log();
        return;
    }

    // Get the address of the Initialize export
    let init_addr = GetProcAddress(client, b"Initialize\0".as_ptr() as _) as usize;
    if init_addr == 0 {
        HOOK_STATUS.store(0xE001, Ordering::Relaxed);
        log("Initialize not exported");
        flush_log();
        return;
    }

    // Try to find the engine table via memory scanning first
    // (this works if the map is already loaded when we inject)
    if let Some(table) = find_gengfuncs_in_client() {
        ENGINE_TABLE.store(table, Ordering::Release);
        MAP_LOADED.store(true, Ordering::Release);
        HOOK_STATUS.store(0, Ordering::Relaxed);
        HOOK_INSTALLED.store(true, Ordering::Relaxed);
        log("engine table found via memory scan");
        flush_log();
        return;
    }

    // Memory scan failed — install a JMP hook on Initialize
    // so we catch the engine table when the next map loads
    if !is_readable(init_addr, 5) {
        HOOK_STATUS.store(0xE002, Ordering::Relaxed);
        log("Initialize not readable");
        flush_log();
        return;
    }

    // Build the trampoline: save original 5 bytes + JMP back
    let src = init_addr as *const u8;
    for i in 0..5usize { TRAMPOLINE[i] = *src.add(i); }
    TRAMPOLINE[5] = 0xE9; // JMP opcode
    let tramp_ptr = TRAMPOLINE.as_ptr() as usize;
    let rel_back = (init_addr as i64 + 5 - tramp_ptr as i64 - 10) as i32;
    std::ptr::write_unaligned(TRAMPOLINE.as_mut_ptr().add(6) as *mut i32, rel_back);

    // Make the trampoline executable
    let mut old: u32 = 0;
    VirtualProtect(TRAMPOLINE.as_mut_ptr() as *mut _, 16, PAGE_RWX, &mut old);

    INIT_TARGET.store(init_addr, Ordering::Relaxed);

    // Overwrite Initialize's first 5 bytes with a JMP to our hook
    if !write_jmp(init_addr, hk_initialize as *const () as usize) {
        HOOK_STATUS.store(0xE003, Ordering::Relaxed);
        log("write_jmp failed");
        flush_log();
        return;
    }

    HOOK_STATUS.store(0, Ordering::Relaxed);
    HOOK_INSTALLED.store(true, Ordering::Relaxed);
    log("Initialize JMP hook installed");
    flush_log();
}



// ============================================================
// Frame Counter (for staleness tracking)
// ============================================================

/// Update the frame counter (called by esp.rs each frame).
pub fn set_frame_counter(f: u32) {
    FRAME_COUNTER.store(f, Ordering::Relaxed);
}

// ============================================================
// Debug Logging
// ============================================================

use std::sync::Mutex;

/// Accumulated log lines (written to file on flush).
static LOG_LINES: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Module handle of our DLL (used to determine log file path).
static DLL_HINST: AtomicUsize = AtomicUsize::new(0);

/// Store the DLL's module handle (called from DllMain).
pub fn set_dll_hinst(h: usize) { DLL_HINST.store(h, Ordering::Relaxed); }

/// Append a log message and flush to file.
pub fn log(s: &str) {
    if let Ok(mut v) = LOG_LINES.lock() {
        v.push(s.to_string());
        let _ = flush_log_inner(&v);
    }
}

/// Append a formatted log message and flush to file.
pub fn logf(s: String) {
    if let Ok(mut v) = LOG_LINES.lock() {
        v.push(s);
        let _ = flush_log_inner(&v);
    }
}

/// Get the log file path (next to the DLL file, named "esp_debug.log").
fn log_path() -> std::path::PathBuf {
    let hinst = DLL_HINST.load(Ordering::Relaxed);
    if hinst != 0 {
        let mut buf = [0u8; 512];
        let len = unsafe {
            GetModuleFileNameA(hinst as _, buf.as_mut_ptr() as _, buf.len() as u32)
        } as usize;
        if len > 0 {
            if let Ok(s) = std::str::from_utf8(&buf[..len]) {
                if let Some(dir) = std::path::Path::new(s).parent() {
                    return dir.join("esp_debug.log");
                }
            }
        }
    }
    std::path::PathBuf::from("esp_debug.log")
}

/// Write all accumulated log lines to the log file (overwrites each time).
fn flush_log_inner(lines: &[String]) -> std::io::Result<()> {
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .create(true).write(true).truncate(true)
        .open(log_path())?;
    for l in lines { writeln!(f, "{}", l)?; }
    Ok(())
}

/// Flush the log file to disk.
pub fn flush_log() {
    if let Ok(v) = LOG_LINES.lock() { let _ = flush_log_inner(&v); }
}

// ============================================================
// Engine API Wrapper
// ============================================================

/// Function signatures for engine API calls.
type FnGetLocalPlayer   = unsafe extern "C" fn() -> *mut u8;
type FnGetEntityByIndex = unsafe extern "C" fn(idx: i32) -> *mut u8;
type FnGetPlayerInfo    = unsafe extern "C" fn(idx: i32, info: *mut HudPlayerInfo);

/// HUD player info structure (returned by engine's GetPlayerInfo).
#[repr(C)]
struct HudPlayerInfo {
    name:        *const i8,   // Player display name
    ping:        i16,         // Network ping
    thisplayer:  u8,          // 1 if this is the local player
    spectator:   u8,          // 1 if spectating
    packetloss:  u8,          // Packet loss percentage
    model:       *const i8,   // Player model name
    topcolor:    i16,         // Player model top color
    bottomcolor: i16,         // Player model bottom color
    steam_id:    u64,         // Steam ID
}

/// Player data extracted from the engine (public struct used by esp.rs).
#[derive(Clone, Default)]
pub struct PlayerData {
    pub origin:     Vec3,     // World position
    pub maxs_z:     f32,      // Bounding box height (from maxs.z)
    pub team:       i32,      // Team number (1=T, 2=CT)
    pub name:       String,   // Display name
    pub weapon:     String,   // Current weapon name
    pub is_local:   bool,     // Is this the local player?
    pub is_ducking: bool,     // Is the player crouching?
}

/// High-level wrapper around the engine function table.
pub struct EngineApi { table: usize }

impl EngineApi {
    /// Try to resolve the engine API. Returns None if:
    ///   - The hook isn't installed yet
    ///   - No map is loaded
    ///   - The engine table is invalid
    pub unsafe fn resolve() -> Option<Self> {
        // Install the Initialize hook if not done yet
        if !HOOK_INSTALLED.load(Ordering::Relaxed) {
            install_initialize_hook();
        }
        if !MAP_LOADED.load(Ordering::Acquire) { return None; }

        let table = ENGINE_TABLE.load(Ordering::Acquire);
        if table == 0 { return None; }

        // Validate that key slots contain valid function pointers
        let s51 = read_u32(table + SLOT_GET_LOCAL_PLAYER * 4);
        let s53 = read_u32(table + SLOT_GET_ENTITY_BY_INDEX * 4);
        if s51 == 0 || s53 == 0 { return None; }

        // Try to find g_PlayerExtraInfo if not cached yet
        if EXTRA_INFO_BASE.load(Ordering::Relaxed) == 0 {
            get_extra_info_base();
        }

        Some(Self { table })
    }

    /// Whether a map is currently loaded.
    pub fn map_loaded() -> bool { MAP_LOADED.load(Ordering::Acquire) }

    /// Get the local player's world position.
    pub unsafe fn local_origin(&self) -> Option<Vec3> {
        let fn_ptr = read_u32(self.table + SLOT_GET_LOCAL_PLAYER * 4) as usize;
        if fn_ptr == 0 { return None; }
        let f: FnGetLocalPlayer = std::mem::transmute(fn_ptr);
        let ent = f();
        if ent.is_null() { return None; }

        let o = read_vec3(ent as usize + ENT_ORIGIN);
        if o.is_zero() { return None; }
        Some(o)
    }

    /// Read all relevant data for a specific player by slot index.
    /// Returns None for invalid, dead, spectating, or unresolvable players.
    pub unsafe fn read_player(&self, idx: i32) -> Option<PlayerData> {
        if idx <= 0 || idx > MAX_CLIENTS { return None; }

        // --- Get player info (name, spectator status) ---
        let mut pinfo: HudPlayerInfo = std::mem::zeroed();
        let mut name: Option<String> = None;
        if let Some(f_info) = self.get_player_info_fn() {
            f_info(idx, &mut pinfo as *mut HudPlayerInfo);
            name = read_cstr(pinfo.name, 32);
            if name.is_none() { return None; }        // No name = slot is empty
            if pinfo.spectator != 0 { return None; }  // Skip spectators
        }

        // --- Get the entity pointer ---
        let fn_ptr = read_u32(self.table + SLOT_GET_ENTITY_BY_INDEX * 4) as usize;
        if fn_ptr == 0 { return None; }
        let f: FnGetEntityByIndex = std::mem::transmute(fn_ptr);
        let ent = f(idx);
        if ent.is_null() { return None; }
        let base = ent as usize;

        // Validate entity index and player flag
        let ent_index = read_i32(base + 0x00);
        let is_player = read_i32(base + 0x04);
        if is_player == 0 { return None; }
        if ent_index > 0 && ent_index <= MAX_CLIENTS && ent_index != idx { return None; }

        let cs = base + CURSTATE_OFFSET; // entity_state_t pointer

        // --- Resolve player origin (with multiple fallbacks) ---
        // Try: interpolated origin -> position history -> entity state origin
        let mut origin = read_vec3(base + ENT_ORIGIN);
        if !origin.x.is_finite() || !origin.y.is_finite() || !origin.z.is_finite() || origin.is_zero() {
            // Fallback 1: position history ring buffer
            let cur_pos = read_i32(base + ENT_CURPOS) as usize & PH_HISTORY_MASK;
            let ph_addr = base + ENT_PH_BASE + cur_pos * PH_ENTRY_SIZE;
            let ph_origin = read_vec3(ph_addr + 4);
            if ph_origin.x.is_finite() && ph_origin.y.is_finite() && ph_origin.z.is_finite() && !ph_origin.is_zero() {
                origin = ph_origin;
            } else {
                // Fallback 2: entity state origin
                let cs_origin = read_vec3(cs + ES_ORIGIN);
                if cs_origin.x.is_finite() && cs_origin.y.is_finite() && cs_origin.z.is_finite() && !cs_origin.is_zero() {
                    origin = cs_origin;
                } else {
                    return None; // All origin sources failed
                }
            }
        }

        // --- Staleness detection ---
        // If a player's position history index hasn't changed for too many frames,
        // their data might be stale (e.g. they disconnected but weren't cleaned up).
        let frame = FRAME_COUNTER.load(Ordering::Relaxed);
        let i = idx as usize;
        let cur_pos_val = read_i32(base + ENT_CURPOS) as usize & PH_HISTORY_MASK;

        let last_cp = LAST_CURPOS[i];
        if last_cp != cur_pos_val {
            // Position history updated — player is active
            LAST_CURPOS[i] = cur_pos_val;
            LAST_CURPOS_FRAME[i] = frame;
            LAST_KNOWN_ORIGIN[i] = origin;
        } else {
            // Position history hasn't changed — check staleness
            let last_frame = LAST_CURPOS_FRAME[i];
            if last_frame == 0 { return None; }
            let age = frame.wrapping_sub(last_frame);
            if age > ORIGIN_STALE_FRAMES {
                // Use cached origin for a while, then give up
                if age <= ORIGIN_STALE_FRAMES.saturating_mul(8) {
                    let cached = LAST_KNOWN_ORIGIN[i];
                    if cached.is_zero() { return None; }
                    origin = cached;
                } else {
                    return None; // Too stale
                }
            }
        }

        // --- Team and alive/dead status from g_PlayerExtraInfo ---
        let base_ei = get_extra_info_base();
        let slot_addr = if base_ei != 0 { base_ei + (idx as usize) * EXTRA_STRIDE } else { 0 };

        let team = if slot_addr != 0 {
            read_i16(slot_addr + EXTRA_OFF_TEAMNUMBER) as i32
        } else { 0 };

        // Skip dead players
        if slot_addr != 0 {
            let is_dead = read_u8(slot_addr + EXTRA_OFF_DEAD);
            if is_dead != 0 { return None; }
        }

        // --- Weapon name (from the weapon model path) ---
        let weapon_name = {
            let wmodel_idx = read_i32(cs + ES_WEAPONMODEL);
            if wmodel_idx > 0 {
                self.get_weapon_name(wmodel_idx)
            } else {
                String::new()
            }
        };

        // --- Ducking detection ---
        let usehull = read_i32(cs + ES_USEHULL);
        let is_ducking = usehull == 1; // Hull 1 = duck hull

        // --- Bounding box height ---
        let margin = 4.0;
        let maxs_z = if is_ducking {
            let maxs_duck = read_f32(cs + ES_MAXS + 8); // maxs.z
            if maxs_duck > 0.0 && maxs_duck < 60.0 { maxs_duck + margin } else { 44.0 + margin }
        } else {
            let maxs_stand = read_f32(cs + ES_MAXS + 8);
            if maxs_stand > 60.0 && maxs_stand < 90.0 { maxs_stand + margin } else { 72.0 + margin }
        };

        let name = name.unwrap_or_else(|| format!("P{}", idx));
        let is_local = pinfo.thisplayer != 0;

        Some(PlayerData {
            origin,
            maxs_z,
            team,
            name,
            weapon: weapon_name,
            is_local,
            is_ducking,
        })
    }

    /// Maximum number of player slots.
    pub fn max_clients(&self) -> i32 { MAX_CLIENTS }

    /// Get a weapon's display name from its model index.
    /// The engine stores weapon models like "models/p_ak47.mdl".
    /// We extract "AK47" from the model path.
    pub unsafe fn get_weapon_name(&self, model_index: i32) -> String {
        type FnGetModelByIndex = unsafe extern "C" fn(idx: i32) -> *mut u8;
        let fn_ptr = read_u32(self.table + SLOT_GET_MODEL_BY_INDEX * 4) as usize;
        if fn_ptr < 0x10000 { return String::new(); }

        let f: FnGetModelByIndex = std::mem::transmute(fn_ptr);
        let model = f(model_index);
        if model.is_null() { return String::new(); }
        let model_addr = model as usize;
        if !is_readable(model_addr, 64) { return String::new(); }

        if let Some(name) = read_cstr(model_addr as *const i8, 64) {
            // Look for "p_" prefix (player weapon model) or "w_" (world weapon model)
            for prefix in &["p_", "w_"] {
                if let Some(start) = name.find(prefix) {
                    let after = &name[start + 2..];
                    let end = after.find('.').unwrap_or(after.len());
                    return after[..end].to_uppercase();
                }
            }
        }
        String::new()
    }

    /// Get the GetPlayerInfo function pointer from the engine table.
    unsafe fn get_player_info_fn(&self) -> Option<FnGetPlayerInfo> {
        let fn_ptr = read_u32(self.table + SLOT_GET_PLAYER_INFO * 4) as usize;
        if fn_ptr <= 0x10000 { return None; }
        Some(std::mem::transmute(fn_ptr))
    }

    /// Project a 3D world position to 2D screen coordinates using the engine's TriAPI.
    /// Returns NDC coordinates (normalized device coordinates) or None if behind camera.
    pub unsafe fn world_to_screen(&self, world: Vec3) -> Option<(f32, f32)> {
        // Get the TriAPI interface pointer
        let tri_api = read_u32(self.table + SLOT_PTRIAPI * 4) as usize;
        if tri_api < 0x10000 { return None; }

        // TriAPI slot 12 = WorldToScreen function
        let w2s_fn_ptr = read_u32(tri_api + 12 * 4) as usize;
        if w2s_fn_ptr == 0 { return None; }

        type FnWorldToScreen = unsafe extern "C" fn(world: *const f32, screen: *mut f32) -> i32;
        let w2s_fn: FnWorldToScreen = std::mem::transmute(w2s_fn_ptr);

        let world_arr = [world.x, world.y, world.z];
        let mut screen = [0f32; 3];
        let z_clipped = w2s_fn(world_arr.as_ptr(), screen.as_mut_ptr());

        // z_clipped != 0 means the point is behind the camera
        if z_clipped != 0 { return None; }
        Some((screen[0], screen[1]))
    }
}

// ============================================================
// Memory Scanning — Find Engine Table & Player Extra Info
// ============================================================

/// Scan client.dll's memory for the engine function table (gEngfuncs).
/// Looks for a consecutive run of 8+ pointers into hw.dll's address range,
/// then validates slots 51 and 53 (GetLocalPlayer, GetEntityByIndex).
unsafe fn find_gengfuncs_in_client() -> Option<usize> {
    let (cl_base, cl_end) = module_range(b"client.dll\0")?;
    let (hw_base, hw_end) = module_range(b"hw.dll\0")?;

    let readable_flags = PAGE_READONLY | PAGE_READWRITE | PAGE_WRITECOPY
        | PAGE_EXECUTE_READ | PAGE_EXECUTE_READWRITE | PAGE_EXECUTE_WRITECOPY;

    let mut addr = cl_base;
    while addr < cl_end {
        let mut mbi: MEMORY_BASIC_INFORMATION = std::mem::zeroed();
        let ret = VirtualQuery(addr as *const _, &mut mbi,
            std::mem::size_of::<MEMORY_BASIC_INFORMATION>());
        if ret == 0 { break; }
        let region_end = (mbi.BaseAddress as usize + mbi.RegionSize).min(cl_end);

        if mbi.State != MEM_COMMIT || mbi.Protect & readable_flags == 0 {
            addr = region_end;
            continue;
        }

        // Scan this memory region for a run of hw.dll pointers
        let mut scan = addr;
        while scan + 4 <= region_end {
            let mut hits = 0usize;
            let mut j = 0usize;
            while scan + (j + 1) * 4 <= region_end && j < 64 {
                let v = std::ptr::read_unaligned((scan + j * 4) as *const u32) as usize;
                if v >= hw_base && v < hw_end { hits += 1; } else { break; }
                j += 1;
            }
            if hits >= 8 {
                // Validate by checking slots 51 and 53 point into hw.dll
                let s51 = std::ptr::read_unaligned((scan + 51 * 4) as *const u32) as usize;
                let s53 = std::ptr::read_unaligned((scan + 53 * 4) as *const u32) as usize;
                if s51 >= hw_base && s51 < hw_end && s53 >= hw_base && s53 < hw_end {
                    return Some(scan);
                }
            }
            scan += 4;
        }
        addr = region_end;
    }
    None
}

/// Scan client.dll for g_PlayerExtraInfo — a global array of per-player metadata.
/// Uses two byte patterns (primary + alternate) to locate the array pointer.
unsafe fn find_player_extra_info() -> Option<usize> {
    let (cl_base, cl_end) = module_range(b"client.dll\0")?;

    // Primary pattern (references g_PlayerExtraInfo via a pointer operand)
    let pat: &[u8] = &[
        0x0F, 0xBF, 0x87, 0xCC, 0xCC, 0xCC, 0xCC,
        0x8B, 0x16, 0x50, 0x68, 0xCC, 0xCC, 0xCC, 0xCC,
        0x8B, 0xCE, 0xFF, 0x52, 0xCC,
        0x8D, 0x4C, 0xAD, 0x00,
        0x66, 0x8B, 0x04, 0x8D,
    ];
    let mask: &[u8] = &[
        1,1,1,0,0,0,0,
        1,1,1,1,0,0,0,0,
        1,1,1,1,0,
        1,1,1,1,
        1,1,1,1,
    ];

    // Try primary pattern
    if let Some(addr) = scan_with_pattern(cl_base, cl_end, pat, mask, 27, 4) {
        return Some(addr);
    }

    // Alternate pattern (different code generation, same data)
    let pat2: &[u8] = &[
        0x0F, 0xBF, 0x87, 0xCC, 0xCC, 0xCC, 0xCC,
        0x8B, 0x16, 0x50, 0x68, 0xCC, 0xCC, 0xCC, 0xCC,
        0x8B, 0xCE, 0xFF, 0x52, 0xCC,
        0x8B, 0xCD, 0xC1, 0xE1, 0x05,
        0x66, 0x8B, 0x81, 0xCC, 0xCC, 0xCC, 0xCC,
        0x66, 0x3D, 0x01, 0x00, 0x7D, 0x46,
    ];
    let mask2: &[u8] = &[
        1,1,1,0,0,0,0,
        1,1,1,1,0,0,0,0,
        1,1,1,1,0,
        1,1,1,1,1,
        1,1,1,0,0,0,0,
        1,1,1,1,1,1,
    ];

    // Try alternate pattern
    if let Some(addr) = scan_with_pattern(cl_base, cl_end, pat2, mask2, 3, 4) {
        return Some(addr);
    }

    None
}

/// Generic masked byte pattern scanner.
/// Scans memory from `start` to `end` for `pattern` (0xCC bytes in mask=0 are wildcards).
/// On match, reads a 4-byte pointer at `match_offset` bytes from the match start.
/// Validates the pointer points to readable memory of size `validate_size * 33`.
unsafe fn scan_with_pattern(
    start: usize, end: usize,
    pattern: &[u8], mask: &[u8],
    ptr_offset: usize, _validate_size: usize,
) -> Option<usize> {
    let readable_flags = PAGE_READONLY | PAGE_READWRITE | PAGE_WRITECOPY
        | PAGE_EXECUTE_READ | PAGE_EXECUTE_READWRITE | PAGE_EXECUTE_WRITECOPY;

    let mut addr = start;
    while addr < end {
        let mut mbi: MEMORY_BASIC_INFORMATION = std::mem::zeroed();
        let ret = VirtualQuery(addr as *const _, &mut mbi,
            std::mem::size_of::<MEMORY_BASIC_INFORMATION>());
        if ret == 0 { break; }
        let region_end = (mbi.BaseAddress as usize + mbi.RegionSize).min(end);

        if mbi.State == MEM_COMMIT && mbi.Protect & readable_flags != 0 {
            let mut scan = addr;
            while scan + pattern.len() <= region_end {
                let mut matched = true;
                for i in 0..pattern.len() {
                    if mask[i] == 1 {
                        let b = std::ptr::read_unaligned((scan + i) as *const u8);
                        if b != pattern[i] { matched = false; break; }
                    }
                }
                if matched {
                    let pa = scan + ptr_offset;
                    if is_readable(pa, 4) {
                        let arr_ptr = std::ptr::read_unaligned(pa as *const u32) as usize;
                        if arr_ptr > 0x10000 && is_readable(arr_ptr, EXTRA_STRIDE * 33) {
                            return Some(arr_ptr);
                        }
                    }
                }
                scan += 1;
            }
        }
        addr = region_end;
    }
    None
}

/// Get the cached g_PlayerExtraInfo base address, scanning for it if needed.
unsafe fn get_extra_info_base() -> usize {
    let cached = EXTRA_INFO_BASE.load(Ordering::Relaxed);
    if cached != 0 { return cached; }
    if let Some(ptr) = find_player_extra_info() {
        EXTRA_INFO_BASE.store(ptr, Ordering::Relaxed);
        return ptr;
    }
    0
}

// ============================================================
// Low-Level Memory Reading Utilities
// ============================================================

/// Get the base address and end address of a loaded module.
unsafe fn module_range(name: &[u8]) -> Option<(usize, usize)> {
    let h = GetModuleHandleA(name.as_ptr() as _);
    if h.is_null() { return None; }
    let mut info: MODULEINFO = std::mem::zeroed();
    let ok = GetModuleInformation(
        GetCurrentProcess(), h, &mut info,
        std::mem::size_of::<MODULEINFO>() as u32,
    );
    if ok == 0 { return None; }
    Some((info.lpBaseOfDll as usize, info.lpBaseOfDll as usize + info.SizeOfImage as usize))
}

/// Check if a memory region is readable (committed + has read permission).
unsafe fn is_readable(addr: usize, len: usize) -> bool {
    if addr == 0 || len == 0 { return false; }
    let readable = PAGE_READONLY | PAGE_READWRITE | PAGE_WRITECOPY
        | PAGE_EXECUTE_READ | PAGE_EXECUTE_READWRITE | PAGE_EXECUTE_WRITECOPY;
    let mut mbi: MEMORY_BASIC_INFORMATION = std::mem::zeroed();
    let ret = VirtualQuery(addr as *const _, &mut mbi,
        std::mem::size_of::<MEMORY_BASIC_INFORMATION>());
    if ret == 0 { return false; }
    if mbi.State != MEM_COMMIT { return false; }
    if mbi.Protect & readable == 0 { return false; }
    addr + len <= mbi.BaseAddress as usize + mbi.RegionSize
}

/// Read a u32 from a remote memory address (returns 0 if unreadable).
#[inline]
unsafe fn read_u32(addr: usize) -> u32 {
    if !is_readable(addr, 4) { return 0; }
    std::ptr::read_unaligned(addr as *const u32)
}

/// Read an i32 from a remote memory address (returns 0 if unreadable).
#[inline]
unsafe fn read_i32(addr: usize) -> i32 {
    if !is_readable(addr, 4) { return 0; }
    std::ptr::read_unaligned(addr as *const i32)
}

/// Read an i16 from a remote memory address (returns 0 if unreadable).
#[inline]
unsafe fn read_i16(addr: usize) -> i16 {
    if !is_readable(addr, 2) { return 0; }
    std::ptr::read_unaligned(addr as *const i16)
}

/// Read a u8 from a remote memory address (returns 0 if unreadable).
#[inline]
unsafe fn read_u8(addr: usize) -> u8 {
    if !is_readable(addr, 1) { return 0; }
    std::ptr::read(addr as *const u8)
}

/// Read an f32 from a remote memory address (returns 0.0 if unreadable).
#[inline]
unsafe fn read_f32(addr: usize) -> f32 {
    if !is_readable(addr, 4) { return 0.0; }
    std::ptr::read_unaligned(addr as *const f32)
}

/// Read a Vec3 (three consecutive f32s) from a memory address.
unsafe fn read_vec3(addr: usize) -> Vec3 {
    Vec3 { x: read_f32(addr), y: read_f32(addr + 4), z: read_f32(addr + 8) }
}

/// Read a null-terminated C string from a memory address.
/// Only includes printable ASCII characters (32-126).
unsafe fn read_cstr(ptr: *const i8, max_len: usize) -> Option<String> {
    if ptr.is_null() { return None; }
    let mut out: Vec<u8> = Vec::new();
    let base = ptr as usize;
    for i in 0..max_len {
        let a = base + i;
        if !is_readable(a, 1) { break; }
        let b = std::ptr::read(a as *const u8);
        if b == 0 { break; }                      // Null terminator
        if (32..=126).contains(&b) { out.push(b); } // Printable ASCII only
    }
    if out.is_empty() { None } else { Some(String::from_utf8_lossy(&out).into_owned()) }
}