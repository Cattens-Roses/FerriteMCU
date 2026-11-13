//! Small OS-integration utilities (no gpui): shell-command install and scratch paths.
pub(crate) mod clipboard;
#[cfg(target_os = "linux")]
pub(crate) mod desktop;
#[cfg(target_os = "linux")]
pub(crate) mod input_method;
#[cfg(unix)]
pub(crate) mod instance;
#[cfg(target_os = "linux")]
pub(crate) mod keyboard_layout;
#[cfg(all(feature = "terminal", target_os = "macos"))]
pub(crate) mod locale;
pub(crate) mod scratch;
pub(crate) mod shellcmd;
#[cfg(all(unix, not(target_os = "macos")))]
pub(crate) mod terminal_app;
#[cfg(target_os = "linux")]
pub(crate) mod window_icon;
#[cfg(target_os = "windows")]
pub(crate) mod windows;
