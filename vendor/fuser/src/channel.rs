use std::io;
use std::os::fd::AsFd;
use std::os::fd::BorrowedFd;
use std::sync::Arc;

use nix::errno::Errno;

use crate::dev_fuse::DevFuse;
use crate::passthrough::BackingId;

/// A raw communication channel to the FUSE kernel driver
#[derive(Debug, Clone)]
pub(crate) struct Channel(
    Arc<DevFuse>,
    #[cfg(all(target_os = "macos", feature = "fuse-t"))] Arc<std::sync::Mutex<()>>,
);

impl AsFd for Channel {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.0.as_fd()
    }
}

impl Channel {
    /// Create a new communication channel to the kernel driver by mounting the
    /// given path. The kernel driver will delegate filesystem operations of
    /// the given path to the channel.
    pub(crate) fn new(device: Arc<DevFuse>) -> Self {
        Self(
            device,
            #[cfg(all(target_os = "macos", feature = "fuse-t"))]
            Arc::new(std::sync::Mutex::new(())),
        )
    }

    /// Receives data up to the capacity of the given buffer (can block).
    fn receive(&self, buffer: &mut [u8]) -> nix::Result<usize> {
        #[cfg(all(target_os = "macos", feature = "fuse-t"))]
        {
            use std::io::Read;
            let mut file = &self.0.0;
            if buffer.len() < 40 {
                return Err(Errno::EINVAL);
            }
            file.read_exact(&mut buffer[..40]).map_err(|error| {
                if error.kind() == io::ErrorKind::UnexpectedEof {
                    Errno::ENODEV
                } else {
                    Errno::from_raw(error.raw_os_error().unwrap_or(libc::EIO))
                }
            })?;
            let len = u32::from_ne_bytes(buffer[..4].try_into().unwrap()) as usize;
            if len < 40 || len > buffer.len() {
                return Err(Errno::EINVAL);
            }
            file.read_exact(&mut buffer[40..len])
                .map_err(|error| Errno::from_raw(error.raw_os_error().unwrap_or(libc::EIO)))?;
            Ok(len)
        }
        #[cfg(not(all(target_os = "macos", feature = "fuse-t")))]
        {
            nix::unistd::read(&self.0, buffer)
        }
    }

    /// Receives data up to the capacity of the given buffer (can block),
    /// retrying on errors that are safe to retry (ENOENT, EINTR, EAGAIN).
    ///
    /// - ENOENT: Operation interrupted. According to FUSE, this is safe to retry.
    /// - EINTR: Interrupted system call, retry.
    /// - EAGAIN: Explicitly instructed to try again.
    pub(crate) fn receive_retrying(&self, buffer: &mut [u8]) -> nix::Result<usize> {
        loop {
            match self.receive(buffer) {
                Ok(size) => return Ok(size),
                Err(Errno::ENOENT | Errno::EINTR | Errno::EAGAIN) => continue,
                Err(err) => return Err(err),
            }
        }
    }

    /// Returns a sender object for this channel. The sender object can be
    /// used to send to the channel. Multiple sender objects can be used
    /// and they can safely be sent to other threads.
    pub(crate) fn sender(&self) -> ChannelSender {
        // Since write/writev syscalls are threadsafe, we can simply create
        // a sender by using the same file and use it in other threads.
        ChannelSender(
            self.0.clone(),
            #[cfg(all(target_os = "macos", feature = "fuse-t"))]
            self.1.clone(),
        )
    }

    /// Clone the FUSE device fd using FUSE_DEV_IOC_CLONE ioctl.
    ///
    /// This creates a new fd that can read FUSE requests independently,
    /// enabling true parallel request processing. The kernel distributes
    /// requests across all cloned fds.
    ///
    /// Requires Linux 4.5+. Returns an error on older kernels or non-Linux.
    #[cfg(target_os = "linux")]
    pub(crate) fn clone_fd(&self) -> io::Result<Channel> {
        use std::os::fd::AsRawFd;

        let new_dev = DevFuse::open()?;

        let mut source_fd = self.0.as_raw_fd() as u32;
        // SAFETY: fuse_dev_ioc_clone is a valid ioctl for /dev/fuse
        unsafe {
            crate::ll::ioctl::fuse_dev_ioc_clone(new_dev.as_raw_fd(), &mut source_fd)?;
        }

        Ok(Channel::new(Arc::new(new_dev)))
    }
}

#[derive(Clone, Debug)]
pub(crate) struct ChannelSender(
    Arc<DevFuse>,
    #[cfg(all(target_os = "macos", feature = "fuse-t"))] Arc<std::sync::Mutex<()>>,
);

impl ChannelSender {
    pub(crate) fn send(&self, bufs: &[io::IoSlice<'_>]) -> io::Result<()> {
        #[cfg(all(target_os = "macos", feature = "fuse-t"))]
        {
            use std::io::Write;
            let _guard = self
                .1
                .lock()
                .map_err(|_| io::Error::other("FUSE-T send lock poisoned"))?;
            let mut file = &self.0.0;
            for buf in bufs {
                file.write_all(buf)?;
            }
            Ok(())
        }
        #[cfg(not(all(target_os = "macos", feature = "fuse-t")))]
        {
            let rc = nix::sys::uio::writev(&self.0, bufs)?;
            debug_assert_eq!(bufs.iter().map(|b| b.len()).sum::<usize>(), rc);
            Ok(())
        }
    }

    pub(crate) fn open_backing(&self, fd: BorrowedFd<'_>) -> std::io::Result<BackingId> {
        BackingId::create(&self.0, fd)
    }

    pub(crate) unsafe fn wrap_backing(&self, id: u32) -> BackingId {
        unsafe { BackingId::wrap_raw(&self.0, id) }
    }
}

#[cfg(all(test, target_os = "macos", feature = "fuse-t"))]
mod fuse_t_tests {
    use super::*;
    use std::fs::File;
    use std::io::{Read, Write};
    use std::os::fd::OwnedFd;
    use std::os::unix::net::UnixStream;

    fn pair() -> (Channel, UnixStream) {
        let (device, peer) = UnixStream::pair().unwrap();
        (
            Channel::new(Arc::new(DevFuse(File::from(OwnedFd::from(device))))),
            peer,
        )
    }

    #[test]
    fn stream_reads_preserve_message_boundaries_and_eof() {
        let (channel, mut peer) = pair();
        let mut first = vec![1u8; 43];
        first[..4].copy_from_slice(&43u32.to_ne_bytes());
        let mut second = vec![2u8; 47];
        second[..4].copy_from_slice(&47u32.to_ne_bytes());
        peer.write_all(&[first.clone(), second.clone()].concat())
            .unwrap();
        drop(peer);
        let mut buffer = vec![0u8; 1024];
        assert_eq!(channel.receive_retrying(&mut buffer).unwrap(), first.len());
        assert_eq!(&buffer[..first.len()], first);
        assert_eq!(channel.receive_retrying(&mut buffer).unwrap(), second.len());
        assert_eq!(&buffer[..second.len()], second);
        assert_eq!(channel.receive_retrying(&mut buffer), Err(Errno::ENODEV));
    }

    #[test]
    fn concurrent_large_stream_replies_do_not_interleave() {
        let (channel, mut peer) = pair();
        let mut threads = Vec::new();
        for byte in [1u8, 2u8] {
            let sender = channel.sender();
            threads.push(std::thread::spawn(move || {
                let data = vec![byte; 1024 * 1024];
                sender
                    .send(&[io::IoSlice::new(&data), io::IoSlice::new(&data)])
                    .unwrap();
            }));
        }
        let mut received = vec![0u8; 4 * 1024 * 1024];
        peer.read_exact(&mut received).unwrap();
        for thread in threads {
            thread.join().unwrap();
        }
        let (first, second) = received.split_at(2 * 1024 * 1024);
        assert!(first.iter().all(|byte| *byte == first[0]));
        assert!(second.iter().all(|byte| *byte == second[0]));
        assert_ne!(first[0], second[0]);
    }
}
