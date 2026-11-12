//! The pseudo terminal the shell runs in: portable-pty on Unix, ConPTY on Windows.

use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::PathBuf;

use anyhow::Result;

pub struct Command {
    pub program: PathBuf,
    pub args: Vec<OsString>,
    /// Set on top of lig's own environment.
    pub env: Vec<(OsString, OsString)>,
    pub cwd: Option<PathBuf>,
}

impl Command {
    pub fn new(program: impl Into<PathBuf>) -> Command {
        Command { program: program.into(), args: vec![], env: vec![], cwd: None }
    }

    pub fn arg(&mut self, a: impl Into<OsString>) -> &mut Command {
        self.args.push(a.into());
        self
    }

    pub fn env(&mut self, k: impl Into<OsString>, v: impl Into<OsString>) -> &mut Command {
        self.env.push((k.into(), v.into()));
        self
    }
}

pub type Spawned = (Pty, Box<dyn Read + Send>, Box<dyn Write + Send>);

pub use imp::Pty;

#[cfg(unix)]
mod imp {
    use super::*;
    use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};

    pub struct Pty {
        master: Box<dyn MasterPty + Send>,
        child: Box<dyn Child + Send + Sync>,
    }

    impl Pty {
        pub fn spawn(cmd: &Command, rows: u16, cols: u16) -> Result<Spawned> {
            let pair = native_pty_system().openpty(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })?;
            let mut b = CommandBuilder::new(&cmd.program);
            b.args(&cmd.args);
            for (k, v) in &cmd.env {
                b.env(k, v);
            }
            // The shell integration loads only on this tty (see shell/post.*).
            if let Some(tty) = pair.master.tty_name() {
                b.env("LIG_TTY", tty);
            }
            if let Some(cwd) = &cmd.cwd {
                b.cwd(cwd);
            }
            let child = pair.slave.spawn_command(b)?;
            drop(pair.slave);
            let reader = pair.master.try_clone_reader()?;
            let writer = pair.master.take_writer()?;
            Ok((Pty { master: pair.master, child }, reader, writer))
        }

        pub fn pid(&self) -> Option<u32> {
            self.child.process_id()
        }

        pub fn tty_name(&self) -> Option<PathBuf> {
            self.master.tty_name()
        }

        pub fn resize(&self, rows: u16, cols: u16) {
            let _ = self.master.resize(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 });
        }

        /// The output reaches end of file when the shell exits: nothing to watch.
        pub fn on_exit(&self, _f: impl FnOnce() + Send + 'static) {}

        pub fn wait(&mut self) -> i32 {
            self.child.wait().map(|st| st.exit_code() as i32).unwrap_or(0)
        }
    }
}

#[cfg(windows)]
mod imp {
    use super::*;
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::io::FromRawHandle;
    use std::ptr::{null, null_mut};
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Console::{
        COORD, ClosePseudoConsole, CreatePseudoConsole, HPCON, ResizePseudoConsole,
    };
    use windows_sys::Win32::System::Pipes::CreatePipe;
    use windows_sys::Win32::System::Threading::*;

    /// A shell in a pseudo console. ConPTY is created without `PSEUDOCONSOLE_INHERIT_CURSOR`:
    /// with it, ConPTY asks for the cursor position and can hang even when answered.
    pub struct Pty {
        hpc: HPCON,
        process: HANDLE,
        pid: u32,
    }

    fn wide(s: &std::ffi::OsStr) -> Vec<u16> {
        s.encode_wide().chain(Some(0)).collect()
    }

    /// One argument as the C runtime (and most programs) split the command line.
    pub(crate) fn quote(arg: &std::ffi::OsStr) -> OsString {
        let s = arg.to_string_lossy();
        if !s.is_empty() && !s.contains([' ', '\t', '"']) {
            return arg.to_os_string();
        }
        let mut out = String::from('"');
        // Backslashes are literal unless they precede a quote, where they double.
        let mut slashes = 0;
        for c in s.chars() {
            if c == '\\' {
                slashes += 1;
                continue;
            }
            out.push_str(&"\\".repeat(if c == '"' { slashes * 2 + 1 } else { slashes }));
            slashes = 0;
            out.push(c);
        }
        out.push_str(&"\\".repeat(slashes * 2));
        out.push('"');
        out.into()
    }

    /// lig's environment with `extra` on top, sorted as Windows expects, as a UTF-16 block.
    fn env_block(extra: &[(OsString, OsString)]) -> Vec<u16> {
        let mut vars: Vec<(OsString, OsString)> = std::env::vars_os().collect();
        for (k, v) in extra {
            let key = k.to_string_lossy().to_uppercase();
            vars.retain(|(old, _)| old.to_string_lossy().to_uppercase() != key);
            vars.push((k.clone(), v.clone()));
        }
        vars.sort_by_key(|(k, _)| k.to_string_lossy().to_uppercase());
        let mut block = vec![];
        for (k, v) in vars {
            block.extend(k.encode_wide());
            block.push(u16::from(b'='));
            block.extend(v.encode_wide());
            block.push(0);
        }
        block.push(0);
        block
    }

    fn last_error(what: &str) -> anyhow::Error {
        anyhow::anyhow!("{what}: {}", std::io::Error::last_os_error())
    }

    impl Pty {
        pub fn spawn(cmd: &Command, rows: u16, cols: u16) -> Result<Spawned> {
            unsafe {
                let (mut in_read, mut in_write, mut out_read, mut out_write) =
                    (null_mut(), null_mut(), null_mut(), null_mut());
                if CreatePipe(&mut in_read, &mut in_write, null(), 0) == 0 {
                    return Err(last_error("CreatePipe"));
                }
                if CreatePipe(&mut out_read, &mut out_write, null(), 0) == 0 {
                    CloseHandle(in_read);
                    CloseHandle(in_write);
                    return Err(last_error("CreatePipe"));
                }
                let mut hpc: HPCON = 0;
                let size = COORD { X: cols as i16, Y: rows as i16 };
                let hr = CreatePseudoConsole(size, in_read, out_write, 0, &mut hpc);
                // The pseudo console holds its own copies.
                CloseHandle(in_read);
                CloseHandle(out_write);
                if hr != 0 {
                    CloseHandle(in_write);
                    CloseHandle(out_read);
                    anyhow::bail!("CreatePseudoConsole failed: HRESULT {hr:#x}");
                }
                let reader = std::fs::File::from_raw_handle(out_read);
                let writer = std::fs::File::from_raw_handle(in_write);
                // Owns the pseudo console from here, so an error below closes it.
                let mut pty = Pty { hpc, process: null_mut(), pid: 0 };

                let mut size = 0usize;
                InitializeProcThreadAttributeList(null_mut(), 1, 0, &mut size);
                let mut attrs = vec![0u8; size];
                let list: LPPROC_THREAD_ATTRIBUTE_LIST = attrs.as_mut_ptr().cast();
                if InitializeProcThreadAttributeList(list, 1, 0, &mut size) == 0 {
                    return Err(last_error("InitializeProcThreadAttributeList"));
                }
                let set = UpdateProcThreadAttribute(
                    list,
                    0,
                    PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE as usize,
                    hpc as *const std::ffi::c_void,
                    std::mem::size_of::<HPCON>(),
                    null_mut(),
                    null(),
                );
                if set == 0 {
                    DeleteProcThreadAttributeList(list);
                    return Err(last_error("UpdateProcThreadAttribute"));
                }
                let mut si: STARTUPINFOEXW = std::mem::zeroed();
                si.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
                // Without this the shell could inherit lig's own (redirected) standard handles.
                si.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
                si.StartupInfo.hStdInput = INVALID_HANDLE_VALUE;
                si.StartupInfo.hStdOutput = INVALID_HANDLE_VALUE;
                si.StartupInfo.hStdError = INVALID_HANDLE_VALUE;
                si.lpAttributeList = list;

                let mut line = quote(cmd.program.as_os_str());
                for a in &cmd.args {
                    line.push(" ");
                    line.push(quote(a));
                }
                let mut line = wide(&line);
                let env = env_block(&cmd.env);
                let cwd = cmd.cwd.as_ref().map(|c| wide(c.as_os_str()));
                let mut pi: PROCESS_INFORMATION = std::mem::zeroed();
                let ok = CreateProcessW(
                    null(),
                    line.as_mut_ptr(),
                    null(),
                    null(),
                    0,
                    EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT,
                    env.as_ptr().cast(),
                    cwd.as_ref().map_or(null(), |c| c.as_ptr()),
                    (&si as *const STARTUPINFOEXW).cast(),
                    &mut pi,
                );
                DeleteProcThreadAttributeList(list);
                if ok == 0 {
                    return Err(last_error(&format!("starting {}", cmd.program.display())));
                }
                CloseHandle(pi.hThread);
                pty.process = pi.hProcess;
                pty.pid = pi.dwProcessId;
                Ok((pty, Box::new(reader), Box::new(writer)))
            }
        }

        pub fn pid(&self) -> Option<u32> {
            Some(self.pid)
        }

        pub fn tty_name(&self) -> Option<PathBuf> {
            None
        }

        pub fn resize(&self, rows: u16, cols: u16) {
            unsafe { ResizePseudoConsole(self.hpc, COORD { X: cols as i16, Y: rows as i16 }) };
        }

        /// The pseudo console's output stays open after the shell exits, so watch the process.
        pub fn on_exit(&self, f: impl FnOnce() + Send + 'static) {
            let h = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, self.pid) };
            if h.is_null() {
                return;
            }
            let h = h as usize;
            std::thread::spawn(move || {
                unsafe {
                    WaitForSingleObject(h as HANDLE, INFINITE);
                    CloseHandle(h as HANDLE);
                }
                f();
            });
        }

        pub fn wait(&mut self) -> i32 {
            let mut code = 0u32;
            unsafe {
                WaitForSingleObject(self.process, INFINITE);
                GetExitCodeProcess(self.process, &mut code);
            }
            code as i32
        }
    }

    impl Drop for Pty {
        /// Before Windows 11 24H2 this blocks until the output is read: keep the reader running.
        fn drop(&mut self) {
            unsafe {
                ClosePseudoConsole(self.hpc);
                if !self.process.is_null() {
                    CloseHandle(self.process);
                }
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn quotes_like_the_c_runtime() {
            let q = |s: &str| quote(s.as_ref()).into_string().unwrap();
            assert_eq!(q("pwsh.exe"), "pwsh.exe");
            assert_eq!(q(r"C:\Program Files\PowerShell\7\pwsh.exe"), r#""C:\Program Files\PowerShell\7\pwsh.exe""#);
            assert_eq!(q(""), r#""""#);
            assert_eq!(q(r#"say "hi""#), r#""say \"hi\"""#);
            assert_eq!(q(r"dir\ x\"), r#""dir\ x\\""#);
        }
    }
}
