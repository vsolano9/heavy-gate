//! The OS owns slot lifetime; metadata never decides whether a slot is free.

#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;

#[cfg(unix)]
pub(crate) use unix::*;
#[cfg(windows)]
pub(crate) use windows::*;

#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
compile_error!("heavy-gate supports Windows, Linux, and macOS");
