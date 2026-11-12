//! Platform process queries; they return no data when a process disappears.
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
use linux as imp;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
use macos as imp;
#[cfg(windows)]
mod windows;
#[cfg(windows)]
use windows as imp;

#[cfg(windows)]
pub use imp::parent;
#[cfg(target_os = "macos")]
pub use imp::{argv, foreground, identity};
pub use imp::{cwd, executable, foreign_program};
