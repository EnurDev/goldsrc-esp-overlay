// esp.rs — ESP (Extra-Sensory Perception) overlay drawing logic.
//
// This module runs every frame (called from the wglSwapBuffers detour).
// It reads player data from the engine, projects 3D positions to 2D screen
// coordinates, and draws bounding boxes, name labels, distance, and weapon info.
//
// Features:
//   - F6 hotkey to toggle overlay on/off
//   - Team-colored bounding boxes with corner brackets
//   - Snap-line from screen bottom to each player's feet
//   - Name labels above boxes, distance + weapon below
//   - Cached boxes that fade out when a player disappears temporarily

use crate::entities::EngineApi;
use crate::math::Vec3;
use crate::render;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use winapi::shared::windef::{HDC, RECT};
use winapi::um::winuser::{GetAsyncKeyState, GetClientRect, WindowFromDC};

// ============================================================
// Configuration Constants
// ============================================================

const VK_F6: i32 = 0x75;               // Virtual key code for F6
const BOX_ASPECT: f32 = 0.50;          // Width/height ratio for ESP boxes
const UNITS_PER_METER: f32 = 39.37;    // GoldSrc units to meters conversion
const PIXEL_MARGIN: f32 = 1_000_000.0; // Off-screen culling threshold
const CACHE_TTL_FRAMES: u32 = 90;      // How many frames to keep showing a cached box

// ============================================================
// State: Toggle & Frame Counter
// ============================================================

/// Whether the ESP overlay is currently visible.
static VISIBLE: AtomicBool = AtomicBool::new(true);

/// Previous F6 key state (for edge detection: press, not hold).
static F6_PREV: AtomicBool = AtomicBool::new(false);

/// Global frame counter (incremented each frame).
static FRAME_ID: AtomicU32 = AtomicU32::new(0);

// ============================================================
// Per-Player Cache (for fade-out effect when players disappear)
// ============================================================
// These arrays are indexed by player slot (1-32). Slot 0 is unused.

/// Cached bounding box coordinates [x0, y0, x1, y1] per player.
static mut LAST_BOX: [[f32; 4]; 33] = [[0.0; 4]; 33];

/// Cached feet screen position [x, y] per player (for snap-lines).
static mut LAST_FEET: [[f32; 2]; 33] = [[0.0; 2]; 33];

/// Cached distance (meters) per player.
static mut LAST_DIST: [f32; 33] = [0.0; 33];

/// Cached team color per player.
static mut LAST_COLOR: [[f32; 4]; 33] = [[0.0; 4]; 33];

/// Frame number when each player was last seen.
static mut LAST_SEEN: [u32; 33] = [0; 33];

/// Cached local player position (fallback when engine returns None briefly).
static mut LAST_LOCAL: [f32; 3] = [0.0; 3];

/// Whether we have a valid cached local player position.
static LAST_LOCAL_VALID: AtomicBool = AtomicBool::new(false);

// ============================================================
// Toggle Hotkey Logic
// ============================================================

/// Poll the F6 key and toggle visibility on rising edge (press, not hold).
fn poll_toggle() {
    let down = unsafe { (GetAsyncKeyState(VK_F6) as u16) & 0x8000 != 0 };
    let was = F6_PREV.swap(down, Ordering::Relaxed);
    if down && !was {
        // XOR with true = flip the boolean
        VISIBLE.fetch_xor(true, Ordering::Relaxed);
    }
}

// ============================================================
// Coordinate Conversion
// ============================================================

/// Convert engine NDC (normalized device coordinates) to pixel coordinates.
/// The engine's W2S returns NDC where (-1,-1) is bottom-left and (1,1) is top-right.
/// We need pixel coords where (0,0) is top-left and (w,h) is bottom-right.
fn ndc_to_px(ndc_x: f32, ndc_y: f32, screen_h: f32, vx: f32, vy: f32, vw: f32, vh: f32) -> [f32; 2] {
    let x = vx + (ndc_x + 1.0) * 0.5 * vw;
    let y_bottom_left = vy + (ndc_y + 1.0) * 0.5 * vh;
    [x, screen_h - y_bottom_left] // Flip Y: bottom-left -> top-left origin
}

// ============================================================
// Main Frame Handler
// ============================================================

/// Called every frame from the wglSwapBuffers detour.
/// Reads all player data and draws the ESP overlay.
pub unsafe fn on_frame(hdc: HDC) {
    // Check for F6 toggle
    poll_toggle();

    // Get the screen dimensions and GL viewport
    let (screen_w, screen_h, vx, vy, vw, vh) = match viewport_size(hdc) {
        Some(v) => v,
        None => return,
    };

    // Enter 2D drawing mode
    render::begin_2d(screen_w, screen_h);

    // Draw status indicator
    let vis = VISIBLE.load(Ordering::Relaxed);
    let status = if vis { "[ESP ON]  F6=toggle" } else { "[ESP OFF] F6=toggle" };
    render::draw_text(hdc, 6.0, 14.0, status, [1.0, 0.15, 0.15, 1.0]);

    // If ESP is toggled off, just show the status and return
    if !vis {
        render::end_2d();
        return;
    }

    // Increment frame counter and sync it with the entity staleness tracker
    let frame = FRAME_ID.fetch_add(1, Ordering::Relaxed).wrapping_add(1);
    crate::entities::set_frame_counter(frame);

    // Try to resolve the engine API (may fail if map isn't loaded yet)
    let api = match EngineApi::resolve() {
        Some(a) => a,
        None => {
            // Show a "waiting" message if the map hasn't loaded
            if !EngineApi::map_loaded() {
                render::draw_text(hdc, 6.0, 28.0,
                    "waiting for map load (start a game)...",
                    [1.0, 0.15, 0.15, 1.0]);
            }
            // Still draw cached boxes from when we last had data
            let _ = draw_cached_boxes(hdc, screen_h, vx, vy, vw, frame, CACHE_TTL_FRAMES, 0.65);
            render::end_2d();
            return;
        }
    };

    // --- Read local player position ---
    let local_pos = match api.local_origin() {
        Some(v) => {
            LAST_LOCAL = [v.x, v.y, v.z];
            LAST_LOCAL_VALID.store(true, Ordering::Relaxed);
            v
        }
        None => {
            // Use cached position if available
            if LAST_LOCAL_VALID.load(Ordering::Relaxed) {
                Vec3 { x: LAST_LOCAL[0], y: LAST_LOCAL[1], z: LAST_LOCAL[2] }
            } else {
                Vec3::default()
            }
        }
    };
    let have_local = LAST_LOCAL_VALID.load(Ordering::Relaxed);

    // --- Draw ESP for each player ---
    let mut drawn = 0u32;
    let mut drawn_now = [false; 33]; // Track which slots were drawn fresh this frame

    for idx in 1..=api.max_clients() {
        // Read player data from the engine (returns None for invalid/dead/spectator players)
        let Some(player) = api.read_player(idx) else { continue };

        // Skip the local player (don't draw ESP on yourself)
        if player.is_local || (have_local && local_pos.distance(player.origin) < 4.0) {
            continue;
        }

        // --- Calculate bounding box in world space ---
        let mut half_h = (player.maxs_z * 0.5).max(8.0);
        let mut z_offset = 0.0f32;
        if player.is_ducking {
            half_h = half_h.max(26.0);
            z_offset = 6.0; // Adjust center when ducking
        }
        let feet = Vec3 {
            x: player.origin.x, y: player.origin.y,
            z: player.origin.z - half_h + z_offset,
        };
        let head = Vec3 {
            x: player.origin.x, y: player.origin.y,
            z: player.origin.z + half_h + z_offset,
        };

        // --- Project feet and head to screen coordinates ---
        let Some((fx, fy)) = api.world_to_screen(feet) else { continue };
        let Some((hx, hy)) = api.world_to_screen(head) else { continue };
        if !fx.is_finite() || !fy.is_finite() || !hx.is_finite() || !hy.is_finite() { continue; }

        let feet_px = ndc_to_px(fx, fy, screen_h, vx, vy, vw, vh);
        let head_px = ndc_to_px(hx, hy, screen_h, vx, vy, vw, vh);

        // Skip if way off-screen
        if feet_px[0] < -PIXEL_MARGIN || feet_px[0] > screen_w + PIXEL_MARGIN
        || feet_px[1] < -PIXEL_MARGIN || feet_px[1] > screen_h + PIXEL_MARGIN {
            continue;
        }

        // --- Calculate 2D bounding box ---
        let y0 = head_px[1].min(feet_px[1]);  // Top of box
        let y1 = head_px[1].max(feet_px[1]);  // Bottom of box
        let box_h = (y1 - y0).max(4.0);
        let box_w = box_h * BOX_ASPECT;        // Width proportional to height
        let cx = (feet_px[0] + head_px[0]) * 0.5; // Center X
        let x0 = cx - box_w * 0.5;
        let x1 = cx + box_w * 0.5;

        // --- Team color ---
        let color: [f32; 4] = match player.team {
            1 => [0.95, 0.18, 0.18, 1.0], // Terrorists = red
            2 => [0.18, 0.50, 0.95, 1.0], // Counter-Terrorists = blue
            _ => [0.10, 0.95, 0.10, 1.0], // Unknown = green
        };

        // --- Draw the ESP elements ---
        render::draw_rect_outline(x0, y0, x1, y1);  // Dark shadow outline
        render::draw_box_corners(x0, y0, x1, y1, color); // Colored corner brackets

        // Snap-line from bottom-center of screen to the player's feet
        render::draw_line(
            vx + vw * 0.5, screen_h - vy,
            feet_px[0], feet_px[1],
            [1.0, 1.0, 0.15, 0.55],
        );

        // Distance in meters
        let dist = if have_local {
            local_pos.distance(player.origin) / UNITS_PER_METER
        } else { 0.0 };

        // Player name centered above the box
        let name_x = cx - (player.name.len() as f32 * 3.5);
        render::draw_text(hdc, name_x, y0 - 2.0, &player.name, [1.0, 1.0, 1.0, 1.0]);

        // Distance and weapon label below the box
        let mut info = format!("{:.1}m", dist);
        if !player.weapon.is_empty() {
            info.push_str(&format!("  [{}]", player.weapon));
        }
        render::draw_text(hdc, x0, y1 + 12.0, &info, [1.0, 1.0, 1.0, 1.0]);

        drawn += 1;

        // --- Cache this frame's data for fade-out ---
        let i = idx as usize;
        drawn_now[i] = true;
        LAST_BOX[i] = [x0, y0, x1, y1];
        LAST_FEET[i] = [feet_px[0], feet_px[1]];
        LAST_DIST[i] = dist;
        LAST_COLOR[i] = color;
        LAST_SEEN[i] = frame;
    }

    // --- Draw cached/fading boxes for players not seen this frame ---
    for idx in 1..=api.max_clients() {
        let i = idx as usize;
        if drawn_now[i] { continue; } // Already drawn fresh above

        let seen = LAST_SEEN[i];
        if seen == 0 { continue; } // Never seen

        // Distance-dependent TTL: closer players stay cached longer
        let dist = LAST_DIST[i];
        let ttl = if dist > 0.0 {
            if dist < 10.0 { 300u32 } else if dist < 30.0 { 150u32 } else { CACHE_TTL_FRAMES }
        } else { CACHE_TTL_FRAMES };

        if frame.wrapping_sub(seen) > ttl { continue; } // Expired

        let [x0, y0, x1, y1] = LAST_BOX[i];
        let [fx, fy] = LAST_FEET[i];
        if x0 == 0.0 && y0 == 0.0 && x1 == 0.0 && y1 == 0.0 { continue; }

        // Fade out over ~12 frames using ease-out curve
        let mut color = LAST_COLOR[i];
        let base_alpha = if dist > 0.0 && dist < 10.0 { 0.95 } else { 0.60 };
        let age = frame.wrapping_sub(seen) as f32;
        let fade_t = (age / 12.0).clamp(0.0, 1.0);
        let ease = 1.0_f32 - (1.0 - fade_t).powf(2.0);
        let final_alpha = (base_alpha * (1.0 - ease)).max(0.02);
        if final_alpha <= 0.02 { continue; }
        color[3] = final_alpha;

        // Draw the cached box with faded alpha
        render::draw_rect_outline(x0, y0, x1, y1);
        render::draw_box_corners(x0, y0, x1, y1, color);
        render::draw_line(vx + vw * 0.5, screen_h - vy, fx, fy, [1.0, 0.15, 0.15, final_alpha * 0.6]);
        let label = format!("{:.1}m", LAST_DIST[i]);
        render::draw_text(hdc, x0, y1 + 12.0, &label, [1.0, 1.0, 1.0, final_alpha]);
        drawn += 1;
    }

    // Show a hint if no players were found
    if drawn == 0 {
        render::draw_text(hdc, 6.0, 84.0, "no players (in-game?)", [1.0, 0.15, 0.15, 1.0]);
    }

    render::end_2d();
}

// ============================================================
// Cached Box Drawing (used when engine API is unavailable)
// ============================================================

/// Draw only the cached/fading boxes (used when the engine API is temporarily unavailable).
unsafe fn draw_cached_boxes(
    hdc: HDC,
    screen_h: f32,
    vx: f32,
    vy: f32,
    vw: f32,
    frame: u32,
    ttl_frames: u32,
    alpha: f32,
) -> u32 {
    let mut drawn = 0u32;
    for idx in 1..=32usize {
        let seen = LAST_SEEN[idx];
        if seen == 0 { continue; }

        // Distance-dependent TTL
        let dist = LAST_DIST[idx];
        let ttl = if dist > 0.0 {
            if dist < 10.0 { 300u32 } else if dist < 30.0 { 150u32 } else { ttl_frames }
        } else { ttl_frames };

        if frame.wrapping_sub(seen) > ttl { continue; }

        let [x0, y0, x1, y1] = LAST_BOX[idx];
        let [fx, fy] = LAST_FEET[idx];
        if x0 == 0.0 && y0 == 0.0 && x1 == 0.0 && y1 == 0.0 { continue; }

        // Fade-out with ease-out curve
        let mut color = LAST_COLOR[idx];
        let base_alpha = if dist > 0.0 && dist < 10.0 { (alpha + 0.6).min(1.0) } else { alpha };
        let age = frame.wrapping_sub(seen) as f32;
        let fade_t = (age / 12.0).clamp(0.0, 1.0);
        let ease = 1.0_f32 - (1.0 - fade_t).powf(2.0);
        let final_alpha = (base_alpha * (1.0 - ease)).max(0.02);
        if final_alpha <= 0.02 { continue; }
        color[3] = final_alpha;

        render::draw_rect_outline(x0, y0, x1, y1);
        render::draw_box_corners(x0, y0, x1, y1, color);
        render::draw_line(vx + vw * 0.5, screen_h - vy, fx, fy, [1.0, 0.15, 0.15, final_alpha * 0.6]);
        let label = format!("{:.1}m", LAST_DIST[idx]);
        render::draw_text(hdc, x0, y1 + 12.0, &label, [1.0, 1.0, 1.0, final_alpha]);
        drawn += 1;
    }
    drawn
}

// ============================================================
// Viewport Helper
// ============================================================

/// Get the game window's client area size and the OpenGL viewport rectangle.
/// Returns (screen_w, screen_h, viewport_x, viewport_y, viewport_w, viewport_h).
unsafe fn viewport_size(hdc: HDC) -> Option<(f32, f32, f32, f32, f32, f32)> {
    // Find the window associated with this device context
    let hwnd = WindowFromDC(hdc);
    if hwnd.is_null() { return None; }

    // Get the client area dimensions
    let mut rc: RECT = std::mem::zeroed();
    if GetClientRect(hwnd, &mut rc) == 0 { return None; }
    let screen_w = rc.right - rc.left;
    let screen_h = rc.bottom - rc.top;
    if screen_w <= 0 || screen_h <= 0 { return None; }

    // Get the GL viewport (may differ from client area in some configs)
    let (vx, vy, vw, vh) = match render::viewport_rect() {
        Some(v) => v,
        None => (0.0, 0.0, screen_w as f32, screen_h as f32),
    };

    Some((screen_w as f32, screen_h as f32, vx, vy, vw, vh))
}