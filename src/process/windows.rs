use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;
use std::path::{Path, PathBuf};

use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows_sys::Win32::System::Threading::{
    OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};

/// Another process's working directory isn't readable without reading its memory; the
/// shell integration reports it instead.
pub fn cwd(_pid: u32) -> Option<PathBuf> {
    None
}

pub fn executable(pid: u32) -> Option<PathBuf> {
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if h.is_null() {
            return None;
        }
        let mut buf = vec![0u16; 32768];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(h, 0, buf.as_mut_ptr(), &mut len);
        CloseHandle(h);
        (ok != 0).then(|| PathBuf::from(OsString::from_wide(&buf[..len as usize])))
    }
}

/// Every process as (pid, parent pid, executable name).
pub fn processes() -> Vec<(u32, u32, String)> {
    let mut out = vec![];
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snap == INVALID_HANDLE_VALUE {
            return out;
        }
        let mut e: PROCESSENTRY32W = std::mem::zeroed();
        e.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        let mut more = Process32FirstW(snap, &mut e) != 0;
        while more {
            let len = e.szExeFile.iter().position(|&c| c == 0).unwrap_or(e.szExeFile.len());
            out.push((e.th32ProcessID, e.th32ParentProcessID, String::from_utf16_lossy(&e.szExeFile[..len])));
            more = Process32NextW(snap, &mut e) != 0;
        }
        CloseHandle(snap);
    }
    out
}

/// The parent of `pid`, with its executable name.
pub fn parent(pid: u32) -> Option<(u32, String)> {
    let all = processes();
    let ppid = all.iter().find(|(p, ..)| *p == pid)?.1;
    all.into_iter().find(|(p, ..)| *p == ppid).map(|(p, _, name)| (p, name))
}

/// Whether the shell runs another program (a pager, an editor…) while a line is edited:
/// any child of it, other than the console hosts Windows starts.
pub fn foreign_program(shell: u32, _tty: &Path) -> bool {
    processes().iter().any(|(_, ppid, name)| {
        *ppid == shell && !name.eq_ignore_ascii_case("conhost.exe") && !name.eq_ignore_ascii_case("OpenConsole.exe")
    })
}
