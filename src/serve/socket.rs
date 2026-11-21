//! Session directory checks, the control socket, peer credentials and signals.
//!
//! All libc FFI used by `serve` lives here behind safe wrappers. Thread rules:
//! `umask` and `signal` are process-global, so [`ControlSocket::bind`] and
//! [`Signals::install`] must run while the process has one thread, which is how
//! `serve` starts: its other threads are spawned only after the control
//! handshake. The handler only stores to an atomic.

use std::ffi::{c_int, c_uint, c_void};
use std::fs;
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use super::Error;

/// File name of the control socket inside the session directory.
pub(super) const SOCKET_NAME: &str = "control.sock";
/// `sockaddr_un.sun_path` holds 108 bytes including the terminating NUL.
const SUN_PATH_MAX: usize = 107;
/// Longest uninterruptible wait: bounds signal latency and deadline overshoot.
pub(super) const SLICE: Duration = Duration::from_millis(50);
/// Prefix of an abstract control socket name; 32 lowercase hex digits follow.
const ABSTRACT_PREFIX: &str = "carlo-";
/// Permission bits a private socket must have: owner read/write only.
const SOCKET_MODE: u32 = 0o600;

unsafe extern "C" {
    // mode_t umask(mode_t): mode_t is `unsigned int` on Linux x86_64/aarch64
    // (glibc, musl) and on 64-bit Bionic.
    fn umask(mask: c_uint) -> c_uint;
    // uid_t geteuid(void): uid_t is a 32-bit unsigned integer on Linux and Bionic.
    fn geteuid() -> u32;
    // int getsockopt(int, int, int, void *restrict, socklen_t *restrict);
    // socklen_t is a 32-bit unsigned integer on Linux and Bionic.
    fn getsockopt(
        socket: c_int,
        level: c_int,
        name: c_int,
        value: *mut c_void,
        length: *mut u32,
    ) -> c_int;
    // sighandler_t signal(int, sighandler_t): same binding as src/accessibility.
    fn signal(number: c_int, handler: usize) -> usize;
}

/// Effective UID of this process.
pub(super) fn effective_uid() -> u32 {
    // SAFETY: geteuid takes no arguments, cannot fail and touches no memory.
    unsafe { geteuid() }
}

/// Refuse anything but an existing real directory that only `owner` can enter.
/// Metadata is read without following a final symlink. Nothing is modified.
pub(super) fn validate_directory(directory: &Path, owner: u32) -> Result<(), Error> {
    if !directory.is_absolute() {
        return Err(Error::Directory("path is not absolute"));
    }
    let metadata = fs::symlink_metadata(directory).map_err(|error| match error.kind() {
        io::ErrorKind::NotFound => Error::Directory("does not exist"),
        _ => Error::Directory("cannot be inspected"),
    })?;
    if !metadata.file_type().is_dir() {
        return Err(Error::Directory("is not a real directory"));
    }
    if metadata.uid() != owner {
        return Err(Error::Directory("is not owned by the effective user"));
    }
    if metadata.mode() & 0o077 != 0 {
        return Err(Error::Directory(
            "is accessible to group or others; mode must be 0700",
        ));
    }
    Ok(())
}

/// Restores the process umask when dropped.
struct Umask(c_uint);

impl Umask {
    fn set(mask: c_uint) -> Self {
        // SAFETY: umask only swaps a per-process integer and cannot fail. The
        // caller is the only thread (module rule), so no other file creation
        // can observe the temporary value.
        Self(unsafe { umask(mask) })
    }
}

impl Drop for Umask {
    fn drop(&mut self) {
        // SAFETY: restores exactly the mask returned by the paired call above.
        unsafe { umask(self.0) };
    }
}

/// Where the control socket lives.
enum Location {
    /// A 0600 socket file; `identity` is the (device, inode) created here.
    Filesystem { path: String, identity: (u64, u64) },
    /// A name in the Linux abstract namespace: no file, no permission bits.
    Abstract { name: String },
}

/// The listening socket of a session: it admits the control client and then, for
/// a bounded window, the video channel. Dropping it closes the listener; a
/// filesystem path is also removed, but only while it is still the socket file
/// created here. An abstract name has nothing to unlink: the kernel frees it on
/// close.
pub(super) struct ControlSocket {
    listener: UnixListener,
    location: Location,
}

impl ControlSocket {
    /// Prefer `<directory>/control.sock`; use an abstract name only when the
    /// platform denies creating the socket file.
    pub(super) fn bind(directory: &Path) -> Result<Self, Error> {
        with_fallback(Self::bind_filesystem(directory), Self::bind_abstract)
    }

    /// Create `<directory>/control.sock` with mode 0600 from the moment it exists.
    /// An existing path of any type is refused and left untouched.
    fn bind_filesystem(directory: &Path) -> Result<Self, Error> {
        let path = directory.join(SOCKET_NAME);
        if path.as_os_str().as_bytes().len() > SUN_PATH_MAX {
            return Err(Error::PathTooLong);
        }
        let text = path
            .to_str()
            .ok_or(Error::Directory("is not UTF-8"))?
            .to_owned();
        let listener = {
            // Everything but owner read/write is masked while the kernel creates
            // the socket inode, so there is no window before a later chmod.
            let _restore = Umask::set(0o177);
            // bind(2) never replaces an existing path, symlinks included.
            UnixListener::bind(&path)
        }
        .map_err(|error| match error.kind() {
            io::ErrorKind::AddrInUse => Error::SocketExists,
            _ => Error::Bind(error),
        })?;
        match fs::symlink_metadata(&path) {
            Ok(metadata)
                if metadata.file_type().is_socket() && metadata.mode() & 0o777 == SOCKET_MODE =>
            {
                Ok(Self {
                    listener,
                    location: Location::Filesystem {
                        path: text,
                        identity: (metadata.dev(), metadata.ino()),
                    },
                })
            }
            // No identity to prove ownership later: unlink by path here,
            // immediately after our own successful bind.
            _ => {
                let _ = fs::remove_file(&path);
                Err(Error::SocketMode)
            }
        }
    }

    /// Bind `carlo-<32 hex>` in the abstract namespace. The name comes from 16
    /// fresh kernel-random bytes, unrelated to the session identifier. Nothing
    /// restricts who may connect: the peer-credential check, the single accept
    /// and the session identifier are the whole access control.
    #[cfg(any(target_os = "linux", target_os = "android"))]
    pub(super) fn bind_abstract() -> Result<Self, Error> {
        #[cfg(target_os = "android")]
        use std::os::android::net::SocketAddrExt;
        #[cfg(target_os = "linux")]
        use std::os::linux::net::SocketAddrExt;
        use std::os::unix::net::SocketAddr;

        let name = format!("{ABSTRACT_PREFIX}{}", super::random_hex()?);
        let address = SocketAddr::from_abstract_name(name.as_bytes()).map_err(Error::Bind)?;
        // An abstract bind never replaces a live name: a collision is EADDRINUSE.
        let listener = UnixListener::bind_addr(&address).map_err(|error| match error.kind() {
            io::ErrorKind::AddrInUse => Error::SocketExists,
            _ => Error::Bind(error),
        })?;
        Ok(Self {
            listener,
            location: Location::Abstract { name },
        })
    }

    /// No abstract namespace off Linux: the original denial stands.
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    pub(super) fn bind_abstract() -> Result<Self, Error> {
        Err(Error::Bind(io::ErrorKind::PermissionDenied.into()))
    }

    /// `"filesystem"` or `"abstract"`, as printed on the ready line.
    pub(super) fn transport(&self) -> &'static str {
        match self.location {
            Location::Filesystem { .. } => "filesystem",
            Location::Abstract { .. } => "abstract",
        }
    }

    /// Socket file path, or the abstract name without its leading NUL.
    pub(super) fn address(&self) -> &str {
        match &self.location {
            Location::Filesystem { path, .. } => path,
            Location::Abstract { name } => name,
        }
    }

    /// Wait for one connection. The listener stays open: its owner decides how
    /// long, and dropping it is what stops anybody else from connecting.
    pub(super) fn accept(&self, timeout: Duration, stop: &AtomicBool) -> Result<UnixStream, Error> {
        accept_bounded(&self.listener, timeout, stop)
    }
}

impl Drop for ControlSocket {
    fn drop(&mut self) {
        if let Location::Filesystem { path, identity } = &self.location {
            let ours = fs::symlink_metadata(path)
                .is_ok_and(|metadata| (metadata.dev(), metadata.ino()) == *identity);
            if ours {
                let _ = fs::remove_file(path);
            }
        }
    }
}

/// The transport decision: the filesystem result stands unless creating the
/// socket file was denied (`EACCES`/`EPERM`, e.g. an SELinux domain without
/// `sock_file` create). Every other failure, and every success, is final.
fn with_fallback<T>(
    filesystem: Result<T, Error>,
    fallback: impl FnOnce() -> Result<T, Error>,
) -> Result<T, Error> {
    match filesystem {
        Err(Error::Bind(error)) if error.kind() == io::ErrorKind::PermissionDenied => fallback(),
        other => other,
    }
}

/// Accept one connection within `timeout`, checking `stop` every [`SLICE`].
pub(super) fn accept_bounded(
    listener: &UnixListener,
    timeout: Duration,
    stop: &AtomicBool,
) -> Result<UnixStream, Error> {
    listener.set_nonblocking(true).map_err(Error::Accept)?;
    let deadline = Instant::now() + timeout;
    loop {
        if stop.load(Ordering::Relaxed) {
            return Err(Error::Interrupted);
        }
        match listener.accept() {
            Ok((stream, _)) => return Ok(stream),
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock
                        | io::ErrorKind::Interrupted
                        | io::ErrorKind::ConnectionAborted
                ) => {}
            Err(error) => return Err(Error::Accept(error)),
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(Error::AcceptTimeout);
        }
        std::thread::sleep(remaining.min(SLICE));
    }
}

/// Kernel-reported identity of the process that connected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Credentials {
    pub uid: u32,
}

/// `struct ucred` from `<sys/socket.h>` (Linux) / `<linux/socket.h>` (Bionic):
/// `pid_t pid; uid_t uid; gid_t gid;`, three 32-bit fields, 12 bytes, no padding.
#[repr(C)]
struct UCred {
    pid: i32,
    uid: u32,
    gid: u32,
}

/// Read `SO_PEERCRED` from a connected Unix stream.
#[cfg(all(
    any(target_os = "linux", target_os = "android"),
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
pub(super) fn peer_credentials(stream: &UnixStream) -> Result<Credentials, Error> {
    // asm-generic/socket.h values, used by x86_64 and aarch64 on Linux and
    // Android. Other architectures (e.g. powerpc, mips) differ and are excluded.
    const SOL_SOCKET: c_int = 1;
    const SO_PEERCRED: c_int = 17;
    let mut credentials = UCred {
        pid: 0,
        uid: u32::MAX,
        gid: u32::MAX,
    };
    let expected = size_of::<UCred>() as u32;
    let mut length = expected;
    // SAFETY: the descriptor is open for the lifetime of `stream`; `credentials`
    // is a writable, properly aligned `struct ucred` and `length` holds its exact
    // size, so the kernel writes at most that many bytes. Neither pointer
    // outlives this synchronous call.
    let result = unsafe {
        getsockopt(
            stream.as_raw_fd(),
            SOL_SOCKET,
            SO_PEERCRED,
            (&raw mut credentials).cast(),
            &raw mut length,
        )
    };
    if result != 0 {
        return Err(Error::PeerCredentials(io::Error::last_os_error()));
    }
    // Linux reports pid 0 with uid/gid -1 when no peer credentials exist.
    if length != expected || credentials.pid <= 0 || credentials.uid == u32::MAX {
        return Err(Error::PeerCredentials(io::ErrorKind::InvalidData.into()));
    }
    let _ = credentials.gid;
    Ok(Credentials {
        uid: credentials.uid,
    })
}

/// Unverified architecture: fail closed instead of guessing socket constants.
#[cfg(not(all(
    any(target_os = "linux", target_os = "android"),
    any(target_arch = "x86_64", target_arch = "aarch64")
)))]
pub(super) fn peer_credentials(_stream: &UnixStream) -> Result<Credentials, Error> {
    Err(Error::PeerCredentials(io::ErrorKind::Unsupported.into()))
}

/// The peer must be this user. On Android only, root is also accepted: `adbd`
/// (which opens the forwarded connection) runs as root on userdebug/eng builds.
pub(super) const fn peer_allowed(peer_uid: u32, own_uid: u32, android: bool) -> bool {
    peer_uid == own_uid || (android && peer_uid == 0)
}

/// Why a [`BoundedStream`] stopped an operation on its own initiative.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Ended {
    /// A termination signal was observed.
    Interrupted,
    /// The deadline set by the caller passed.
    Deadline,
}

/// A blocking stream whose reads and writes end at a caller-set deadline or when
/// `stop` is raised, checked at least every [`SLICE`].
pub(super) struct BoundedStream<'a> {
    stream: UnixStream,
    stop: &'a AtomicBool,
    deadline: Instant,
    ended: Option<Ended>,
}

impl<'a> BoundedStream<'a> {
    pub(super) fn new(stream: UnixStream, stop: &'a AtomicBool) -> io::Result<Self> {
        stream.set_nonblocking(false)?;
        stream.set_read_timeout(Some(SLICE))?;
        stream.set_write_timeout(Some(SLICE))?;
        Ok(Self {
            stream,
            stop,
            deadline: Instant::now(),
            ended: None,
        })
    }

    /// Absolute deadline for the operations that follow.
    pub(super) fn set_deadline(&mut self, deadline: Instant) {
        self.deadline = deadline;
    }

    /// The connection itself, with the slice timeouts still set on the socket.
    pub(super) fn into_inner(self) -> UnixStream {
        self.stream
    }

    /// The reason this wrapper failed the last operation, if it did.
    pub(super) fn ended(&self) -> Option<Ended> {
        self.ended
    }

    fn check(&mut self) -> io::Result<()> {
        let (ended, kind) = if self.stop.load(Ordering::Relaxed) {
            // Not `Interrupted`: callers retry that kind, this must end the read.
            (Ended::Interrupted, io::ErrorKind::Other)
        } else if Instant::now() >= self.deadline {
            (Ended::Deadline, io::ErrorKind::TimedOut)
        } else {
            return Ok(());
        };
        self.ended = Some(ended);
        Err(kind.into())
    }

    fn bounded(
        &mut self,
        mut operation: impl FnMut(&mut UnixStream) -> io::Result<usize>,
    ) -> io::Result<usize> {
        loop {
            self.check()?;
            match operation(&mut self.stream) {
                // A slice elapsed (reported as either kind) or a signal arrived.
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock
                            | io::ErrorKind::TimedOut
                            | io::ErrorKind::Interrupted
                    ) => {}
                other => return other,
            }
        }
    }
}

impl Read for BoundedStream<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.bounded(|stream| stream.read(buffer))
    }
}

impl Write for BoundedStream<'_> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.bounded(|stream| stream.write(buffer))
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Raised by SIGHUP, SIGINT or SIGTERM while a [`Signals`] guard is alive.
pub(super) static STOP: AtomicBool = AtomicBool::new(false);

unsafe extern "C" fn stop_handler(_: c_int) {
    STOP.store(true, Ordering::Relaxed);
}

/// Routes termination signals to [`STOP`]; restores the previous dispositions on drop.
pub(super) struct Signals([(c_int, usize); 3]);

impl Signals {
    /// SIGHUP (adbd closing the shell), SIGINT and SIGTERM: identical numbers on
    /// every Linux and Android architecture.
    const NUMBERS: [c_int; 3] = [1, 2, 15];
    /// `SIG_ERR`, i.e. `(sighandler_t)-1`.
    const FAILED: usize = usize::MAX;

    pub(super) fn install() -> Result<Self, Error> {
        STOP.store(false, Ordering::Relaxed);
        // The guard exists first, so an early return restores what was installed.
        let mut guard = Self([(0, Self::FAILED); 3]);
        for (slot, number) in Self::NUMBERS.into_iter().enumerate() {
            // SAFETY: the handler is an `extern "C" fn(c_int)` that only performs
            // a lock-free atomic store, which is async-signal-safe. `signal` with
            // a valid number has no other preconditions.
            let old = unsafe { signal(number, stop_handler as *const () as usize) };
            if old == Self::FAILED {
                return Err(Error::Signals);
            }
            guard.0[slot] = (number, old);
        }
        Ok(guard)
    }
}

impl Drop for Signals {
    fn drop(&mut self) {
        for (number, old) in self.0 {
            if old != Self::FAILED {
                // SAFETY: restores exactly the disposition `signal` returned for
                // this number; no pointer to Rust data is involved.
                unsafe { signal(number, old) };
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;
    use std::sync::atomic::AtomicU32;

    use super::*;

    /// A private scratch directory removed on drop. Never changes the umask.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> Self {
            static NEXT: AtomicU32 = AtomicU32::new(0);
            let path = std::env::temp_dir().join(format!(
                "carlo-unit-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
            Self(path)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn directory_must_be_real_private_and_owned() {
        let scratch = Scratch::new();
        let own = effective_uid();
        assert!(validate_directory(&scratch.0, own).is_ok());
        let refused = |path: &Path, owner| match validate_directory(path, owner) {
            Err(Error::Directory(reason)) => reason,
            other => panic!("expected a directory refusal, got {other:?}"),
        };
        assert_eq!(
            refused(&scratch.0, own.wrapping_add(1)),
            "is not owned by the effective user"
        );
        assert_eq!(refused(&scratch.0.join("missing"), own), "does not exist");
        assert_eq!(refused(Path::new("relative"), own), "path is not absolute");
        let file = scratch.0.join("file");
        fs::write(&file, b"").unwrap();
        assert_eq!(refused(&file, own), "is not a real directory");
        let link = scratch.0.join("link");
        std::os::unix::fs::symlink(&scratch.0, &link).unwrap();
        assert_eq!(refused(&link, own), "is not a real directory");
        for mode in [0o750, 0o705, 0o701, 0o710, 0o777, 0o1777] {
            fs::set_permissions(&scratch.0, fs::Permissions::from_mode(mode)).unwrap();
            assert!(
                refused(&scratch.0, own).contains("mode must be 0700"),
                "{mode:o}"
            );
        }
        for mode in [0o700, 0o500] {
            fs::set_permissions(&scratch.0, fs::Permissions::from_mode(mode)).unwrap();
            assert!(validate_directory(&scratch.0, own).is_ok());
        }
        fs::set_permissions(&scratch.0, fs::Permissions::from_mode(0o700)).unwrap();
    }

    #[test]
    fn only_a_denied_socket_file_selects_the_fallback() {
        let denied = || Error::Bind(io::ErrorKind::PermissionDenied.into());
        assert_eq!(
            with_fallback(Err(denied()), || Ok("abstract")).unwrap(),
            "abstract"
        );
        // EACCES and EPERM both map to PermissionDenied.
        for errno in [13, 1] {
            let error = Error::Bind(io::Error::from_raw_os_error(errno));
            assert_eq!(
                with_fallback(Err(error), || Ok("abstract")).unwrap(),
                "abstract"
            );
        }
        // A failing fallback is the reported failure.
        assert!(matches!(
            with_fallback::<&str>(Err(denied()), || Err(Error::SocketExists)),
            Err(Error::SocketExists)
        ));
        let never = || -> Result<&str, Error> { panic!("fallback must not run") };
        assert_eq!(
            with_fallback(Ok("filesystem"), never).unwrap(),
            "filesystem"
        );
        for error in [
            Error::Bind(io::ErrorKind::AddrNotAvailable.into()),
            Error::Bind(io::ErrorKind::NotFound.into()),
            Error::Bind(io::ErrorKind::ReadOnlyFilesystem.into()),
            Error::Bind(io::ErrorKind::StorageFull.into()),
            Error::SocketExists,
            Error::SocketMode,
            Error::PathTooLong,
            Error::Directory("does not exist"),
            Error::PeerCredentials(io::ErrorKind::PermissionDenied.into()),
        ] {
            let kept = format!("{error:?}");
            let result = with_fallback(Err(error), never).unwrap_err();
            assert_eq!(format!("{result:?}"), kept);
        }
    }

    #[cfg(target_os = "linux")]
    fn connect_abstract(name: &str) -> io::Result<UnixStream> {
        use std::os::linux::net::SocketAddrExt;
        let address = std::os::unix::net::SocketAddr::from_abstract_name(name.as_bytes())?;
        UnixStream::connect_addr(&address)
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn abstract_socket_has_a_fresh_name_no_file_and_is_released_on_drop() {
        use std::os::linux::net::SocketAddrExt;
        let first = ControlSocket::bind_abstract().unwrap();
        let second = ControlSocket::bind_abstract().unwrap();
        assert_eq!(
            (first.transport(), second.transport()),
            ("abstract", "abstract")
        );
        for socket in [&first, &second] {
            let hex = socket.address().strip_prefix("carlo-").unwrap();
            assert_eq!(hex.len(), 32);
            assert!(
                hex.bytes()
                    .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
            );
            assert!(!Path::new(socket.address()).exists());
        }
        assert_ne!(first.address(), second.address());
        let name = first.address().to_owned();

        // The live name cannot be bound again by anyone.
        let address = std::os::unix::net::SocketAddr::from_abstract_name(name.as_bytes()).unwrap();
        let again = UnixListener::bind_addr(&address).unwrap_err();
        assert_eq!(again.kind(), io::ErrorKind::AddrInUse);

        let stop = AtomicBool::new(false);
        let mut client = connect_abstract(&name).unwrap();
        let mut accepted = first.accept(Duration::from_secs(5), &stop).unwrap();
        assert_eq!(peer_credentials(&accepted).unwrap().uid, effective_uid());
        // Still listening until its owner drops it: a second connection (the
        // video channel) can queue. Once dropped nobody else can attach, and
        // the accepted connection survives.
        let _queued = connect_abstract(&name).unwrap();
        drop(first);
        let refused = connect_abstract(&name).unwrap_err();
        assert_eq!(refused.kind(), io::ErrorKind::ConnectionRefused);
        client.write_all(b"ok").unwrap();
        let mut buffer = [0_u8; 2];
        accepted.read_exact(&mut buffer).unwrap();
        assert_eq!(&buffer, b"ok");

        // A listener that times out or is dropped also releases its name.
        let name = second.address().to_owned();
        assert!(matches!(
            second.accept(Duration::from_millis(60), &stop),
            Err(Error::AcceptTimeout)
        ));
        drop(second);
        assert!(connect_abstract(&name).is_err());
    }

    #[test]
    fn only_the_same_user_or_android_root_may_connect() {
        assert!(peer_allowed(2000, 2000, true) && peer_allowed(1000, 1000, false));
        assert!(peer_allowed(0, 2000, true) && peer_allowed(0, 0, false));
        assert!(!peer_allowed(0, 1000, false) && !peer_allowed(10123, 2000, true));
        assert!(!peer_allowed(2000, 0, true) && !peer_allowed(1001, 1000, false));
    }

    #[test]
    fn peer_credentials_report_this_process_for_a_local_pair() {
        let (left, _right) = UnixStream::pair().unwrap();
        assert_eq!(peer_credentials(&left).unwrap().uid, effective_uid());
    }

    #[test]
    fn accept_is_bounded_by_its_deadline_and_by_the_stop_flag() {
        let scratch = Scratch::new();
        let listener = UnixListener::bind(scratch.0.join("s")).unwrap();
        let stop = AtomicBool::new(false);
        let started = Instant::now();
        assert!(matches!(
            accept_bounded(&listener, Duration::from_millis(120), &stop),
            Err(Error::AcceptTimeout)
        ));
        assert!((Duration::from_millis(120)..Duration::from_secs(5)).contains(&started.elapsed()));
        let _client = UnixStream::connect(scratch.0.join("s")).unwrap();
        assert!(accept_bounded(&listener, Duration::from_secs(5), &stop).is_ok());
        stop.store(true, Ordering::Relaxed);
        let started = Instant::now();
        assert!(matches!(
            accept_bounded(&listener, Duration::from_secs(30), &stop),
            Err(Error::Interrupted)
        ));
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn stream_waits_end_at_the_deadline_or_stop_and_say_which() {
        let (left, mut right) = UnixStream::pair().unwrap();
        let stop = AtomicBool::new(false);
        let mut stream = BoundedStream::new(left, &stop).unwrap();
        let mut buffer = [0_u8; 4];
        stream.set_deadline(Instant::now() + Duration::from_millis(120));
        let started = Instant::now();
        let error = stream.read(&mut buffer).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert_eq!(stream.ended(), Some(Ended::Deadline));
        assert!((Duration::from_millis(120)..Duration::from_secs(5)).contains(&started.elapsed()));

        stream.set_deadline(Instant::now() + Duration::from_secs(30));
        right.write_all(b"ab").unwrap();
        assert_eq!(stream.read(&mut buffer).unwrap(), 2);
        stream.write_all(b"cd").unwrap();
        right.read_exact(&mut buffer[..2]).unwrap();
        assert_eq!(&buffer[..2], b"cd");

        // A peer that never reads: the write deadline still ends the wait.
        stream.set_deadline(Instant::now() + Duration::from_millis(200));
        let error = stream.write_all(&vec![0; 16 * 1024 * 1024]).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);

        stream.set_deadline(Instant::now() + Duration::from_secs(30));
        stop.store(true, Ordering::Relaxed);
        let started = Instant::now();
        let error = stream.read(&mut buffer).unwrap_err();
        assert_ne!(error.kind(), io::ErrorKind::Interrupted);
        assert_eq!(stream.ended(), Some(Ended::Interrupted));
        assert!(started.elapsed() < Duration::from_secs(5));
    }
}
