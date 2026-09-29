//! Windows-specific interaction guard.
//!
//! When holding a macro/shortcut for push-to-talk voice recording on Windows,
//! key combinations like `Ctrl+Space` or mouse macro chording keep modifiers
//! (especially `VK_CONTROL`, `VK_MENU`, `VK_SHIFT`, or mouse buttons) logically
//! held down at the OS level.
//!
//! Under standard Windows behavior, holding `Ctrl` while rotating the mouse wheel
//! triggers Zoom In / Zoom Out in web browsers (Chrome, Edge, Firefox), document
//! viewers (Acrobat, Word), code editors (VS Code), and file explorers, completely
//! breaking the user's ability to scroll through their notes or references while dictating.
//!
//! This module installs a lightweight low-level mouse hook (`WH_MOUSE_LL`) that:
//! 1. Detects when voice recording is actively in progress.
//! 2. Intercepts `WM_MOUSEWHEEL` and `WM_MOUSEHWHEEL` events while macro modifiers are active.
//! 3. Strips the modifier flags (`MK_CONTROL`, `MK_SHIFT`, `MK_XBUTTON*`, etc.) from `wParam`.
//! 4. Delivers a clean, pure scroll message directly to the target window under the mouse cursor,
//!    allowing standard, smooth vertical scrolling without zooming or stuttering.
//! 5. Ensures the user can interact with their computer as normal during dictation.

use std::sync::atomic::{AtomicBool, Ordering};

pub static RECORDING_ACTIVE: AtomicBool = AtomicBool::new(false);

/// Notify the interaction guard of recording state changes.
pub fn set_recording_active(active: bool) {
    RECORDING_ACTIVE.store(active, Ordering::SeqCst);
    log::debug!("windows_interaction: recording active set to {active}");
}

/// Check if recording is currently active.
pub fn is_recording_active() -> bool {
    RECORDING_ACTIVE.load(Ordering::Relaxed)
}

#[cfg(target_os = "windows")]
mod platform {
    use super::*;
    use std::sync::atomic::AtomicU32;
    use std::thread;
    use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, WPARAM};
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        GetAsyncKeyState, VK_CONTROL, VK_MBUTTON, VK_MENU, VK_SHIFT, VK_XBUTTON1, VK_XBUTTON2,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        CallNextHookEx, DispatchMessageW, GetMessageW, PostMessageW,
        SetWindowsHookExW, TranslateMessage, UnhookWindowsHookEx, WindowFromPoint, MSG,
        MSLLHOOKSTRUCT, WH_MOUSE_LL, WM_MOUSEHWHEEL, WM_MOUSEWHEEL,
    };

    static HOOK_THREAD_ID: AtomicU32 = AtomicU32::new(0);

    /// Low-level mouse hook callback.
    unsafe extern "system" fn mouse_hook_proc(
        code: i32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        if code < 0 {
            return CallNextHookEx(None, code, wparam, lparam);
        }

        // Only intercept when voice recording is actively holding
        if is_recording_active() {
            let msg = wparam.0 as u32;
            if msg == WM_MOUSEWHEEL || msg == WM_MOUSEHWHEEL {
                // Check if macro modifiers or extra buttons are held down
                let ctrl_down = (GetAsyncKeyState(VK_CONTROL.0 as i32) as u16 & 0x8000) != 0;
                let alt_down = (GetAsyncKeyState(VK_MENU.0 as i32) as u16 & 0x8000) != 0;
                let shift_down = (GetAsyncKeyState(VK_SHIFT.0 as i32) as u16 & 0x8000) != 0;
                let xbtn1_down = (GetAsyncKeyState(VK_XBUTTON1.0 as i32) as u16 & 0x8000) != 0;
                let xbtn2_down = (GetAsyncKeyState(VK_XBUTTON2.0 as i32) as u16 & 0x8000) != 0;
                let mbtn_down = (GetAsyncKeyState(VK_MBUTTON.0 as i32) as u16 & 0x8000) != 0;

                // When holding a macro that includes Ctrl, Alt, Shift, or mouse buttons,
                // Windows attaches those modifier flags to WM_MOUSEWHEEL (e.g. MK_CONTROL),
                // converting scrolling into zoom or horizontal scroll.
                if ctrl_down || alt_down || shift_down || xbtn1_down || xbtn2_down || mbtn_down {
                    let mouse_struct = &*(lparam.0 as *const MSLLHOOKSTRUCT);
                    let pt = mouse_struct.pt;
                    let hwnd = WindowFromPoint(pt);

                    if !hwnd.is_invalid() && hwnd.0 != 0 {
                        let delta = (mouse_struct.mouseData >> 16) as i16;
                        // High-word: wheel rotation delta (+120 up, -120 down).
                        // Low-word (fwKeys): 0 -> Completely clean of MK_CONTROL / MK_SHIFT / MK_XBUTTON!
                        let clean_wparam = ((delta as u16 as u32) << 16) as usize;
                        let lparam_coords =
                            (((pt.y as u32) & 0xFFFF) << 16) | ((pt.x as u32) & 0xFFFF);

                        let _ = PostMessageW(
                            Some(hwnd),
                            msg,
                            WPARAM(clean_wparam),
                            LPARAM(lparam_coords as isize),
                        );
                        // Swallow the original event so the target application receives only
                        // the clean vertical scroll and never triggers zoom.
                        return LRESULT(1);
                    }
                }
            }
        }

        CallNextHookEx(None, code, wparam, lparam)
    }

    /// Initialize the background hook thread.
    pub fn init_interaction_guard() {
        thread::Builder::new()
            .name("windows-interaction-guard".into())
            .spawn(|| {
                unsafe {
                    let thread_id = windows::Win32::System::Threading::GetCurrentThreadId();
                    HOOK_THREAD_ID.store(thread_id, Ordering::SeqCst);

                    let hook = SetWindowsHookExW(
                        WH_MOUSE_LL,
                        Some(mouse_hook_proc),
                        HINSTANCE(std::ptr::null_mut()),
                        0,
                    );

                    let hook = match hook {
                        Ok(h) => {
                            log::info!("windows_interaction: WH_MOUSE_LL installed successfully");
                            h
                        }
                        Err(e) => {
                            log::error!("windows_interaction: Failed to install WH_MOUSE_LL: {e}");
                            return;
                        }
                    };

                    let mut msg = MSG::default();
                    while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                        let _ = TranslateMessage(&msg);
                        DispatchMessageW(&msg);
                    }

                    let _ = UnhookWindowsHookEx(hook);
                    log::info!("windows_interaction: WH_MOUSE_LL unhooked");
                }
            })
            .expect("Failed to spawn windows-interaction-guard thread");
    }
}

#[cfg(target_os = "windows")]
pub use platform::init_interaction_guard;

#[cfg(not(target_os = "windows"))]
pub fn init_interaction_guard() {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_recording_state_transitions() {
        set_recording_active(false);
        assert!(!is_recording_active());

        set_recording_active(true);
        assert!(is_recording_active());

        set_recording_active(false);
        assert!(!is_recording_active());
    }
}
