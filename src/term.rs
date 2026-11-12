//! The terminal lig runs in: its size, raw mode, reading keys and writing output.
//! Unix uses the tty (termios, `SIGWINCH`); Windows uses the console in virtual terminal mode.

use std::time::Duration;

use anyhow::Result;

pub use imp::{RawMode, is_terminal, read, read_timeout, size, watch_resize, write};

#[cfg(unix)]
mod imp {
    use super::*;
    use nix::sys::termios::{SetArg, Termios, cfmakeraw, tcgetattr, tcsetattr};
    use std::io::Write;

    pub fn is_terminal() -> bool {
        unsafe { libc::isatty(0) == 1 && libc::isatty(1) == 1 }
    }

    /// Rows and columns.
    pub fn size() -> Option<(u16, u16)> {
        let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
        let ok = unsafe { libc::ioctl(0, libc::TIOCGWINSZ, &mut ws) } == 0
            || unsafe { libc::ioctl(1, libc::TIOCGWINSZ, &mut ws) } == 0;
        (ok && ws.ws_row > 0 && ws.ws_col > 0).then_some((ws.ws_row, ws.ws_col))
    }

    pub struct RawMode(Termios);

    impl RawMode {
        pub fn enable() -> Result<RawMode> {
            let orig = tcgetattr(std::io::stdin())?;
            let mut raw = orig.clone();
            cfmakeraw(&mut raw);
            tcsetattr(std::io::stdin(), SetArg::TCSANOW, &raw)?;
            Ok(RawMode(orig))
        }
    }

    impl Drop for RawMode {
        fn drop(&mut self) {
            let _ = tcsetattr(std::io::stdin(), SetArg::TCSANOW, &self.0);
        }
    }

    /// Blocks for input; `Ok(0)` once the terminal is gone.
    pub fn read(buf: &mut [u8]) -> std::io::Result<usize> {
        loop {
            let n = unsafe { libc::read(0, buf.as_mut_ptr().cast(), buf.len()) };
            if n >= 0 {
                return Ok(n as usize);
            }
            let e = std::io::Error::last_os_error();
            if e.kind() != std::io::ErrorKind::Interrupted {
                return Err(e);
            }
        }
    }

    /// Input that arrives within `timeout`, if any.
    pub fn read_timeout(buf: &mut [u8], timeout: Duration) -> Option<usize> {
        let mut fd = libc::pollfd { fd: 0, events: libc::POLLIN, revents: 0 };
        let ready = unsafe { libc::poll(&mut fd, 1, timeout.as_millis().max(1) as i32) };
        if ready <= 0 || fd.revents & libc::POLLIN == 0 {
            return None;
        }
        read(buf).ok().filter(|&n| n > 0)
    }

    /// Calls `f` after each resize until it returns false.
    pub fn watch_resize(f: impl Fn() -> bool + Send + 'static) -> Result<()> {
        let mut signals = signal_hook::iterator::Signals::new([libc::SIGWINCH])?;
        std::thread::spawn(move || {
            for _ in signals.forever() {
                if !f() {
                    break;
                }
            }
        });
        Ok(())
    }

    pub fn write(bytes: &[u8]) -> std::io::Result<()> {
        let mut out = std::io::stdout().lock();
        out.write_all(bytes)?;
        out.flush()
    }
}

#[cfg(windows)]
mod imp {
    use super::*;
    use std::sync::Mutex;
    use windows_sys::Win32::Foundation::{HANDLE, WAIT_OBJECT_0};
    use windows_sys::Win32::Storage::FileSystem::WriteFile;
    use windows_sys::Win32::System::Console::*;
    use windows_sys::Win32::System::Threading::WaitForSingleObject;

    fn stdin() -> HANDLE {
        unsafe { GetStdHandle(STD_INPUT_HANDLE) }
    }

    fn stdout() -> HANDLE {
        unsafe { GetStdHandle(STD_OUTPUT_HANDLE) }
    }

    fn mode(h: HANDLE) -> Option<CONSOLE_MODE> {
        let mut m = 0;
        (unsafe { GetConsoleMode(h, &mut m) } != 0).then_some(m)
    }

    pub fn is_terminal() -> bool {
        mode(stdin()).is_some() && mode(stdout()).is_some()
    }

    /// Rows and columns of the visible window.
    pub fn size() -> Option<(u16, u16)> {
        let mut info: CONSOLE_SCREEN_BUFFER_INFO = unsafe { std::mem::zeroed() };
        if unsafe { GetConsoleScreenBufferInfo(stdout(), &mut info) } == 0 {
            return None;
        }
        let w = info.srWindow;
        let (rows, cols) = (w.Bottom - w.Top + 1, w.Right - w.Left + 1);
        (rows > 0 && cols > 0).then_some((rows as u16, cols as u16))
    }

    /// Keys arrive as VT sequences in UTF-8, output is parsed as VT, and Ctrl+C is a key.
    pub struct RawMode {
        input: CONSOLE_MODE,
        output: CONSOLE_MODE,
        input_cp: u32,
        output_cp: u32,
    }

    impl RawMode {
        pub fn enable() -> Result<RawMode> {
            let input = mode(stdin()).ok_or_else(|| anyhow::anyhow!("stdin is not a console"))?;
            let output = mode(stdout()).ok_or_else(|| anyhow::anyhow!("stdout is not a console"))?;
            let saved = RawMode {
                input,
                output,
                input_cp: unsafe { GetConsoleCP() },
                output_cp: unsafe { GetConsoleOutputCP() },
            };
            let raw_in = (input & !(ENABLE_LINE_INPUT | ENABLE_ECHO_INPUT | ENABLE_PROCESSED_INPUT))
                | ENABLE_VIRTUAL_TERMINAL_INPUT;
            let raw_out =
                output | ENABLE_PROCESSED_OUTPUT | ENABLE_VIRTUAL_TERMINAL_PROCESSING | DISABLE_NEWLINE_AUTO_RETURN;
            unsafe {
                anyhow::ensure!(SetConsoleMode(stdin(), raw_in) != 0, "the console doesn't take VT input");
                anyhow::ensure!(SetConsoleMode(stdout(), raw_out) != 0, "the console doesn't take VT output");
                SetConsoleCP(65001);
                SetConsoleOutputCP(65001);
            }
            Ok(saved)
        }
    }

    impl Drop for RawMode {
        fn drop(&mut self) {
            unsafe {
                SetConsoleMode(stdin(), self.input);
                SetConsoleMode(stdout(), self.output);
                SetConsoleCP(self.input_cp);
                SetConsoleOutputCP(self.output_cp);
            }
        }
    }

    /// A high surrogate whose low half comes with the next read.
    static PENDING: Mutex<Option<u16>> = Mutex::new(None);

    /// Blocks for input (UTF-16 from the console, as UTF-8); `Ok(0)` once the console is gone.
    pub fn read(buf: &mut [u8]) -> std::io::Result<usize> {
        let mut wide = vec![0u16; (buf.len() / 4).max(1)];
        let mut n = 0u32;
        let ok =
            unsafe { ReadConsoleW(stdin(), wide.as_mut_ptr().cast(), wide.len() as u32, &mut n, std::ptr::null()) };
        if ok == 0 {
            return Err(std::io::Error::last_os_error());
        }
        let mut units: Vec<u16> = PENDING.lock().unwrap().take().into_iter().collect();
        units.extend_from_slice(&wide[..n as usize]);
        if units.last().is_some_and(|u| (0xD800..0xDC00).contains(u)) {
            *PENDING.lock().unwrap() = units.pop();
        }
        let text = String::from_utf16_lossy(&units);
        let len = text.len().min(buf.len());
        buf[..len].copy_from_slice(&text.as_bytes()[..len]);
        // An empty read with a surrogate held back isn't the end of input.
        if len == 0 && n > 0 {
            return read(buf);
        }
        Ok(len)
    }

    /// Whether a key is waiting; other console events (focus, …) are dropped so `read` won't block.
    fn key_waiting() -> bool {
        let mut records: [INPUT_RECORD; 64] = unsafe { std::mem::zeroed() };
        let mut n = 0u32;
        if unsafe { PeekConsoleInputW(stdin(), records.as_mut_ptr(), records.len() as u32, &mut n) } == 0 {
            return false;
        }
        let key = |r: &INPUT_RECORD| r.EventType == KEY_EVENT as u16 && unsafe { r.Event.KeyEvent.bKeyDown } != 0;
        if records[..n as usize].iter().any(key) {
            return true;
        }
        let mut read = 0u32;
        unsafe { ReadConsoleInputW(stdin(), records.as_mut_ptr(), n, &mut read) };
        false
    }

    /// Input that arrives within `timeout`, if any.
    pub fn read_timeout(buf: &mut [u8], timeout: Duration) -> Option<usize> {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            if left.is_zero()
                || unsafe { WaitForSingleObject(stdin(), left.as_millis().max(1) as u32) } != WAIT_OBJECT_0
            {
                return None;
            }
            if key_waiting() {
                return read(buf).ok().filter(|&n| n > 0);
            }
        }
    }

    /// Calls `f` after each resize until it returns false (the console has no resize signal).
    pub fn watch_resize(f: impl Fn() -> bool + Send + 'static) -> Result<()> {
        std::thread::spawn(move || {
            let mut last = size();
            loop {
                std::thread::sleep(Duration::from_millis(120));
                let now = size();
                if now != last {
                    last = now;
                    if !f() {
                        break;
                    }
                }
            }
        });
        Ok(())
    }

    /// Bytes as they are: the console's code page is UTF-8 while lig runs.
    pub fn write(mut bytes: &[u8]) -> std::io::Result<()> {
        while !bytes.is_empty() {
            let mut n = 0u32;
            let chunk = bytes.len().min(1 << 20) as u32;
            if unsafe { WriteFile(stdout(), bytes.as_ptr(), chunk, &mut n, std::ptr::null_mut()) } == 0 {
                return Err(std::io::Error::last_os_error());
            }
            bytes = &bytes[n as usize..];
        }
        Ok(())
    }
}
