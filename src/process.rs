//! Platform-specific process queries, returning no data when a process disappears.
//! Both platforms provide the functions re-exported below, so a missing one fails `cargo check`
//! on either OS. Only the macOS mirror needs `foreground`, `argv` and `identity`
//! (the Linux mirror reads `/proc` itself).
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
use linux as imp;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
use macos as imp;

#[cfg(target_os = "macos")]
pub use imp::{argv, foreground, identity};
pub use imp::{cwd, executable, foreign_program};
