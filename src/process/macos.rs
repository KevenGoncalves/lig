//! Native macOS process inspection. No process metadata is read from /proc.
use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;

fn parse_args(data: &[u8]) -> Option<Vec<OsString>> {
    let argc = i32::from_ne_bytes(data.get(..4)?.try_into().ok()?);
    if argc <= 0 || argc as usize > data.len() {
        return None;
    }
    let mut rest = data.get(4..)?;
    let path_end = rest.iter().position(|&b| b == 0)? + 1;
    // XNU exec_extract_strings aligns the executable-path area to the target
    // pointer size. Apple Silicon and Rosetta processes are both 64-bit.
    // KERN_PROCARGS2 removes the 16-byte "executable_path=" prefix, which
    // preserves that alignment. Do NOT skip all NULs: argv[0] may be empty.
    // See apple-oss-distributions/xnu bsd/kern/kern_exec.c: exec_extract_strings.
    let argv_start = path_end.checked_add(7)? & !7;
    if rest.get(path_end..argv_start)?.iter().any(|&b| b != 0) {
        return None;
    }
    rest = rest.get(argv_start..)?;
    let mut args = Vec::new();
    for _ in 0..argc {
        let end = rest.iter().position(|&b| b == 0)?;
        args.push(OsString::from_vec(rest[..end].to_vec()));
        rest = &rest[end + 1..];
    }
    Some(args)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn arguments_preserve_empty_spaces_and_non_utf8_without_environment() {
        let mut bytes = 4i32.to_ne_bytes().to_vec();
        bytes.extend_from_slice(b"/bin/test\0");
        bytes.resize(4 + 16, 0);
        bytes.extend_from_slice(b"test\0two words\0\0\xff\0SECRET=hidden\0");
        assert_eq!(
            parse_args(&bytes),
            Some(vec!["test".into(), "two words".into(), "".into(), OsString::from_vec(vec![255])])
        );
    }
    #[test]
    fn empty_argv_zero_never_consumes_environment() {
        let mut bytes = 2i32.to_ne_bytes().to_vec();
        bytes.extend_from_slice(b"/bin/sleep\0");
        bytes.resize(4 + 16, 0); // 64-bit exec pathname padding
        bytes.extend_from_slice(b"\0".as_slice());
        bytes.extend_from_slice(b"10\0LIG_TEST_SENTINEL=not-a-secret\0");
        assert_eq!(parse_args(&bytes), Some(vec!["".into(), "10".into()]));
    }
    #[test]
    fn rejects_truncated_or_invalid_arguments() {
        let mut truncated = 2i32.to_ne_bytes().to_vec();
        truncated.extend_from_slice(b"/bin/test\0");
        truncated.resize(4 + 16, 0);
        truncated.extend_from_slice(b"test\0unterminated");
        for data in [
            truncated,
            vec![],
            (-1i32).to_ne_bytes().to_vec(),
            [2i32.to_ne_bytes().as_slice(), b"/bin/test\0test\0unterminated"].concat(),
        ] {
            assert!(parse_args(&data).is_none());
        }
    }
}

use std::mem::size_of;
use std::path::{Path, PathBuf};

fn path(chars: impl IntoIterator<Item = libc::c_char>) -> Option<PathBuf> {
    let bytes: Vec<u8> = chars.into_iter().take_while(|&c| c != 0).map(|c| c as u8).collect();
    (!bytes.is_empty()).then(|| PathBuf::from(OsString::from_vec(bytes)))
}

fn bsd(pid: u32) -> Option<libc::proc_bsdinfo> {
    let pid = i32::try_from(pid).ok()?;
    // SAFETY: this C struct contains only integers/arrays; the buffer has its exact size.
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let n = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            0,
            (&mut info as *mut libc::proc_bsdinfo).cast(),
            size_of::<libc::proc_bsdinfo>() as i32,
        )
    };
    (n == size_of::<libc::proc_bsdinfo>() as i32).then_some(info)
}

pub fn cwd(pid: u32) -> Option<PathBuf> {
    let pid = i32::try_from(pid).ok()?;
    // SAFETY: integer-only C struct, passed with its exact allocated size.
    let mut info: libc::proc_vnodepathinfo = unsafe { std::mem::zeroed() };
    let n = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDVNODEPATHINFO,
            0,
            (&mut info as *mut libc::proc_vnodepathinfo).cast(),
            size_of::<libc::proc_vnodepathinfo>() as i32,
        )
    };
    (n == size_of::<libc::proc_vnodepathinfo>() as i32)
        .then(|| path(info.pvi_cdir.vip_path.into_iter().flatten()))
        .flatten()
}

pub fn executable(pid: u32) -> Option<PathBuf> {
    let pid = i32::try_from(pid).ok()?;
    let mut buf = [0; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    // SAFETY: writable byte buffer and matching length.
    let n = unsafe { libc::proc_pidpath(pid, buf.as_mut_ptr().cast(), buf.len() as u32) };
    (n > 0).then(|| path(buf)).flatten()
}

pub fn children(pid: u32) -> Vec<u32> {
    let Ok(pid) = i32::try_from(pid) else { return vec![] };
    // SAFETY: null/zero requests the required buffer size.
    let size = unsafe { libc::proc_listchildpids(pid, std::ptr::null_mut(), 0) };
    if size <= 0 {
        return vec![];
    }
    let mut pids = vec![0i32; (size as usize + 32).min(65536)];
    // SAFETY: writable pid buffer with size in bytes, as required by libproc.
    let n = unsafe { libc::proc_listchildpids(pid, pids.as_mut_ptr().cast(), (pids.len() * size_of::<i32>()) as i32) };
    pids.into_iter().take(n.max(0) as usize).filter_map(|p| u32::try_from(p).ok().filter(|&p| p > 0)).collect()
}

pub fn foreground(pid: u32) -> Option<u32> {
    bsd(pid).map(|i| i.e_tpgid).filter(|&p| p > 0 && p <= i32::MAX as u32)
}

pub fn argv(pid: u32) -> Option<Vec<OsString>> {
    let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, i32::try_from(pid).ok()?];
    let mut buf = vec![0u8; 1024 * 1024];
    let mut size = buf.len();
    // SAFETY: mib and output buffer have the provided lengths; no sysctl write requested.
    let result =
        unsafe { libc::sysctl(mib.as_mut_ptr(), 3, buf.as_mut_ptr().cast(), &mut size, std::ptr::null_mut(), 0) };
    if result != 0 || size > buf.len() {
        return None;
    }
    parse_args(&buf[..size])
}

// ABI from Apple's sys/proc_info.h; libc provides vnode_info_path but not these wrappers.
#[repr(C)]
struct FileInfo {
    openflags: u32,
    status: u32,
    offset: libc::off_t,
    kind: i32,
    guardflags: u32,
}
#[repr(C)]
struct VnodeFdInfo {
    file: FileInfo,
    vnode: libc::vnode_info_path,
}

fn has_tty(pid: u32, tty: &Path) -> bool {
    let Some(info) = bsd(pid) else { return false };
    let mut fds: Vec<libc::proc_fdinfo> = (0..(info.pbi_nfiles as usize + 32).min(65536))
        .map(|_| libc::proc_fdinfo { proc_fd: -1, proc_fdtype: 0 })
        .collect();
    // SAFETY: initialized buffer of proc_fdinfo, size in bytes.
    let n = unsafe {
        libc::proc_pidinfo(
            pid as i32,
            libc::PROC_PIDLISTFDS,
            0,
            fds.as_mut_ptr().cast(),
            (fds.len() * size_of::<libc::proc_fdinfo>()) as i32,
        )
    };
    fds.iter().take(n.max(0) as usize / size_of::<libc::proc_fdinfo>()).any(|fd| {
        // PROX_FDTYPE_VNODE = 1; ignore stderr-only prompt helpers as on Linux.
        if fd.proc_fd == 2 || fd.proc_fdtype != 1 {
            return false;
        }
        // SAFETY: C integer-only structure and matching size for PROC_PIDFDVNODEPATHINFO = 2.
        let mut vnode: VnodeFdInfo = unsafe { std::mem::zeroed() };
        let n = unsafe {
            libc::proc_pidfdinfo(
                pid as i32,
                fd.proc_fd,
                2,
                (&mut vnode as *mut VnodeFdInfo).cast(),
                size_of::<VnodeFdInfo>() as i32,
            )
        };
        n == size_of::<VnodeFdInfo>() as i32
            && path(vnode.vnode.vip_path.into_iter().flatten()).is_some_and(|p| p == tty || p == Path::new("/dev/tty"))
    })
}

pub fn foreign_program(shell: u32, tty: &Path) -> bool {
    let Some(shell_info) = bsd(shell) else { return false };
    let mut todo = children(shell);
    let mut seen = std::collections::HashSet::new();
    while let Some(pid) = todo.pop() {
        if !seen.insert(pid) || seen.len() > 4096 {
            continue;
        }
        todo.extend(children(pid));
        let Some(info) = bsd(pid) else { continue };
        let comm = path(info.pbi_comm);
        if info.pbi_pgid != shell_info.pbi_pgid
            || comm.as_deref().is_some_and(|p| matches!(p.to_str(), Some("zsh" | "bash" | "sh" | "dash")))
        {
            continue;
        }
        if has_tty(pid, tty) {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod native_tests {
    use super::*;
    #[test]
    fn native_empty_argv_zero_stays_separate_from_environment() {
        use std::os::unix::process::CommandExt;
        let mut child = std::process::Command::new("/bin/sleep")
            .arg0("")
            .arg("10")
            .env_clear()
            .env("LIG_TEST_SENTINEL", "not-a-secret")
            .spawn()
            .unwrap();
        let args = argv(child.id());
        child.kill().unwrap();
        child.wait().unwrap();
        assert_eq!(args, Some(vec!["".into(), "10".into()]));
    }
    #[test]
    fn detects_tty_descriptors() {
        let pair = portable_pty::native_pty_system().openpty(portable_pty::PtySize::default()).unwrap();
        let tty = pair.master.tty_name().unwrap();
        let mut cmd = portable_pty::CommandBuilder::new("/bin/sleep");
        cmd.arg("10");
        let mut child = pair.slave.spawn_command(cmd).unwrap();
        let present = has_tty(child.process_id().unwrap(), &tty);
        child.kill().unwrap();
        child.wait().unwrap();
        assert!(present);
    }
    #[test]
    fn inspect_self_and_child() {
        let pid = std::process::id();
        assert_eq!(cwd(pid).unwrap().canonicalize().unwrap(), std::env::current_dir().unwrap().canonicalize().unwrap());
        assert_eq!(executable(pid).unwrap(), std::env::current_exe().unwrap());
        assert!(!argv(pid).unwrap().is_empty());
        let mut child = std::process::Command::new("/bin/sleep").arg("10").spawn().unwrap();
        let found = children(pid).contains(&child.id());
        child.kill().unwrap();
        child.wait().unwrap();
        assert!(found);
        assert!(argv(u32::MAX).is_none());
        assert!(cwd(u32::MAX).is_none());
    }
}

pub fn identity(pid: u32) -> Option<(u64, u64)> {
    bsd(pid).map(|i| (i.pbi_start_tvsec, i.pbi_start_tvusec))
}
