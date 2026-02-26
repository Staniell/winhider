/*
 * =============================================================================
 * WinHider Core - Shared Library
 * =============================================================================
 *
 * Shared functionality between the WinHider GUI app and CLI tool.
 * Contains hook-based injection logic, window manipulation, and utility functions.
 *
 * Designed At - Bitmutex Technologies
 * =============================================================================
 */

use windows::core::{s, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::System::Diagnostics::ToolHelp::*;
use windows::Win32::System::LibraryLoader::*;
use windows::Win32::System::Memory::*;
use windows::Win32::System::Threading::*;
use windows::Win32::UI::WindowsAndMessaging::*;

// Windows to ignore in the list
pub const IGNORED_WINDOWS: &[&str] = &[
    "Program Manager",
    "Settings",
    "Microsoft Text Input Application",
    "WinHider",
];

pub enum InjectionAction {
    HideCapture,
    ShowCapture,
}

pub fn get_pid(hwnd: HWND) -> u32 {
    let mut pid = 0;
    unsafe {
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
    }
    pid
}

pub fn set_taskbar_visibility_external(hwnd: HWND, hide: bool) -> Result<(), String> {
    unsafe {
        let mut style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
        if hide {
            style &= !WS_EX_APPWINDOW.0;
            style |= WS_EX_TOOLWINDOW.0;
        } else {
            style &= !WS_EX_TOOLWINDOW.0;
            style |= WS_EX_APPWINDOW.0;
        }
        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, style as isize);
        SetWindowPos(
            hwnd,
            HWND(0),
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_FRAMECHANGED,
        )
        .map_err(|e| e.to_string())?;
    }
    Ok(())
}

pub fn set_always_on_top(hwnd: HWND, topmost: bool) -> Result<(), String> {
    unsafe {
        let z = if topmost { HWND_TOPMOST } else { HWND_NOTOPMOST };
        SetWindowPos(hwnd, z, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE)
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Injects the payload DLL into the target process using SetWindowsHookEx
/// with action communication via named shared memory.
///
/// The shared memory layout (16 bytes):
///   Bytes 0-7: target HWND (isize, identifies the specific window)
///   Byte 8:    action bitmask (1 = hide capture, 2 = show capture)
///
/// The process:
/// 1. Create named shared memory "WinHider_Action_{pid}" with HWND + action
/// 2. Load payload DLL into our own process via LoadLibraryW
/// 3. Get the exported HookProc address via GetProcAddress
/// 4. Get target window's thread ID via GetWindowThreadProcessId
/// 5. Install a WH_CALLWNDPROC hook on that thread via SetWindowsHookExW
/// 6. Trigger the hook with SendMessageTimeoutW (causes DLL to load in target)
/// 7. Unhook and clean up shared memory
pub fn inject_payload(target_hwnd: HWND, action: InjectionAction) -> Result<String, String> {
    unsafe {
        // Resolve the DLL path
        let mut dll_path = std::env::current_exe()
            .map_err(|e| e.to_string())?
            .parent()
            .ok_or("Cannot determine directory")?
            .join("winhider_payload.dll");

        if !dll_path.exists() {
            if let Ok(cwd) = std::env::current_dir() {
                dll_path = cwd
                    .join("target")
                    .join("release")
                    .join("winhider_payload.dll");
            }
        }
        if !dll_path.exists() {
            return Err("Base DLL not found".to_string());
        }

        // Determine the action bitmask
        let action_byte: u8 = match action {
            InjectionAction::HideCapture => 1,
            InjectionAction::ShowCapture => 2,
        };

        let action_name = match action {
            InjectionAction::HideCapture => "HideCapture",
            InjectionAction::ShowCapture => "ShowCapture",
        };

        // Get target PID for shared memory naming
        let mut target_pid = 0u32;
        GetWindowThreadProcessId(target_hwnd, Some(&mut target_pid));
        if target_pid == 0 {
            return Err("Failed to get target PID".to_string());
        }

        // 1. Create named shared memory with target HWND + action bitmask
        let map_name = format!("WinHider_Action_{}\0", target_pid);
        let wide_map_name: Vec<u16> = map_name.encode_utf16().collect();

        const SHARED_MEM_SIZE: usize = 16; // 8 bytes HWND + 1 byte action + padding

        let mapping = CreateFileMappingW(
            INVALID_HANDLE_VALUE,
            None,
            PAGE_READWRITE,
            0,
            SHARED_MEM_SIZE as u32,
            PCWSTR(wide_map_name.as_ptr()),
        )
        .map_err(|e| format!("CreateFileMappingW failed: {}", e))?;

        let view = MapViewOfFile(mapping, FILE_MAP_WRITE, 0, 0, SHARED_MEM_SIZE);
        if view.Value.is_null() {
            let _ = CloseHandle(mapping);
            return Err("MapViewOfFile failed".to_string());
        }

        // Write the target HWND (bytes 0-7) and action bitmask (byte 8)
        let ptr = view.Value as *mut u8;
        let hwnd_bytes = (target_hwnd.0 as isize).to_ne_bytes();
        std::ptr::copy_nonoverlapping(hwnd_bytes.as_ptr(), ptr, 8);
        *ptr.add(8) = action_byte;
        let _ = UnmapViewOfFile(view);

        // 2. Load the DLL into our own process
        let wide_path: Vec<u16> = dll_path
            .to_str()
            .ok_or("Invalid UTF-8 in path")?
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();

        let dll_handle = LoadLibraryW(PCWSTR(wide_path.as_ptr())).map_err(|e| {
            let _ = CloseHandle(mapping);
            format!("LoadLibraryW failed: {}", e)
        })?;

        // 3. Get the HookProc export
        let hook_proc_addr = GetProcAddress(dll_handle, s!("HookProc"));
        if hook_proc_addr.is_none() {
            let _ = FreeLibrary(dll_handle);
            let _ = CloseHandle(mapping);
            return Err("HookProc not found in DLL".to_string());
        }

        let hook_proc: HOOKPROC =
            Some(std::mem::transmute::<
                unsafe extern "system" fn() -> isize,
                unsafe extern "system" fn(i32, WPARAM, LPARAM) -> LRESULT,
            >(hook_proc_addr.unwrap()));

        // 4. Get the target window's thread ID
        let thread_id = GetWindowThreadProcessId(target_hwnd, None);
        if thread_id == 0 {
            let _ = FreeLibrary(dll_handle);
            let _ = CloseHandle(mapping);
            return Err("Failed to get target thread ID".to_string());
        }

        // 5. Install hook on the target thread — this causes the DLL to load in the target process
        let hook = SetWindowsHookExW(WH_CALLWNDPROC, hook_proc, dll_handle, thread_id)
            .map_err(|e| {
                let _ = FreeLibrary(dll_handle);
                let _ = CloseHandle(mapping);
                format!("SetWindowsHookExW failed: {}", e)
            })?;

        // 6. Trigger the hook by sending a message to the target window
        //    Using SendMessageTimeoutW with a 2-second timeout to avoid hanging
        let mut _result: usize = 0;
        let _ = SendMessageTimeoutW(
            target_hwnd,
            WM_NULL,
            WPARAM(0),
            LPARAM(0),
            SMTO_ABORTIFHUNG | SMTO_BLOCK,
            2000,
            Some(&mut _result),
        );

        // Small delay to let the DLL's DllMain thread finish work
        std::thread::sleep(std::time::Duration::from_millis(100));

        // 7. Cleanup: unhook, free DLL, close shared memory
        let _ = UnhookWindowsHookEx(hook);
        let _ = FreeLibrary(dll_handle);
        let _ = CloseHandle(mapping);

        Ok(format!("{}(pid={})", action_name, target_pid))
    }
}

/// Kills all processes matching the given name.
/// Available for the GUI's manual "Force Clean" feature with user confirmation.
pub fn kill_process_by_name(name: &str) {
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0).ok();
        if let Some(snapshot) = snapshot {
            let mut entry = PROCESSENTRY32 {
                dwSize: std::mem::size_of::<PROCESSENTRY32>() as u32,
                ..Default::default()
            };
            if Process32First(snapshot, &mut entry).is_ok() {
                loop {
                    let exe_file = &entry.szExeFile;
                    let len = exe_file
                        .iter()
                        .position(|&c| c == 0)
                        .unwrap_or(exe_file.len());
                    let process_name = String::from_utf8_lossy(
                        &exe_file[0..len]
                            .iter()
                            .map(|&c| c as u8)
                            .collect::<Vec<u8>>(),
                    )
                    .into_owned();

                    if process_name.eq_ignore_ascii_case(name) {
                        if let Ok(h) = OpenProcess(PROCESS_TERMINATE, false, entry.th32ProcessID) {
                            let _ = TerminateProcess(h, 1);
                            let _ = CloseHandle(h);
                        }
                    }
                    if Process32Next(snapshot, &mut entry).is_err() {
                        break;
                    }
                }
            }
            let _ = CloseHandle(snapshot);
        }
    }
}

/// Cleans up temporary DLL files from previous injection sessions.
/// Silently skips locked files — they will be cleaned on the next launch.
pub fn clean_temp_files() {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            if let Ok(entries) = std::fs::read_dir(dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                        if name.starts_with("winhider_payload_") && name.ends_with(".dll") {
                            let _ = std::fs::remove_file(path);
                        }
                    }
                }
            }
        }
    }
}
