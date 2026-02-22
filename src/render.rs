// render.rs - Low-level OpenGL 1.x drawing primitives for the 2D overlay.
//
// This module provides custom 2D drawing functions specifically tailored for 
// an in-game hardware-accelerated overlay. Because Counter-Strike 1.6 runs on 
// the legacy GoldSrc engine using OpenGL 1.x, we hook into its rendering pipeline 
// and draw directly using immediate-mode OpenGL API calls (`glBegin`, `glVertex2f`).
//
// Key features of this implementation:
//   - Pure GL Rendering: Does not rely on Windows GDI (e.g., `wglUseFontBitmapsA`), 
//     ensuring flawless display in both Windowed and Fullscreen Exclusive modes.
//   - Custom Stroke Font: Text is drawn using a fast, crisp, custom line-segment 
//     font (`glVertex2f`), mimicking the classic blocky CS 1.6 HUD typography.
//   - State Preservation: The `begin_2d()` and `end_2d()` functions ensure the game's 
//     original 3D pipeline state is saved and restored perfectly, avoiding visual artifacts.

use winapi::shared::windef::HDC;

const GL_ALL_ATTRIB_BITS:     u32 = 0x000F_FFFF;
const GL_DEPTH_TEST:          u32 = 0x0B71;
const GL_BLEND:               u32 = 0x0BE2;
const GL_TEXTURE_2D:          u32 = 0x0DE1;
const GL_LIGHTING:            u32 = 0x0B50;
const GL_FOG:                 u32 = 0x0B60;
const GL_ALPHA_TEST:          u32 = 0x0BC0;
const GL_CULL_FACE:           u32 = 0x0B44;
const GL_SCISSOR_TEST:        u32 = 0x0C11;
const GL_STENCIL_TEST:        u32 = 0x0B90;
const GL_VIEWPORT:            u32 = 0x0BA2;
const GL_SRC_ALPHA:           u32 = 0x0302;
const GL_ONE_MINUS_SRC_ALPHA: u32 = 0x0303;
const GL_PROJECTION:          u32 = 0x1701;
const GL_MODELVIEW:           u32 = 0x1700;
const GL_LINES:               u32 = 0x0001;

#[link(name = "opengl32")]
extern "system" {
    fn glPushAttrib(mask: u32);
    fn glPopAttrib();
    fn glDisable(cap: u32);
    fn glEnable(cap: u32);
    fn glBlendFunc(sfactor: u32, dfactor: u32);
    fn glMatrixMode(mode: u32);
    fn glPushMatrix();
    fn glPopMatrix();
    fn glLoadIdentity();
    fn glOrtho(left: f64, right: f64, bottom: f64, top: f64, zn: f64, zf: f64);
    fn glColor4f(r: f32, g: f32, b: f32, a: f32);
    fn glBegin(mode: u32);
    fn glVertex2f(x: f32, y: f32);
    fn glEnd();
    fn glLineWidth(w: f32);
    fn glGetIntegerv(pname: u32, data: *mut i32);
}

// ============================================================
// 2D Overlay
// ============================================================

pub unsafe fn begin_2d(w: f32, h: f32) {
    glPushAttrib(GL_ALL_ATTRIB_BITS);
    glDisable(GL_DEPTH_TEST);
    glDisable(GL_TEXTURE_2D);
    glDisable(GL_LIGHTING);
    glDisable(GL_FOG);
    glDisable(GL_ALPHA_TEST);
    glDisable(GL_CULL_FACE);
    glDisable(GL_SCISSOR_TEST);
    glDisable(GL_STENCIL_TEST);
    glEnable(GL_BLEND);
    glBlendFunc(GL_SRC_ALPHA, GL_ONE_MINUS_SRC_ALPHA);
    glLineWidth(1.5);
    glColor4f(1.0, 1.0, 1.0, 1.0);
    glMatrixMode(GL_PROJECTION);
    glPushMatrix();
    glLoadIdentity();
    glOrtho(0.0, w as f64, h as f64, 0.0, -1.0, 1.0);
    glMatrixMode(GL_MODELVIEW);
    glPushMatrix();
    glLoadIdentity();
}

pub unsafe fn end_2d() {
    glPopMatrix();
    glMatrixMode(GL_PROJECTION);
    glPopMatrix();
    glPopAttrib();
}

pub unsafe fn viewport_rect() -> Option<(f32, f32, f32, f32)> {
    let mut vp = [0i32; 4];
    glGetIntegerv(GL_VIEWPORT, vp.as_mut_ptr());
    if vp[2] <= 0 || vp[3] <= 0 { return None; }
    Some((vp[0] as f32, vp[1] as f32, vp[2] as f32, vp[3] as f32))
}

// ============================================================
// Drawing Primitives
// ============================================================

pub unsafe fn draw_rect(x0: f32, y0: f32, x1: f32, y1: f32, c: [f32; 4]) {
    glColor4f(c[0], c[1], c[2], c[3]);
    glBegin(GL_LINES);
    glVertex2f(x0, y0); glVertex2f(x1, y0);
    glVertex2f(x1, y0); glVertex2f(x1, y1);
    glVertex2f(x1, y1); glVertex2f(x0, y1);
    glVertex2f(x0, y1); glVertex2f(x0, y0);
    glEnd();
}

pub unsafe fn draw_box_corners(x0: f32, y0: f32, x1: f32, y1: f32, c: [f32; 4]) {
    let bw = x1 - x0;
    let bh = y1 - y0;
    let lw = (bw * 0.22).clamp(4.0, 18.0);
    let lh = (bh * 0.22).clamp(4.0, 18.0);
    glColor4f(c[0], c[1], c[2], c[3]);
    glBegin(GL_LINES);
    glVertex2f(x0,     y0); glVertex2f(x0 + lw, y0);
    glVertex2f(x0,     y0); glVertex2f(x0,      y0 + lh);
    glVertex2f(x1,     y0); glVertex2f(x1 - lw, y0);
    glVertex2f(x1,     y0); glVertex2f(x1,      y0 + lh);
    glVertex2f(x0,     y1); glVertex2f(x0 + lw, y1);
    glVertex2f(x0,     y1); glVertex2f(x0,      y1 - lh);
    glVertex2f(x1,     y1); glVertex2f(x1 - lw, y1);
    glVertex2f(x1,     y1); glVertex2f(x1,      y1 - lh);
    glEnd();
}

pub unsafe fn draw_rect_outline(x0: f32, y0: f32, x1: f32, y1: f32) {
    draw_rect(x0 - 1.0, y0 - 1.0, x1 + 1.0, y1 + 1.0, [0.0, 0.0, 0.0, 0.6]);
}

pub unsafe fn draw_line(x0: f32, y0: f32, x1: f32, y1: f32, c: [f32; 4]) {
    glColor4f(c[0], c[1], c[2], c[3]);
    glBegin(GL_LINES);
    glVertex2f(x0, y0);
    glVertex2f(x1, y1);
    glEnd();
}

// ============================================================
// Stroke Font - CS 1.6 styled, pure GL lines
// ============================================================
// Characters are drawn on a 6-wide x 8-tall grid, scaled by SC.
// Grid origin = top-left. Y increases downward.
// Mostly horizontal/vertical strokes for the blocky bitmap-font look.
//
// CHAR_W  = total column width (char + spacing)
// SC      = pixel scale — increase for bigger text

const CHAR_W: f32 = 9.0;
const SC:     f32 = 1.2;

unsafe fn draw_stroke_char(cx: f32, cy: f32, ch: u8) {
    macro_rules! seg {
        ($x1:expr,$y1:expr, $x2:expr,$y2:expr) => {
            ($x1 as f32, $y1 as f32, $x2 as f32, $y2 as f32)
        };
    }

    let segs: &[(f32,f32,f32,f32)] = match ch.to_ascii_uppercase() {
        b'A' => &[seg!(0,8,  0,2), seg!(0,2,  1,0), seg!(1,0,  4,0), seg!(4,0,  5,2),
                  seg!(5,2,  5,8), seg!(0,5,  5,5)],
        b'B' => &[seg!(0,0,  0,8), seg!(0,0,  4,0), seg!(4,0,  5,1), seg!(5,1,  5,3),
                  seg!(5,3,  4,4), seg!(0,4,  4,4), seg!(4,4,  5,5), seg!(5,5,  5,7),
                  seg!(5,7,  4,8), seg!(0,8,  4,8)],
        b'C' => &[seg!(5,1,  4,0), seg!(4,0,  1,0), seg!(1,0,  0,1), seg!(0,1,  0,7),
                  seg!(0,7,  1,8), seg!(1,8,  4,8), seg!(4,8,  5,7)],
        b'D' => &[seg!(0,0,  0,8), seg!(0,0,  3,0), seg!(3,0,  5,2), seg!(5,2,  5,6),
                  seg!(5,6,  3,8), seg!(3,8,  0,8)],
        b'E' => &[seg!(0,0,  0,8), seg!(0,0,  5,0), seg!(0,4,  4,4), seg!(0,8,  5,8)],
        b'F' => &[seg!(0,0,  0,8), seg!(0,0,  5,0), seg!(0,4,  4,4)],
        b'G' => &[seg!(5,1,  4,0), seg!(4,0,  1,0), seg!(1,0,  0,1), seg!(0,1,  0,7),
                  seg!(0,7,  1,8), seg!(1,8,  4,8), seg!(4,8,  5,7), seg!(5,7,  5,4),
                  seg!(3,4,  5,4)],
        b'H' => &[seg!(0,0,  0,8), seg!(5,0,  5,8), seg!(0,4,  5,4)],
        b'I' => &[seg!(1,0,  4,0), seg!(2,0,  2,8), seg!(1,8,  4,8)],
        b'J' => &[seg!(2,0,  5,0), seg!(4,0,  4,7), seg!(4,7,  3,8), seg!(3,8,  1,8),
                  seg!(1,8,  0,7)],
        b'K' => &[seg!(0,0,  0,8), seg!(5,0,  0,4), seg!(1,4,  5,8)],
        b'L' => &[seg!(0,0,  0,8), seg!(0,8,  5,8)],
        b'M' => &[seg!(0,8,  0,0), seg!(0,0,  3,5), seg!(3,5,  6,0), seg!(6,0,  6,8)],
        b'N' => &[seg!(0,8,  0,0), seg!(0,0,  5,8), seg!(5,8,  5,0)],
        b'O' => &[seg!(1,0,  4,0), seg!(4,0,  5,1), seg!(5,1,  5,7), seg!(5,7,  4,8),
                  seg!(4,8,  1,8), seg!(1,8,  0,7), seg!(0,7,  0,1), seg!(0,1,  1,0)],
        b'P' => &[seg!(0,8,  0,0), seg!(0,0,  4,0), seg!(4,0,  5,1), seg!(5,1,  5,3),
                  seg!(5,3,  4,4), seg!(4,4,  0,4)],
        b'Q' => &[seg!(1,0,  4,0), seg!(4,0,  5,1), seg!(5,1,  5,7), seg!(5,7,  4,8),
                  seg!(4,8,  1,8), seg!(1,8,  0,7), seg!(0,7,  0,1), seg!(0,1,  1,0),
                  seg!(3,6,  6,8)],
        b'R' => &[seg!(0,8,  0,0), seg!(0,0,  4,0), seg!(4,0,  5,1), seg!(5,1,  5,3),
                  seg!(5,3,  4,4), seg!(4,4,  0,4), seg!(2,4,  5,8)],
        b'S' => &[seg!(5,1,  4,0), seg!(4,0,  1,0), seg!(1,0,  0,1), seg!(0,1,  0,3),
                  seg!(0,3,  1,4), seg!(1,4,  4,4), seg!(4,4,  5,5), seg!(5,5,  5,7),
                  seg!(5,7,  4,8), seg!(4,8,  1,8), seg!(1,8,  0,7)],
        b'T' => &[seg!(0,0,  5,0), seg!(2,0,  2,8)],
        b'U' => &[seg!(0,0,  0,7), seg!(0,7,  1,8), seg!(1,8,  4,8), seg!(4,8,  5,7),
                  seg!(5,7,  5,0)],
        b'V' => &[seg!(0,0,  2,8), seg!(2,8,  5,0)],
        b'W' => &[seg!(0,0,  1,8), seg!(1,8,  3,4), seg!(3,4,  5,8), seg!(5,8,  6,0)],
        b'X' => &[seg!(0,0,  5,8), seg!(5,0,  0,8)],
        b'Y' => &[seg!(0,0,  2,4), seg!(5,0,  2,4), seg!(2,4,  2,8)],
        b'Z' => &[seg!(0,0,  5,0), seg!(5,0,  0,8), seg!(0,8,  5,8)],

        b'0' => &[seg!(1,0,  4,0), seg!(4,0,  5,1), seg!(5,1,  5,7), seg!(5,7,  4,8),
                  seg!(4,8,  1,8), seg!(1,8,  0,7), seg!(0,7,  0,1), seg!(0,1,  1,0),
                  seg!(1,2,  4,6)], // slash through 0 (CS 1.6 style)
        b'1' => &[seg!(1,2,  2,0), seg!(2,0,  2,8), seg!(1,8,  4,8)],
        b'2' => &[seg!(0,1,  1,0), seg!(1,0,  4,0), seg!(4,0,  5,1), seg!(5,1,  5,3),
                  seg!(5,3,  0,8), seg!(0,8,  5,8)],
        b'3' => &[seg!(0,1,  1,0), seg!(1,0,  4,0), seg!(4,0,  5,1), seg!(5,1,  5,3),
                  seg!(5,3,  3,4), seg!(0,4,  3,4), seg!(3,4,  5,5), seg!(5,5,  5,7),
                  seg!(5,7,  4,8), seg!(4,8,  1,8), seg!(1,8,  0,7)],
        b'4' => &[seg!(0,0,  0,4), seg!(0,4,  5,4), seg!(4,0,  4,8)],
        b'5' => &[seg!(5,0,  0,0), seg!(0,0,  0,4), seg!(0,4,  4,4), seg!(4,4,  5,5),
                  seg!(5,5,  5,7), seg!(5,7,  4,8), seg!(4,8,  1,8), seg!(1,8,  0,7)],
        b'6' => &[seg!(5,1,  4,0), seg!(4,0,  1,0), seg!(1,0,  0,1), seg!(0,1,  0,7),
                  seg!(0,7,  1,8), seg!(1,8,  4,8), seg!(4,8,  5,7), seg!(5,7,  5,5),
                  seg!(5,5,  4,4), seg!(4,4,  0,4)],
        b'7' => &[seg!(0,0,  5,0), seg!(5,0,  2,8)],
        b'8' => &[seg!(1,0,  4,0), seg!(4,0,  5,1), seg!(5,1,  5,3), seg!(5,3,  4,4),
                  seg!(1,4,  4,4), seg!(4,4,  5,5), seg!(5,5,  5,7), seg!(5,7,  4,8),
                  seg!(4,8,  1,8), seg!(1,8,  0,7), seg!(0,7,  0,5), seg!(0,5,  1,4),
                  seg!(1,4,  0,3), seg!(0,3,  0,1), seg!(0,1,  1,0)],
        b'9' => &[seg!(5,4,  1,4), seg!(1,4,  0,3), seg!(0,3,  0,1), seg!(0,1,  1,0),
                  seg!(1,0,  4,0), seg!(4,0,  5,1), seg!(5,1,  5,7), seg!(5,7,  4,8),
                  seg!(4,8,  1,8)],

        b'.' => &[seg!(2,7,  3,7), seg!(3,7,  3,8), seg!(3,8,  2,8), seg!(2,8,  2,7)],
        b',' => &[seg!(3,7,  2,9)],
        b':' => &[seg!(2,2,  3,2), seg!(2,6,  3,6)],
        b';' => &[seg!(2,2,  3,2), seg!(3,6,  2,8)],
        b'!' => &[seg!(2,0,  2,5), seg!(2,7,  2,8)],
        b'?' => &[seg!(0,1,  1,0), seg!(1,0,  4,0), seg!(4,0,  5,1), seg!(5,1,  5,3),
                  seg!(5,3,  3,5), seg!(3,5,  3,6), seg!(3,7,  3,8)],
        b'-' => &[seg!(1,4,  4,4)],
        b'+' => &[seg!(1,4,  4,4), seg!(2,2,  2,6)],
        b'=' => &[seg!(1,3,  4,3), seg!(1,5,  4,5)],
        b'_' => &[seg!(0,8,  5,8)],
        b'/' => &[seg!(0,8,  5,0)],
        b'\\' => &[seg!(0,0,  5,8)],
        b'(' => &[seg!(4,0,  2,2), seg!(2,2,  2,6), seg!(2,6,  4,8)],
        b')' => &[seg!(2,0,  4,2), seg!(4,2,  4,6), seg!(4,6,  2,8)],
        b'[' => &[seg!(4,0,  2,0), seg!(2,0,  2,8), seg!(2,8,  4,8)],
        b']' => &[seg!(2,0,  4,0), seg!(4,0,  4,8), seg!(4,8,  2,8)],
        b'<' => &[seg!(4,0,  1,4), seg!(1,4,  4,8)],
        b'>' => &[seg!(1,0,  4,4), seg!(4,4,  1,8)],
        b'*' => &[seg!(1,1,  4,7), seg!(4,1,  1,7), seg!(0,4,  5,4)],
        b'#' => &[seg!(1,0,  1,8), seg!(4,0,  4,8), seg!(0,3,  5,3), seg!(0,6,  5,6)],
        b'%' => &[seg!(0,8,  5,0), seg!(1,0,  1,2), seg!(0,1,  2,1), seg!(4,6,  4,8),
                  seg!(3,7,  5,7)],
        b'\'' => &[seg!(2,0,  2,2)],
        b'"' => &[seg!(1,0,  1,2), seg!(3,0,  3,2)],
        b'~' => &[seg!(0,4,  1,3), seg!(1,3,  2,4), seg!(2,4,  3,3), seg!(3,3,  4,4),
                  seg!(4,4,  5,3)],
        b'|' => &[seg!(2,0,  2,8)],
        b'^' => &[seg!(1,3,  3,0), seg!(3,0,  5,3)],
        b' ' => &[],
        _    => &[seg!(0,0,  4,0), seg!(4,0,  4,8), seg!(4,8,  0,8), seg!(0,8,  0,0)],
    };

    for &(x1, y1, x2, y2) in segs {
        glVertex2f(cx + x1 * SC, cy + y1 * SC);
        glVertex2f(cx + x2 * SC, cy + y2 * SC);
    }
}


/// Draw text at screen position (x, y) using the stroke font.
/// Draws a dark shadow first for contrast, then the colored text on top.
/// Works in windowed AND fullscreen - uses only glVertex2f, same as boxes/lines.
pub unsafe fn draw_text(_hdc: HDC, x: f32, y: f32, text: &str, c: [f32; 4]) {
    if text.is_empty() { return; }

    // Shadow pass (dark, slightly offset for readability)
    glColor4f(0.0, 0.0, 0.0, c[3] * 0.75);
    glBegin(GL_LINES);
    let mut cx = 0.0f32;
    for &b in text.as_bytes() {
        draw_stroke_char(x + cx + 1.0, y + 1.0, b);
        cx += CHAR_W;
    }
    glEnd();

    // Foreground pass
    glColor4f(c[0], c[1], c[2], c[3]);
    glBegin(GL_LINES);
    cx = 0.0;
    for &b in text.as_bytes() {
        draw_stroke_char(x + cx, y, b);
        cx += CHAR_W;
    }
    glEnd();
}