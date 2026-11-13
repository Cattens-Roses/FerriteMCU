//! Small Windows-specific integrations kept behind the platform boundary.

use std::path::Path;
use std::process::Command;

/// Current local UTC offset in minutes east of UTC.
pub(crate) fn timezone_offset_minutes() -> Option<i32> {
    use windows_sys::Win32::System::Time::{GetTimeZoneInformation, TIME_ZONE_INFORMATION};

    let mut info = TIME_ZONE_INFORMATION::default();
    // SAFETY: `info` is a valid writable Windows time-zone structure.
    let state = unsafe { GetTimeZoneInformation(&mut info) };
    if state == u32::MAX {
        return None;
    }
    // TIME_ZONE_ID_DAYLIGHT is 2. Windows Bias is minutes west of UTC.
    let seasonal_bias = if state == 2 {
        info.DaylightBias
    } else {
        info.StandardBias
    };
    Some(info.Bias.saturating_add(seasonal_bias).saturating_neg())
}

/// Hand a title-strip drag to Windows after GPUI has finished dispatching the mouse event.
pub(crate) fn start_window_move(window: &gpui::Window) {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use windows_sys::Win32::Foundation::POINT;
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::ReleaseCapture;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetCursorPos, PostMessageW, HTCAPTION, SC_MOVE, WM_SYSCOMMAND,
    };

    let Ok(handle) = HasWindowHandle::window_handle(window) else {
        return;
    };
    let RawWindowHandle::Win32(handle) = handle.as_raw() else {
        return;
    };
    let mut position = POINT { x: 0, y: 0 };
    // SAFETY: the handle belongs to this live window and position points to valid storage.
    // Posting (rather than sending) avoids re-entering GPUI during mouse event dispatch.
    unsafe {
        if GetCursorPos(&mut position) == 0 {
            return;
        }
        ReleaseCapture();
        let coords = (position.x as u16 as u32) | ((position.y as u16 as u32) << 16);
        PostMessageW(
            handle.hwnd.get() as _,
            WM_SYSCOMMAND,
            (SC_MOVE | HTCAPTION) as _,
            coords as isize,
        );
    }
}

/// Open a Windows Terminal tab in `dir`, falling back to a new Command Prompt window when
/// Windows Terminal is unavailable. Process creation is the only synchronous work here; the
/// caller runs this function on the background executor.
pub(crate) fn open_terminal(dir: &Path) -> std::io::Result<()> {
    if Command::new("wt.exe")
        .args(["-d"])
        .arg(dir)
        .current_dir(dir)
        .spawn()
        .is_ok()
    {
        return Ok(());
    }

    use std::os::windows::process::CommandExt;
    const CREATE_NEW_CONSOLE: u32 = 0x0000_0010;

    Command::new("cmd.exe")
        .arg("/K")
        .current_dir(dir)
        .creation_flags(CREATE_NEW_CONSOLE)
        .spawn()
        .map(|_| ())
}

/// Open a URL or file path with its registered Windows application.
pub(crate) fn open_with_system(target: &str) -> std::io::Result<()> {
    Command::new("explorer.exe").arg(target).spawn().map(|_| ())
}
