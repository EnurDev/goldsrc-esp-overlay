# GoldSrc ESP Overlay - CS 1.6

> [!NOTE]
> **Educational purposes only.** As far as I can tell, CS 1.6 VAC is pretty weak, so getting banned from it is unlikely - that said, **you play with it at your own risk!**

A 32-bit Rust DLL + Python injector that draws an ESP (Extra-Sensory Perception) overlay inside Counter-Strike 1.6. Renders bounding boxes, player names, distance, weapon info, and team colors on top of the game using an OpenGL hook.

![ESP Screenshot in Action](image.png)

---

## Compatibility

| Property         | Value                        |
|------------------|------------------------------|
| Game             | Counter-Strike 1.6 (GoldSrc) |
| Tested Build     | **4554**                     |
| Protocol         | **48**                       |
| Executable Version | **1.1.2.6 / 2.0.0.0**     |
| Target Arch      | 32-bit (i686, WoW64)         |

> Offsets in `src/entities.rs` are hardcoded for **Build 4554**. Other builds will likely require offset adjustments.

---

## Features

- **Bounding boxes** with corner brackets around all visible players
- **Team colors** - Red for Terrorists, Blue for Counter-Terrorists
- **Snap-lines** from the bottom of screen to each player's feet
- **Name label** above each box
- **Distance and weapon** shown below each box
- **Box fade-out** - cached boxes fade smoothly when a player temporarily disappears
- **F6 hotkey** to toggle the overlay on/off in-game
- **Cross-architecture injection** - 64-bit Python to 32-bit `hl.exe`
- **Live debug log** streamed to your terminal after injection

---

## Project Structure

```
├── inject.py          # Python injector (LoadLibraryA + CreateRemoteThread)
├── Cargo.toml         # Rust project manifest
└── src/
    ├── lib.rs         # DLL entry point (DllMain, worker thread)
    ├── hook.rs        # wglSwapBuffers detour lifecycle (MinHook)
    ├── esp.rs         # ESP drawing logic (boxes, labels, snap-lines)
    ├── render.rs      # OpenGL 1.x drawing primitives (lines, text, rects)
    ├── entities.rs    # Engine API access, memory reading, player data
    └── math.rs        # Vec3 math (distance, is_zero)
```

---

## Prerequisites

- **Windows 10/11**
- **Rust** with MSVC toolchain → [rustup.rs](https://rustup.rs)
- **32-bit target:**
  ```
  rustup target add i686-pc-windows-msvc
  ```
- **Visual Studio Build Tools** with the C++ workload (needed by `minhook-sys`)
- **Python 3.6+** with psutil:
  ```
  pip install psutil
  ```

---

## Build

```bash
# Release build (recommended)
cargo build --release --target i686-pc-windows-msvc
```

Output DLL:
```
target/i686-pc-windows-msvc/release/goldsrc_diag_overlay.dll
```

---

## Usage

1. Launch **Counter-Strike 1.6** and load into a game.
2. Open a terminal **as Administrator** in the project folder.
3. Run the injector:
   ```bash
   python inject.py
   ```
   Or specify a custom DLL path:
   ```bash
   python inject.py path\to\goldsrc_diag_overlay.dll
   ```
4. The overlay activates immediately. Press **F6** to toggle it on/off.

---

## In-Game Controls

| Key   | Action              |
|-------|---------------------|
| **F6** | Toggle ESP on/off  |

---

## How It Works

### Injection (`inject.py`)
Uses the classic **CreateRemoteThread + LoadLibraryA** technique:
1. Find `hl.exe` by process name
2. Open the process with full access
3. Allocate memory in the target for the DLL path string
4. Write the DLL path to that memory
5. Resolve `LoadLibraryA` - handles 64-bit injector to 32-bit target automatically
6. Create a remote thread that calls `LoadLibraryA(dll_path)`
7. Tail the DLL's debug log in real-time

### Hook (`hook.rs` + `entities.rs`)
- Hooks `client.dll!Initialize` to capture the **engine function table** (`cl_enginefunc_t*`)
- Hooks `opengl32!wglSwapBuffers` using **MinHook** to intercept each rendered frame
- Falls back to memory scanning to locate the engine table if already initialized

### ESP (`esp.rs` + `render.rs`)
- Each frame: reads all 32 player slots via `GetEntityByIndex`
- Projects 3D world positions to 2D screen coordinates via the engine's TriAPI `WorldToScreen`
- Draws boxes, snap-lines, and text using **OpenGL immediate-mode** (`glBegin`/`glEnd`)

---

## Troubleshooting

| Problem | Solution |
|---------|----------|
| `Failed to open process` | Run as **Administrator** |
| `DLL not found` | Build first: `cargo build --release --target i686-pc-windows-msvc` |
| `Process not found` | Make sure `hl.exe` is running before injecting |
| `LoadLibrary returned NULL` | Check `esp_debug.log` next to the DLL for error messages |
| No boxes visible | Make sure you are in an active game; spectator mode is not supported |

---

## Disclaimer

This project is strictly for **educational and research purposes** - studying GoldSrc internals, reverse engineering, and low-level Windows programming. The code demonstrates:

- Windows DLL injection techniques
- Inline function hooking (trampoline/JMP patch)
- OpenGL overlay rendering
- PE export table parsing
- Process memory reading via the Windows API

From what is known, CS 1.6's VAC implementation is not very advanced, so getting banned is unlikely - but as always, **use at your own risk.**