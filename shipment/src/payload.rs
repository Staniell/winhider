/*
 * =============================================================================
 * WinHider Payload DLL - Window Manipulation Library
 * =============================================================================
 *
 * Filename: payload.rs
 * Author: bigwiz
 * Description: Dynamic link library (DLL) payload for WinHider that performs
 *              window display affinity manipulation. Loaded into target
 *              processes via SetWindowsHookEx to hide/show windows from screen capture.
 *
 * Action Communication:
 *   The injector creates a named shared memory region "WinHider_Action_{pid}"
 *   containing 16 bytes:
 *     Bytes 0-7: target HWND (isize) — the specific window to modify
 *     Byte 8:    action bitmask:
 *       1 = Hide from capture (WDA_EXCLUDEFROMCAPTURE)
 *       2 = Show in capture (WDA_NONE)
 *
 * Created: 2024
 * License: Proprietary - Bitmutex Technologies
 * =============================================================================
 */

use windows::core::PCWSTR;
use windows::Win32::Foundation::*;
use windows::Win32::System::Memory::*;
use windows::Win32::System::SystemServices::*;
use windows::Win32::System::Threading::GetCurrentProcessId;
use windows::Win32::UI::WindowsAndMessaging::*;

#[unsafe(no_mangle)]
#[allow(non_snake_case, unused_variables)]
pub extern "system" fn DllMain(
    dll_module: HINSTANCE,
    call_reason: u32,
    reserved: *mut std::ffi::c_void,
) -> BOOL {
    match call_reason {
        DLL_PROCESS_ATTACH => {
            std::thread::spawn(|| {
                unsafe { apply_stealth(); }
            });
        }
        _ => {}
    }
    BOOL(1)
}

/// Exported hook procedure for SetWindowsHookEx-based DLL loading.
/// Simply forwards the call to the next hook in the chain.
#[unsafe(no_mangle)]
pub unsafe extern "system" fn HookProc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

unsafe fn apply_stealth() {
    unsafe {
        let current_pid = GetCurrentProcessId();

        // Read target HWND + action from named shared memory
        let map_name = format!("WinHider_Action_{}\0", current_pid);
        let wide_name: Vec<u16> = map_name.encode_utf16().collect();

        let mapping = OpenFileMappingW(FILE_MAP_READ.0, false, PCWSTR(wide_name.as_ptr()));
        let mapping = match mapping {
            Ok(m) => m,
            Err(_) => return, // No shared memory found — nothing to do
        };

        const SHARED_MEM_SIZE: usize = 16;
        let view = MapViewOfFile(mapping, FILE_MAP_READ, 0, 0, SHARED_MEM_SIZE);
        if view.Value.is_null() {
            let _ = CloseHandle(mapping);
            return;
        }

        // Read target HWND (bytes 0-7) and action mask (byte 8)
        let ptr = view.Value as *const u8;
        let mut hwnd_bytes = [0u8; 8];
        std::ptr::copy_nonoverlapping(ptr, hwnd_bytes.as_mut_ptr(), 8);
        let target_hwnd = HWND(isize::from_ne_bytes(hwnd_bytes));
        let action_mask = *ptr.add(8);

        let _ = UnmapViewOfFile(view);
        let _ = CloseHandle(mapping);

        if action_mask == 0 {
            return;
        }

        // Apply action to the specific target window only
        if (action_mask & 1) != 0 {
            let _ = SetWindowDisplayAffinity(target_hwnd, WDA_EXCLUDEFROMCAPTURE);
        }
        if (action_mask & 2) != 0 {
            let _ = SetWindowDisplayAffinity(target_hwnd, WDA_NONE);
        }
    }
}
