//! Small poll reactor: input, PTYs and IPC wake the session without periodic sleeps.
use std::{io, os::fd::RawFd, time::Duration};
#[repr(C)]
struct PollFd {
    fd: i32,
    events: i16,
    revents: i16,
}
#[repr(C)]
struct Timespec {
    seconds: i64,
    nanos: i64,
}
unsafe extern "C" {
    fn ppoll(
        fds: *mut PollFd,
        count: usize,
        timeout: *const Timespec,
        sigmask: *const std::ffi::c_void,
    ) -> i32;
}
#[derive(Default)]
pub struct Reactor {
    fds: Vec<PollFd>,
}
impl Reactor {
    pub fn clear(&mut self) {
        self.fds.clear();
    }
    pub fn watch(&mut self, fd: RawFd, readable: bool, writable: bool) {
        self.fds.push(PollFd {
            fd,
            events: i16::from(readable) | (i16::from(writable) << 2),
            revents: 0,
        });
    }
    pub fn wait(&mut self, timeout: Duration) -> io::Result<()> {
        let timeout = Timespec {
            seconds: timeout.as_secs().min(i64::MAX as u64) as i64,
            nanos: timeout.subsec_nanos() as i64,
        };
        // SAFETY: live Linux pollfd/timespec structures; null leaves the signal mask unchanged.
        if unsafe {
            ppoll(
                self.fds.as_mut_ptr(),
                self.fds.len(),
                &timeout,
                std::ptr::null(),
            )
        } < 0
        {
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(error);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::Write,
        os::{fd::AsRawFd, unix::net::UnixStream},
    };
    #[test]
    fn ready_input_and_pending_output_wake_the_reactor() {
        let (input, mut output) = UnixStream::pair().unwrap();
        output.write_all(b"key").unwrap();
        let mut reactor = Reactor::default();
        reactor.watch(input.as_raw_fd(), true, false);
        reactor.wait(Duration::from_secs(1)).unwrap();
        assert_ne!(reactor.fds[0].revents & 1, 0);
        reactor.clear();
        reactor.watch(output.as_raw_fd(), false, true);
        reactor.wait(Duration::from_secs(1)).unwrap();
        assert_ne!(reactor.fds[0].revents & 4, 0);
    }
}
