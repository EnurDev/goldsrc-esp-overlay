"""
inject.py - DLL Injector for GoldSrc Engine (hl.exe)

Injects a 32-bit DLL into a running GoldSrc process using the classic
LoadLibraryA + CreateRemoteThread technique. Supports cross-architecture
injection (64-bit Python -> 32-bit target).

Usage:
    python inject.py                  # uses default DLL path
    python inject.py path/to/dll      # uses custom DLL path

Requirements:
    - Windows OS
    - Python 3.6+
    - psutil  (pip install psutil)
    - Run as Administrator
"""

import ctypes
import struct
import sys
import os
import psutil
from ctypes import wintypes

# ============================================================
# Windows API Constants
# ============================================================

PROCESS_ALL_ACCESS = 0x1F0FFF       # Full access rights to a process
MEM_COMMIT         = 0x00001000     # Commit memory pages
MEM_RESERVE        = 0x00002000     # Reserve memory pages
PAGE_READWRITE     = 0x04           # Read/write page protection
TH32CS_SNAPMODULE  = 0x00000008     # Snapshot: modules of a process
TH32CS_SNAPMODULE32= 0x00000010     # Snapshot: 32-bit modules (for WoW64)
INFINITE           = 0xFFFFFFFF     # Infinite wait timeout
WAIT_OBJECT_0      = 0x00000000     # Wait completed successfully

# ============================================================
# Kernel32 DLL - Load & Define Function Signatures
# ============================================================

# Load kernel32.dll with error tracking enabled
kernel32 = ctypes.WinDLL('kernel32', use_last_error=True)

# OpenProcess - opens an existing process object
kernel32.OpenProcess.argtypes = [wintypes.DWORD, wintypes.BOOL, wintypes.DWORD]
kernel32.OpenProcess.restype = wintypes.HANDLE

# VirtualAllocEx - allocates memory in a remote process
kernel32.VirtualAllocEx.argtypes = [
    wintypes.HANDLE, wintypes.LPVOID, ctypes.c_size_t,
    wintypes.DWORD, wintypes.DWORD
]
kernel32.VirtualAllocEx.restype = wintypes.LPVOID

# WriteProcessMemory - writes data to a remote process's memory
kernel32.WriteProcessMemory.argtypes = [
    wintypes.HANDLE, wintypes.LPVOID, wintypes.LPCVOID,
    ctypes.c_size_t, ctypes.POINTER(ctypes.c_size_t)
]
kernel32.WriteProcessMemory.restype = wintypes.BOOL

# ReadProcessMemory - reads data from a remote process's memory
kernel32.ReadProcessMemory.argtypes = [
    wintypes.HANDLE, wintypes.LPCVOID, wintypes.LPVOID,
    ctypes.c_size_t, ctypes.POINTER(ctypes.c_size_t)
]
kernel32.ReadProcessMemory.restype = wintypes.BOOL

# CreateRemoteThread - creates a thread in a remote process
kernel32.CreateRemoteThread.argtypes = [
    wintypes.HANDLE, wintypes.LPVOID, ctypes.c_size_t,
    wintypes.LPVOID, wintypes.LPVOID, wintypes.DWORD, wintypes.LPVOID
]
kernel32.CreateRemoteThread.restype = wintypes.HANDLE

# WaitForSingleObject - waits for a thread/process to signal
kernel32.WaitForSingleObject.argtypes = [wintypes.HANDLE, wintypes.DWORD]
kernel32.WaitForSingleObject.restype = wintypes.DWORD

# GetExitCodeThread - gets the exit code of a finished thread
kernel32.GetExitCodeThread.argtypes = [wintypes.HANDLE, wintypes.LPDWORD]
kernel32.GetExitCodeThread.restype = wintypes.BOOL

# CloseHandle - closes an open handle
kernel32.CloseHandle.argtypes = [wintypes.HANDLE]
kernel32.CloseHandle.restype = wintypes.BOOL

# CreateToolhelp32Snapshot - takes a snapshot of processes/modules
kernel32.CreateToolhelp32Snapshot.argtypes = [wintypes.DWORD, wintypes.DWORD]
kernel32.CreateToolhelp32Snapshot.restype = wintypes.HANDLE

# IsWow64Process - checks if a process runs under WoW64 (32-bit on 64-bit OS)
kernel32.IsWow64Process.argtypes = [wintypes.HANDLE, ctypes.POINTER(wintypes.BOOL)]
kernel32.IsWow64Process.restype = wintypes.BOOL

# ============================================================
# MODULEENTRY32 - Structure for module snapshot enumeration
# ============================================================

class MODULEENTRY32(ctypes.Structure):
    """Describes a module (DLL) loaded in a process.
    Used with Module32First/Module32Next to iterate loaded modules."""
    _fields_ = [
        ('dwSize',        wintypes.DWORD),       # Size of this struct (must be set before use)
        ('th32ModuleID',  wintypes.DWORD),        # Unused (always 1)
        ('th32ProcessID', wintypes.DWORD),        # Owner process ID
        ('GlblcntUsage',  wintypes.DWORD),        # Global usage count (unused)
        ('ProccntUsage',  wintypes.DWORD),        # Process usage count (unused)
        ('modBaseAddr',   ctypes.c_void_p),       # Base address of the module
        ('modBaseSize',   wintypes.DWORD),        # Size of the module in bytes
        ('hModule',       wintypes.HMODULE),      # Handle to the module
        ('szModule',      ctypes.c_char * 256),   # Module name (e.g. "kernel32.dll")
        ('szExePath',     ctypes.c_char * 260),   # Full path to the module file
    ]

# Module32First/Next - iterate through a module snapshot
kernel32.Module32First.argtypes = [wintypes.HANDLE, ctypes.POINTER(MODULEENTRY32)]
kernel32.Module32First.restype = wintypes.BOOL
kernel32.Module32Next.argtypes = [wintypes.HANDLE, ctypes.POINTER(MODULEENTRY32)]
kernel32.Module32Next.restype = wintypes.BOOL

# ============================================================
# Helper Functions
# ============================================================

def find_process_by_name(process_name):
    """Search all running processes and return the PID matching the given name.
    Returns None if the process is not found."""
    for proc in psutil.process_iter(['pid', 'name']):
        try:
            if proc.info['name'].lower() == process_name.lower():
                return proc.info['pid']
        except (psutil.NoSuchProcess, psutil.AccessDenied, psutil.ZombieProcess):
            pass
    return None


def is_target_32bit(h_process):
    """Check if the target process is 32-bit (running under WoW64).
    Returns True if the process is 32-bit on a 64-bit OS."""
    is_wow64 = wintypes.BOOL(False)
    kernel32.IsWow64Process(h_process, ctypes.byref(is_wow64))
    return bool(is_wow64.value)


def is_self_64bit():
    """Check if the current Python interpreter is 64-bit.
    Returns True if pointer size is 8 bytes (64-bit)."""
    return ctypes.sizeof(ctypes.c_void_p) == 8


def find_remote_module_base(process_id, module_name):
    """Find a module's base address in a remote process.

    Uses CreateToolhelp32Snapshot to enumerate all modules loaded
    in the target process, then returns the base address of the
    module matching the given name.

    Args:
        process_id:  PID of the target process
        module_name: Name of the module to find (e.g. "kernel32.dll")

    Returns:
        Base address (int) of the module, or None if not found.
    """
    # Take a snapshot of all modules in the process
    snap = kernel32.CreateToolhelp32Snapshot(
        TH32CS_SNAPMODULE | TH32CS_SNAPMODULE32, process_id
    )
    if snap == -1 or snap == ctypes.c_void_p(-1).value:
        return None

    # Prepare the module entry structure
    me32 = MODULEENTRY32()
    me32.dwSize = ctypes.sizeof(MODULEENTRY32)

    found_base = None

    # Walk through the module list
    if kernel32.Module32First(snap, ctypes.byref(me32)):
        while True:
            name = me32.szModule.decode('utf-8', errors='ignore').lower()
            if name == module_name.lower():
                found_base = me32.modBaseAddr
                break
            if not kernel32.Module32Next(snap, ctypes.byref(me32)):
                break

    kernel32.CloseHandle(snap)
    return found_base


def find_remote_export(h_process, module_base, function_name):
    """Parse the PE export table of a remote module to find a function address.

    This reads the PE headers directly from the target process's memory,
    which allows finding function addresses even when the injector and
    target have different architectures (64-bit -> 32-bit).

    How it works:
      1. Read the DOS header to find PE header offset (e_lfanew)
      2. Read the PE optional header to locate the export directory RVA
      3. Read the export directory to get name/ordinal/function arrays
      4. Search the name array for the desired function
      5. Use the ordinal to index into the function address array

    Args:
        h_process:     Handle to the target process
        module_base:   Base address of the module in the target process
        function_name: Name of the exported function (e.g. "LoadLibraryA")

    Returns:
        Virtual address (int) of the function, or None if not found.
    """
    func_name_bytes = function_name.encode('ascii')
    bytes_read = ctypes.c_size_t(0)

    # --- Step 1: Read DOS header to get PE header offset ---
    dos_header = (ctypes.c_byte * 64)()
    if not kernel32.ReadProcessMemory(
        h_process, module_base, dos_header, 64, ctypes.byref(bytes_read)
    ):
        return None

    # e_lfanew (at offset 0x3C) points to the PE signature
    e_lfanew = struct.unpack_from('<I', bytes(dos_header), 0x3C)[0]

    # --- Step 2: Read PE header + optional header ---
    pe_header = (ctypes.c_byte * 256)()
    if not kernel32.ReadProcessMemory(
        h_process, module_base + e_lfanew, pe_header, 256, ctypes.byref(bytes_read)
    ):
        return None

    pe_data = bytes(pe_header)

    # Verify PE signature ("PE\0\0" = 0x4550)
    pe_sig = struct.unpack_from('<I', pe_data, 0)[0]
    if pe_sig != 0x4550:
        return None

    # Determine PE format from magic number
    # Offset 24 = start of optional header (after 4-byte PE sig + 20-byte file header)
    magic = struct.unpack_from('<H', pe_data, 24)[0]

    if magic == 0x10B:     # PE32 (32-bit) - export dir RVA at optional header offset 96
        export_rva = struct.unpack_from('<I', pe_data, 24 + 96)[0]
    elif magic == 0x20B:   # PE32+ (64-bit) - export dir RVA at optional header offset 112
        export_rva = struct.unpack_from('<I', pe_data, 24 + 112)[0]
    else:
        return None  # Unknown PE format

    if export_rva == 0:
        return None  # Module has no exports

    # --- Step 3: Read the export directory (40 bytes) ---
    export_dir = (ctypes.c_byte * 40)()
    if not kernel32.ReadProcessMemory(
        h_process, module_base + export_rva, export_dir, 40, ctypes.byref(bytes_read)
    ):
        return None

    ed = bytes(export_dir)
    num_functions = struct.unpack_from('<I', ed, 20)[0]  # Total exported functions
    num_names     = struct.unpack_from('<I', ed, 24)[0]  # Number of named exports
    func_rvas_rva = struct.unpack_from('<I', ed, 28)[0]  # RVA of function address array
    name_rvas_rva = struct.unpack_from('<I', ed, 32)[0]  # RVA of name pointer array
    ordinals_rva  = struct.unpack_from('<I', ed, 36)[0]  # RVA of ordinal array

    # --- Step 4: Read the three export arrays ---
    # Name RVA array (each entry is a 4-byte pointer to a function name string)
    name_rvas_buf = (ctypes.c_byte * (num_names * 4))()
    kernel32.ReadProcessMemory(
        h_process, module_base + name_rvas_rva,
        name_rvas_buf, num_names * 4, ctypes.byref(bytes_read)
    )

    # Ordinal array (each entry is a 2-byte index into the function address array)
    ordinals_buf = (ctypes.c_byte * (num_names * 2))()
    kernel32.ReadProcessMemory(
        h_process, module_base + ordinals_rva,
        ordinals_buf, num_names * 2, ctypes.byref(bytes_read)
    )

    # Function RVA array (each entry is a 4-byte RVA of the function body)
    func_rvas_buf = (ctypes.c_byte * (num_functions * 4))()
    kernel32.ReadProcessMemory(
        h_process, module_base + func_rvas_rva,
        func_rvas_buf, num_functions * 4, ctypes.byref(bytes_read)
    )

    name_rvas_data = bytes(name_rvas_buf)
    ordinals_data  = bytes(ordinals_buf)
    func_rvas_data = bytes(func_rvas_buf)

    # --- Step 5: Search for the target function by name ---
    for i in range(num_names):
        # Get the RVA that points to this export's name string
        name_rva = struct.unpack_from('<I', name_rvas_data, i * 4)[0]

        # Read the name string from the remote process
        name_buf = (ctypes.c_byte * 128)()
        kernel32.ReadProcessMemory(
            h_process, module_base + name_rva,
            name_buf, 128, ctypes.byref(bytes_read)
        )
        name = bytes(name_buf).split(b'\x00')[0]

        if name == func_name_bytes:
            # Found it! Use the ordinal to index the function address array
            ordinal  = struct.unpack_from('<H', ordinals_data, i * 2)[0]
            func_rva = struct.unpack_from('<I', func_rvas_data, ordinal * 4)[0]
            return module_base + func_rva  # Convert RVA to virtual address

    return None  # Function not found

# ============================================================
# Core Injection Logic
# ============================================================

def inject_dll(process_id, dll_path):
    """Inject a DLL into a target process using LoadLibraryA.

    Technique (CreateRemoteThread + LoadLibraryA):
      1. Open the target process with full access
      2. Allocate memory in the target for the DLL path string
      3. Write the DLL path into that memory
      4. Resolve LoadLibraryA address (handling cross-arch if needed)
      5. Create a remote thread that calls LoadLibraryA(dll_path)
      6. Wait for the thread to complete and check the result

    Args:
        process_id: PID of the target process
        dll_path:   Absolute path to the DLL file to inject

    Returns:
        True if injection succeeded, False otherwise.
    """
    # Validate the DLL file exists
    if not os.path.exists(dll_path):
        print(f"[ERROR] DLL not found: {dll_path}")
        return False

    dll_path = os.path.abspath(dll_path)
    dll_bytes = dll_path.encode('utf-8') + b'\x00'  # Null-terminated string
    dll_size = len(dll_bytes)

    # --- Open the target process ---
    print(f"[*] Opening process (PID: {process_id})...")
    h_process = kernel32.OpenProcess(PROCESS_ALL_ACCESS, False, process_id)
    if not h_process:
        print(f"[ERROR] Failed to open process. Error: {ctypes.get_last_error()}")
        print("[ERROR] Make sure you run as Administrator.")
        return False

    # Detect architecture mismatch (64-bit injector -> 32-bit target)
    target_32bit = is_target_32bit(h_process)
    self_64bit = is_self_64bit()
    cross_arch = self_64bit and target_32bit

    print(f"[*] Injector: {'64-bit' if self_64bit else '32-bit'}")
    print(f"[*] Target:   {'32-bit (WoW64)' if target_32bit else '64-bit'}")
    if cross_arch:
        print("[*] Cross-architecture injection (64-bit -> 32-bit)")

    try:
        # --- Allocate memory in the target process for the DLL path ---
        remote_memory = kernel32.VirtualAllocEx(
            h_process, None, dll_size,
            MEM_COMMIT | MEM_RESERVE, PAGE_READWRITE
        )
        if not remote_memory:
            print(f"[ERROR] Memory allocation failed. Error: {ctypes.get_last_error()}")
            return False
        print(f"[*] Allocated {dll_size} bytes at 0x{remote_memory:X}")

        # --- Write the DLL path string into the allocated memory ---
        bytes_written = ctypes.c_size_t(0)
        if not kernel32.WriteProcessMemory(
            h_process, remote_memory, dll_bytes, dll_size,
            ctypes.byref(bytes_written)
        ):
            print(f"[ERROR] Write failed. Error: {ctypes.get_last_error()}")
            return False
        print(f"[*] Wrote {bytes_written.value} bytes (DLL path)")

        # --- Resolve LoadLibraryA address ---
        if cross_arch:
            # For cross-arch: find kernel32.dll in the 32-bit target and
            # parse its PE export table to get the 32-bit LoadLibraryA address
            kernel32_base = find_remote_module_base(process_id, "kernel32.dll")
            if kernel32_base is None:
                print("[ERROR] Could not find kernel32.dll in target process")
                return False

            load_library_addr = find_remote_export(h_process, kernel32_base, "LoadLibraryA")
            if load_library_addr is None:
                print("[ERROR] Could not find LoadLibraryA in target's kernel32.dll")
                return False
        else:
            # Same architecture: local kernel32 address matches the target's
            load_library_addr = kernel32.GetProcAddress(
                kernel32.GetModuleHandleW("kernel32.dll"), b"LoadLibraryA"
            )
            if not load_library_addr:
                print(f"[ERROR] GetProcAddress failed. Error: {ctypes.get_last_error()}")
                return False

        print(f"[*] LoadLibraryA at 0x{load_library_addr:X}")

        # Sanity check: 32-bit target can't use 64-bit addresses
        if target_32bit and load_library_addr > 0xFFFFFFFF:
            print("[ERROR] LoadLibraryA address exceeds 32-bit range!")
            return False

        # --- Create a remote thread that calls LoadLibraryA(dll_path) ---
        h_thread = kernel32.CreateRemoteThread(
            h_process, None, 0,
            load_library_addr,  # Thread start = LoadLibraryA
            remote_memory,      # Thread param = pointer to DLL path
            0, None
        )
        if not h_thread:
            print(f"[ERROR] CreateRemoteThread failed. Error: {ctypes.get_last_error()}")
            return False
        print(f"[*] Remote thread created (handle: {h_thread})")

        # --- Wait for the DLL to finish loading ---
        print("[*] Waiting for DLL to load...")
        wait_result = kernel32.WaitForSingleObject(h_thread, 10000)  # 10s timeout
        if wait_result == WAIT_OBJECT_0:
            # Thread finished - check if LoadLibrary returned a valid handle
            exit_code = wintypes.DWORD(0)
            kernel32.GetExitCodeThread(h_thread, ctypes.byref(exit_code))
            if exit_code.value != 0:
                print(f"[SUCCESS] DLL loaded! Module handle: 0x{exit_code.value:X}")
            else:
                print("[WARNING] LoadLibrary returned NULL - DLL may have failed to load")
                return False
        else:
            print(f"[WARNING] Wait timed out or failed (result: {wait_result})")

        kernel32.CloseHandle(h_thread)
        return True

    finally:
        # Always close the process handle
        kernel32.CloseHandle(h_process)

# ============================================================
# Entry Point
# ============================================================

def main():
    """Main entry point. Finds the target process and injects the DLL."""
    TARGET_PROCESS = "hl.exe"
    script_dir = os.path.dirname(os.path.abspath(__file__))

    # Allow custom DLL path via command-line argument
    if len(sys.argv) > 1:
        dll_path = sys.argv[1]
    else:
        dll_path = os.path.join(
            script_dir, "target", "i686-pc-windows-msvc",
            "release", "goldsrc_diag_overlay.dll"
        )

    # Print banner
    print("=" * 60)
    print("GoldSrc ESP Overlay Injector")
    print("=" * 60)
    print(f"Target Process: {TARGET_PROCESS}")
    print(f"DLL Path:       {dll_path}")
    print(f"Injector Arch:  {'64-bit' if is_self_64bit() else '32-bit'}")
    print(f"Hotkey:         F6 = toggle overlay on/off in-game")
    print("=" * 60)
    print()

    # Validate the DLL exists
    if not os.path.exists(dll_path):
        print(f"[ERROR] DLL not found: {dll_path}")
        print("\nBuild the DLL first:")
        print("  cargo build --release --target i686-pc-windows-msvc")
        sys.exit(1)

    # Find the target process
    print(f"[*] Searching for {TARGET_PROCESS}...")
    process_id = find_process_by_name(TARGET_PROCESS)
    if process_id is None:
        print(f"[ERROR] '{TARGET_PROCESS}' not found! Make sure it is running.")
        sys.exit(1)
    print(f"[*] Found {TARGET_PROCESS} (PID: {process_id})\n")

    # Inject the DLL
    if inject_dll(process_id, dll_path):
        print(f"\n[*] Injection successful!")
    else:
        print(f"\n[!] Injection failed!")
        sys.exit(1)


if __name__ == "__main__":
    main()
