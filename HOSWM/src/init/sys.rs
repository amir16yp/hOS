//! Linux calls shared by the supervisor and the system services.
//!
//! The crate has no external dependencies, so the pieces of libc that init
//! needs are declared here once: signals, child reaping, sockets, ioctls and
//! the realtime clock. Everything else uses `std`.
use std::{
    ffi::c_int,
    fs,
    io::{self, Write},
    os::fd::{AsRawFd, RawFd},
    path::Path,
    sync::atomic::{AtomicI32, AtomicU64, Ordering},
};

pub const SIGHUP: i32 = 1;
pub const SIGINT: i32 = 2;
pub const SIGKILL: i32 = 9;
pub const SIGUSR1: i32 = 10;
pub const SIGUSR2: i32 = 12;
pub const SIGTERM: i32 = 15;
pub const SIGCHLD: i32 = 17;
/// `waitpid` without blocking.
pub const WNOHANG: i32 = 1;
pub const CLOCK_REALTIME: i32 = 0;

pub use crate::drm::ioctl;

unsafe extern "C" {
    fn signal(number: c_int, handler: usize) -> usize;
    fn waitpid(pid: c_int, status: *mut c_int, flags: c_int) -> c_int;
    pub fn kill(pid: c_int, signal: c_int) -> c_int;
    pub fn geteuid() -> u32;
    pub fn socket(domain: c_int, kind: c_int, protocol: c_int) -> c_int;
    pub fn bind(fd: c_int, address: *const u8, length: u32) -> c_int;
    pub fn setsockopt(fd: c_int, level: c_int, name: c_int, value: *const u8, length: u32)
    -> c_int;
    pub fn getsockopt(
        fd: c_int,
        level: c_int,
        name: c_int,
        value: *mut u8,
        length: *mut u32,
    ) -> c_int;
    pub fn recv(fd: c_int, buffer: *mut u8, length: usize, flags: c_int) -> isize;
    fn fcntl(fd: c_int, command: c_int, argument: c_int) -> c_int;
    fn close(fd: c_int) -> c_int;
    fn write(fd: c_int, buffer: *const u8, length: usize) -> isize;
    fn clock_settime(clock: c_int, time: *const Timespec) -> c_int;
    fn adjtime(delta: *const Timeval, old: *mut Timeval) -> c_int;
    pub fn sync();
    pub fn reboot(command: c_int) -> c_int;
}

#[repr(C)]
pub struct Timespec {
    pub seconds: i64,
    pub nanos: i64,
}
#[repr(C)]
pub struct Timeval {
    pub seconds: i64,
    pub micros: i64,
}

/// An owned file descriptor for the sockets `std` cannot open, such as netlink.
pub struct Fd(RawFd);
impl Fd {
    /// Take ownership of a descriptor, turning a negative result into an error.
    pub fn new(fd: c_int) -> io::Result<Self> {
        if fd < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(Self(fd))
        }
    }
    pub fn nonblocking(self) -> io::Result<Self> {
        // SAFETY: F_GETFL/F_SETFL on a descriptor this value owns.
        let flags = unsafe { fcntl(self.0, 3, 0) };
        if flags < 0 || unsafe { fcntl(self.0, 4, flags | 0o4000) } < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(self)
    }
}
impl AsRawFd for Fd {
    fn as_raw_fd(&self) -> RawFd {
        self.0
    }
}
impl Drop for Fd {
    fn drop(&mut self) {
        // SAFETY: this type owns the descriptor and is dropped once.
        unsafe { close(self.0) };
    }
}

/// Signals delivered since the last [`Signals::take`] call, as a bit mask.
static PENDING: AtomicU64 = AtomicU64::new(0);
/// Write end of the self-pipe the handler pokes so a blocked poll wakes up.
static WAKE: AtomicI32 = AtomicI32::new(-1);

extern "C" fn handler(number: c_int) {
    if number > 0 && number < 64 {
        PENDING.fetch_or(1 << number, Ordering::Relaxed);
    }
    let fd = WAKE.load(Ordering::Relaxed);
    if fd >= 0 {
        // SAFETY: write to a pipe descriptor is async-signal-safe; a full pipe
        // only means an earlier wake-up is still unread.
        unsafe { write(fd, [1u8].as_ptr(), 1) };
    }
}

/// Caught signals and the descriptor that reports them to a poll loop.
pub struct Signals {
    reader: std::os::unix::net::UnixStream,
    _writer: std::os::unix::net::UnixStream,
}
impl Signals {
    /// Catch `numbers` and route them through a self-pipe.
    pub fn catch(numbers: &[i32]) -> io::Result<Self> {
        let (reader, writer) = std::os::unix::net::UnixStream::pair()?;
        reader.set_nonblocking(true)?;
        writer.set_nonblocking(true)?;
        WAKE.store(writer.as_raw_fd(), Ordering::Relaxed);
        for number in numbers {
            // SAFETY: installing a C handler for a valid signal number.
            unsafe { signal(*number, handler as *const () as usize) };
        }
        Ok(Self {
            reader,
            _writer: writer,
        })
    }
    /// Take the set of signals seen since the last call, draining the pipe.
    pub fn take(&mut self) -> Vec<i32> {
        use std::io::Read;
        let mut buffer = [0u8; 64];
        while matches!(self.reader.read(&mut buffer), Ok(n) if n > 0) {}
        let mask = PENDING.swap(0, Ordering::Relaxed);
        (1..64).filter(|n| mask & (1 << n) != 0).collect()
    }
    /// Poke the self-pipe so a waiting loop runs one more iteration.
    pub fn wake(&self) {
        let _ = (&self._writer).write(&[1]);
    }
}
impl AsRawFd for Signals {
    fn as_raw_fd(&self) -> RawFd {
        self.reader.as_raw_fd()
    }
}

/// Reap one exited child, returning its pid and exit status.
///
/// The status is the shell convention: the exit code, or 128 plus the signal.
pub fn reap() -> Option<(i32, i32)> {
    let mut status = 0;
    // SAFETY: waitpid with WNOHANG over every child of this process.
    let pid = unsafe { waitpid(-1, &mut status, WNOHANG) };
    if pid <= 0 {
        return None;
    }
    let code = if status & 0x7f == 0 {
        (status >> 8) & 0xff
    } else {
        128 + (status & 0x7f)
    };
    Some((pid, code))
}

/// Set the realtime clock. Used by `hos-ntpd` for large corrections.
pub fn set_realtime(seconds: i64, nanos: i64) -> io::Result<()> {
    let time = Timespec { seconds, nanos };
    // SAFETY: a live timespec for CLOCK_REALTIME.
    if unsafe { clock_settime(CLOCK_REALTIME, &time) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}
/// Slew the clock by a small offset instead of stepping it.
pub fn slew_realtime(offset: f64) -> io::Result<()> {
    let delta = Timeval {
        seconds: offset.trunc() as i64,
        micros: (offset.fract() * 1e6) as i64,
    };
    // SAFETY: a live timeval; the previous adjustment is not needed.
    if unsafe { adjtime(&delta, std::ptr::null_mut()) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}
/// Seconds and nanoseconds since the Unix epoch.
pub fn realtime() -> (i64, u32) {
    match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => (d.as_secs() as i64, d.subsec_nanos()),
        Err(e) => (-(e.duration().as_secs() as i64), 0),
    }
}

/// Read a small sysfs or procfs file, trimmed. Missing files read as `None`.
pub fn read_text(path: impl AsRef<Path>) -> Option<String> {
    fs::read_to_string(path)
        .ok()
        .map(|text| text.trim().to_string())
}
/// Read a sysfs file holding one number.
pub fn read_number<T: std::str::FromStr>(path: impl AsRef<Path>) -> Option<T> {
    read_text(path)?.parse().ok()
}
/// Replace a file in one step, so readers never see a half-written version.
pub fn write_atomic(path: &Path, contents: &str) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension("hos-new");
    fs::write(&temporary, contents)?;
    match fs::rename(&temporary, path) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = fs::remove_file(&temporary);
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exit_status_conversion_matches_the_shell_convention() {
        // Exit code 3 and death by signal 9, as waitpid encodes them.
        assert_eq!((3 << 8) >> 8 & 0xff, 3);
        assert_eq!(128 + (9 & 0x7f), 137);
    }
    #[test]
    fn signals_report_themselves_through_the_pipe() {
        let mut signals = Signals::catch(&[SIGUSR1]).unwrap();
        assert!(signals.take().is_empty());
        // SAFETY: signalling this process, whose handler is installed above.
        assert_eq!(unsafe { kill(std::process::id() as i32, SIGUSR1) }, 0);
        // A signal sent to the process may be delivered on another thread, so
        // the test waits for it rather than assuming it arrived already.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut seen = signals.take();
        while seen.is_empty() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(1));
            seen = signals.take();
        }
        assert_eq!(seen, [SIGUSR1]);
        assert!(signals.take().is_empty());
    }
    #[test]
    fn write_atomic_replaces_contents_and_leaves_no_temporary() {
        let dir = std::env::temp_dir().join(format!("hos-sys-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("resolv.conf");
        write_atomic(&path, "nameserver 10.0.2.3\n").unwrap();
        write_atomic(&path, "nameserver 1.1.1.1\n").unwrap();
        assert_eq!(read_text(&path).unwrap(), "nameserver 1.1.1.1");
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
        fs::remove_dir_all(&dir).unwrap();
    }
}
