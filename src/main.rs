 
 
use clap::{value_parser, Arg, ArgAction, Command};
use log::{debug, error, Log, Record};
use nix::errno::Errno;
use nix::libc;
use nix::poll::{PollFd, PollFlags};
use nix::sys::{signal, socket, stat, wait};
use nix::{fcntl, unistd};
use std::collections::BTreeMap;
use std::env;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::io::Write;
use std::os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Child;
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};

mod bench;
mod compress;
mod damage;
mod dmabuf;
mod gbm;
mod kernel;
mod mainloop;
mod mirror;
mod platform;
mod read;
mod secctx;
mod stub;
mod tracking;
mod util;
mod video;
mod wayland;
mod wayland_gen;

use crate::mainloop::*;
use crate::util::*;

 
struct Logger {
    max_level: log::LevelFilter,
    pid: u32,
    color_output: bool,
    anti_staircase: bool,
    color: usize,
    label: &'static str,
}

impl Log for Logger {
    fn enabled(&self, meta: &log::Metadata<'_>) -> bool {
        meta.level() <= self.max_level
    }
    fn log(&self, record: &Record<'_>) {
        if record.level() > self.max_level {
            return;
        }

        let time = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH);
        let t = if let Ok(t) = time {
            (t.as_nanos() % 100000000000u128) / 1000u128
        } else {
            0
        };
        let (esc1a, esc1b, esc1c) = if self.color_output {
            let c = if self.color == 0 {
                "36"
            } else if self.color == 1 {
                "34"
            } else {
                "35"
            };
            if record.level() <= log::Level::Error {
                ("\x1b[0;", c, ";1m")
            } else {
                ("\x1b[0;", c, "m")
            }
        } else {
            ("", "", "")
        };
        let esc2 = if self.color_output { "\x1b[0m" } else { "" };
        let esc3 = if self.anti_staircase { "\r\n" } else { "\n" };
        let lvl_str: &str = match record.level() {
            log::Level::Error => "ERR",
            log::Level::Warn => "Wrn",
            log::Level::Debug => "dbg",
            log::Level::Info => "inf",
            log::Level::Trace => "trc",
        };

        const MAX_LOG_LEN: usize = 512;
        let mut buf = [0u8; MAX_LOG_LEN];
        let mut cursor = std::io::Cursor::new(&mut buf[..MAX_LOG_LEN - 5]);
        let _ = write!(
            &mut cursor,
            "{}{}{}[{:02}.{:06} {} {}({}) {}:{}]{} {}{}",
            esc1a,
            esc1b,
            esc1c,
            t / 1000000u128,
            t % 1000000u128,
            lvl_str,
            self.label,
            self.pid,
            record
                .file()
                .unwrap_or("src/unknown")
                .strip_prefix("src/")
                .unwrap_or_else(|| record.file().unwrap_or("unknown")),
            record.line().unwrap_or(0),
            esc2,
            record.args(),
            esc3
        );
        let mut str_end = cursor.position() as usize;
        if str_end >= MAX_LOG_LEN - 9 {
             
            str_end = match std::str::from_utf8(&buf[..str_end]) {
                Ok(x) => x.len(),
                Err(y) => y.valid_up_to(),
            };
        }
        if str_end >= MAX_LOG_LEN - 9 {
             
            assert!(str_end <= MAX_LOG_LEN - 5, "{} {}", str_end, MAX_LOG_LEN);
            buf[str_end..str_end + 3].fill(b'.');
            if self.anti_staircase {
                buf[str_end + 3] = b'\r';
                buf[str_end + 4] = b'\n';
                str_end += 5;
            } else {
                buf[str_end + 3] = b'\n';
                str_end += 4;
            }
        }
        let handle = &mut std::io::stderr().lock();
        let _ = handle.write_all(&buf[..str_end]);
        let _ = handle.flush();
    }
    fn flush(&self) {
         
    }
}

 
fn get_rand_tag() -> Result<[u8; 10], String> {
    let mut rand_buf = [0_u8; 16];
    getrandom::getrandom(&mut rand_buf).map_err(|x| tag!("Failed to get random bits: {}", x))?;
    let mut n: u128 = u128::from_le_bytes(rand_buf);

     
     
    let mut rand_tag = [0u8; 10];
    for i in rand_tag.iter_mut() {
        let v = (n % 62) as u32;
        n /= 62;
        *i = if v < 26 {
            (v + ('a' as u32)) as u8
        } else if v < 52 {
            (v - 26 + ('A' as u32)) as u8
        } else {
            (v - 52 + ('0' as u32)) as u8
        }
    }
    Ok(rand_tag)
}

 
#[cfg(target_os = "linux")]
fn dir_flags() -> fcntl::OFlag {
     
    fcntl::OFlag::O_PATH | fcntl::OFlag::O_DIRECTORY
}
#[cfg(not(target_os = "linux"))]
fn dir_flags() -> fcntl::OFlag {
    fcntl::OFlag::O_DIRECTORY
}

 
fn open_folder(p: &Path) -> Result<OwnedFd, String> {
    let p = if p.as_os_str().is_empty() {
        Path::new(".")
    } else {
        p
    };
    fcntl::open(
        p,
        dir_flags() | fcntl::OFlag::O_CLOEXEC | fcntl::OFlag::O_NOCTTY,
        nix::sys::stat::Mode::empty(),
    )
    .map_err(|x| tag!("Failed to open folder '{:?}': {}", p, x))
}

 
#[derive(Debug, Copy, Clone)]
struct VSockConfig {
     
    to_host: bool,
     
    cid: u32,
    port: u32,
}

 
#[derive(Debug, Clone)]
enum SocketSpec {
    VSock(VSockConfig),
    Unix(PathBuf),
}

 
#[cfg(target_os = "linux")]
const VMADDR_CID_HOST: u32 = libc::VMADDR_CID_HOST;
#[cfg(not(target_os = "linux"))]
const VMADDR_CID_HOST: u32 = 0;

impl FromStr for VSockConfig {
    type Err = &'static str;

    fn from_str(mut s: &str) -> Result<Self, Self::Err> {
        const FAILURE: &str = "VSOCK spec should have format [[s]CID:]port";

        let (to_host, cid) = if let Some((mut prefix, suffix)) = s.split_once(':') {
            let to_host = if prefix.starts_with('s') {
                prefix = &prefix[1..];
                true
            } else {
                false
            };
            let cid = prefix.parse::<u32>().map_err(|_| FAILURE)?;
            s = suffix;
            (to_host, cid)
        } else {
            (false, VMADDR_CID_HOST)
        };
        let port = s.parse::<u32>().map_err(|_| FAILURE)?;
        Ok(VSockConfig { to_host, cid, port })
    }
}

impl fmt::Display for VSockConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.cid == VMADDR_CID_HOST && !self.to_host {
            write!(f, "{}", self.port)
        } else {
            let prefix = if self.to_host { "s" } else { "" };
            write!(f, "{}{}:{}", prefix, self.cid, self.port)
        }
    }
}

 
fn prune_connections(connections: &mut BTreeMap<u32, std::process::Child>, pid: nix::unistd::Pid) {
    if let Some(mut child) = connections.remove(&(pid.as_raw() as u32)) {
        debug!("Waiting for dead child {} to reveal status", child.id());
        let _ = child.wait();
        debug!("Status received");
    } else {
        let status = wait::waitpid(pid, Some(wait::WaitPidFlag::WNOHANG));
        error!(
            "Received SIGCHLD for unexpected child {}: {:?}",
            pid.as_raw(),
            status
        );
    }
}

 
fn wait_for_connnections(mut connections: BTreeMap<u32, std::process::Child>) {
    while let Some((_, mut child)) = connections.pop_first() {
        debug!("Waiting for dead child {} to reveal status", child.id());
        let _ = child.wait();
        debug!("Status received");
    }
}

 
fn build_connection_command<'a>(
    strings: &'a mut Vec<OsString>,
    socket_path: &'a SocketSpec,
    options: &'a Options,
    client: bool,
    anti_staircase: bool,
) -> Vec<&'a OsStr> {
    let mut args: Vec<&'a OsStr> = Vec::new();

    strings.push(OsString::from(options.compression.to_string()));
    strings.push(OsString::from(options.threads.to_string()));
    strings.push(OsString::from(format!("--video={}", options.video)));
    match socket_path {
        SocketSpec::VSock(x) => strings.push(format!("{}", x).into()),
        SocketSpec::Unix(x) => strings.push(x.into()),
    };
    let comp_str = &strings[0];
    let thread_str = &strings[1];
    let vid_str = &strings[2];
    let socket_str = &strings[3];

     
    args.push(OsStr::new("-s"));
    args.push(socket_str);
    if matches!(socket_path, SocketSpec::VSock(_)) {
        args.push(OsStr::new("--vsock"));
    }
    if options.debug {
        args.push(OsStr::new("-d"));
    }
    if options.no_gpu {
        args.push(OsStr::new("-n"));
    }
    args.push(OsStr::new("--threads"));
    args.push(thread_str);
    args.push(OsStr::new("-c"));
    args.push(comp_str);
    if !options.title_prefix.is_empty() {
        args.push(OsStr::new("--title-prefix"));
        args.push(OsStr::new(&options.title_prefix));
    }
    if options.video.format.is_some() {
        args.push(vid_str);
    }
    if let Some(d) = &options.drm_node {
        assert!(!client);
        args.push(OsStr::new("--drm-node"));
        args.push(OsStr::new(d));
    }
    if anti_staircase {
        args.push(OsStr::new("--anti-staircase"));
    }
    if let Some(ref path) = options.debug_store_video {
        args.push(OsStr::new("--test-store-video"));
        args.push(path.as_os_str());
    }
    if options.test_skip_vulkan {
        args.push(OsStr::new("--test-skip-vulkan"));
    }
    if options.test_no_timeline_export {
        args.push(OsStr::new("--test-no-timeline-export"));
    }
    if options.test_no_binary_semaphore_import {
        args.push(OsStr::new("--test-no-binary-semaphore-import"));
    }
    if client {
        args.push(OsStr::new("client-conn"));
    } else {
        args.push(OsStr::new("server-conn"));
    }
    args
}

 
fn handle_server_conn(
    link_fd: OwnedFd,
    wayland_fd: OwnedFd,
    opts: &Options,
    wire_version_override: Option<u32>,
) -> Result<(), String> {
     
    let mut header: [u8; 16] = [0_u8; 16];

    let ver = wire_version_override
        .map(|x| x.clamp(MIN_PROTOCOL_VERSION, WAYPIPE_PROTOCOL_VERSION))
        .unwrap_or(WAYPIPE_PROTOCOL_VERSION);

    let ver_hi = ver >> 4;
    let ver_lo = ver & ((1 << 4) - 1);

    let mut lead: u32 = (ver_hi << 16) | (ver_lo << 3) | CONN_FIXED_BIT;
    match opts.compression {
        Compression::None => lead |= CONN_NO_COMPRESSION,
        Compression::Lz4(_) => lead |= CONN_LZ4_COMPRESSION,
        Compression::Zstd(_) => lead |= CONN_ZSTD_COMPRESSION,
    }
    if opts.no_gpu {
        lead |= CONN_NO_DMABUF_SUPPORT;
    }
    if let Some(ref f) = opts.video.format {
        match f {
            VideoFormat::H264 => {
                lead |= CONN_H264_VIDEO;
            }
            VideoFormat::VP9 => {
                lead |= CONN_VP9_VIDEO;
            }
            VideoFormat::AV1 => {
                lead |= CONN_AV1_VIDEO;
            }
        }
    } else {
        lead |= CONN_NO_VIDEO;
    }
    debug!("header: {:0x}", lead);
    header[..4].copy_from_slice(&u32::to_le_bytes(lead));

    write_exact(&link_fd, &header).map_err(|x| tag!("Failed to write connection header: {}", x))?;

    debug!("have written initial bytes");

    set_nonblock(&link_fd)?;
    set_nonblock(&wayland_fd)?;

    let (sigmask, sigint_received) = setup_sigint_handler()?;
    mainloop::main_interface_loop(
        link_fd,
        wayland_fd,
        opts,
        MIN_PROTOCOL_VERSION,
        false,
        sigmask,
        sigint_received,
    )
}

 
fn socket_connect(
    spec: &SocketSpec,
    cwd: &OwnedFd,
    nonblocking: bool,
    unlink_after: bool,  
) -> Result<OwnedFd, String> {
    let socket = match spec {
        SocketSpec::Unix(path) => {
            #[cfg(target_os = "linux")]
            let socket = {
                let socket_flags = if nonblocking {
                    socket::SockFlag::SOCK_CLOEXEC | socket::SockFlag::SOCK_NONBLOCK
                } else {
                    socket::SockFlag::SOCK_CLOEXEC
                };
                socket::socket(
                    socket::AddressFamily::Unix,
                    socket::SockType::Stream,
                    socket_flags,
                    None,
                )
                .map_err(|x| tag!("Failed to create socket: {}", x))?
            };
            #[cfg(not(target_os = "linux"))]
            let socket = {
                let s = socket::socket(
                    socket::AddressFamily::Unix,
                    socket::SockType::Stream,
                    socket::SockFlag::empty(),
                    None,
                )
                .map_err(|x| tag!("Failed to create socket: {}", x))?;
                set_cloexec(&s, true)?;
                if nonblocking {
                    set_nonblock(&s)?;
                }
                s
            };

            let file = path
                .file_name()
                .ok_or_else(|| tag!("Socket path {:?} missing file name", path))?;
            let addr = socket::UnixAddr::new(file)
                .map_err(|x| tag!("Failed to create Unix socket address from file name: {}", x))?;

            let r = if let Some(folder) = path.parent() {
                nix::unistd::chdir(folder).map_err(|x| tag!("Failed to visit folder: {}", x))?;
                 
                 
                let x = socket::connect(socket.as_raw_fd(), &addr);
                if x.is_ok() && unlink_after {
                     
                     
                    nix::unistd::unlink(file)
                        .map_err(|x| tag!("Failed to unlink socket: {}", x))?;
                }
                nix::unistd::fchdir(cwd)
                    .map_err(|x| tag!("Failed to return to original path: {}", x))?;
                x
            } else {
                let x = socket::connect(socket.as_raw_fd(), &addr);
                if x.is_ok() && unlink_after {
                    nix::unistd::unlink(file)
                        .map_err(|x| tag!("Failed to unlink socket: {}", x))?;
                }
                x
            };
            r.map_err(|x| tag!("Failed to connnect to socket at {:?}: {}", path, x))?;

            socket
        }
        #[cfg(target_os = "linux")]
        SocketSpec::VSock(v) => {
            let socket = socket::socket(
                socket::AddressFamily::Vsock,
                socket::SockType::Stream,
                socket::SockFlag::SOCK_CLOEXEC,
                None,
            )
            .map_err(|x| tag!("Failed to create socket: {}", x))?;

            unsafe {
                 
                const VMADDR_FLAG_TO_HOST: u8 = 0x1;
                let svm_flags = if v.to_host { VMADDR_FLAG_TO_HOST } else { 0 };
                let addr = libc::sockaddr_vm {
                    svm_family: libc::AF_VSOCK as u16,
                    svm_reserved1: 0,
                    svm_port: v.port,
                    svm_cid: v.cid,
                    svm_zero: [svm_flags, 0, 0, 0],
                };
                assert!(std::mem::align_of::<libc::sockaddr_vm>() == 4);
                assert!(std::mem::size_of::<libc::sockaddr_vm>() == 16);

                 
                 
                let r = libc::connect(
                    socket.as_raw_fd(),
                    &addr as *const libc::sockaddr_vm as *const libc::sockaddr,
                    std::mem::size_of::<libc::sockaddr_vm>() as _,
                );
                if r != 0 {
                    return Err(tag!(
                        "Failed to connnect to socket at {}: {}",
                        v.to_string(),
                        Errno::last()
                    ));
                }
                socket
            }
        }
        #[cfg(not(target_os = "linux"))]
        SocketSpec::VSock(_) => unreachable!(),
    };
    set_nonblock(&socket)?;
    Ok(socket)
}

 
struct FileCleanup {
    folder: OwnedFd,
    full_path: PathBuf,
}

impl Drop for FileCleanup {
    fn drop(&mut self) {
        let file_name = self.full_path.file_name().unwrap();
        debug!("Trying to unlink socket created at: {:?}", self.full_path);
        if let Err(x) =
            unistd::unlinkat(&self.folder, file_name, unistd::UnlinkatFlags::NoRemoveDir)
        {
            error!(
                "Failed to unlink display socket at: {:?}: {:?}",
                self.full_path, x
            )
        }
    }
}

 
fn unix_socket_create_and_bind(
    path: &Path,
    cwd: &OwnedFd,
     
    nonblock: bool,
    cloexec: bool,
) -> Result<(OwnedFd, FileCleanup), String> {
    #[cfg(target_os = "linux")]
    let flags = {
        let mut f = socket::SockFlag::empty();
        if nonblock { f |= socket::SockFlag::SOCK_NONBLOCK; }
        if cloexec { f |= socket::SockFlag::SOCK_CLOEXEC; }
        f
    };
    #[cfg(not(target_os = "linux"))]
    let flags = socket::SockFlag::empty();
    let socket: OwnedFd = socket::socket(
        socket::AddressFamily::Unix,
        socket::SockType::Stream,
        flags,
        None,
    )
    .map_err(|x| tag!("Failed to create socket: {}", x))?;

    #[cfg(not(target_os = "linux"))]
    {
        use nix::fcntl;
        if cloexec {
            let _ = fcntl::fcntl(&socket, fcntl::FcntlArg::F_SETFD(fcntl::FdFlag::FD_CLOEXEC));
        }
        if nonblock {
            let _ = fcntl::fcntl(&socket, fcntl::FcntlArg::F_SETFL(fcntl::OFlag::O_NONBLOCK));
        }
    }

    let file = path
        .file_name()
        .ok_or_else(|| tag!("Socket path {:?} missing file name", path))?;
    let addr = socket::UnixAddr::new(file)
        .map_err(|x| tag!("Failed to create Unix socket address from file name: {}", x))?;

    let (f, r) = if let Some(folder) = path.parent() {
        let f = open_folder(folder)?;

        unistd::fchdir(&f).map_err(|x| tag!("Failed to visit folder: {}", x))?;
         
         
        let x = socket::bind(socket.as_raw_fd(), &addr);
        unistd::fchdir(cwd).map_err(|x| tag!("Failed to return to original path: {}", x))?;
        (f, x)
    } else {
        let f: OwnedFd =
            OwnedFd::try_clone(cwd).map_err(|x| tag!("Failed to duplicate cwd: {}", x))?;
        let x = socket::bind(socket.as_raw_fd(), &addr);
        (f, x)
    };
    r.map_err(|x| tag!("Failed to bind socket at {:?}: {}", path, x))?;
    Ok((
        socket,
        FileCleanup {
            folder: f,
            full_path: PathBuf::from(path),
        },
    ))
}

 
fn socket_create_and_bind(
    path: &SocketSpec,
    cwd: &OwnedFd,
     
    nonblock: bool,
    cloexec: bool,
) -> Result<(OwnedFd, Option<FileCleanup>), String> {
    match path {
        #[cfg(target_os = "linux")]
        SocketSpec::VSock(spec) => {
            let socket: OwnedFd = socket::socket(
                socket::AddressFamily::Vsock,
                socket::SockType::Stream,
                socket::SockType::Stream,
                {
                    #[cfg(target_os = "linux")]
                    let flags = {
                        let mut f = socket::SockFlag::empty();
                        if nonblock { f |= socket::SockFlag::SOCK_NONBLOCK; }
                        if cloexec { f |= socket::SockFlag::SOCK_CLOEXEC; }
                        f
                    };
                    #[cfg(not(target_os = "linux"))]
                    let flags = socket::SockFlag::empty();
                    flags
                },
                None,
            )
            .map_err(|x| tag!("Failed to create socket: {}", x))?;

            let addr = socket::VsockAddr::new(libc::VMADDR_CID_ANY, spec.port);
            socket::bind(socket.as_raw_fd(), &addr)
                .map_err(|x| tag!("Failed to bind socket at {}: {}", spec.to_string(), x))?;

            Ok((socket, None))
        }
        #[cfg(not(target_os = "linux"))]
        SocketSpec::VSock(_) => unreachable!(),

        SocketSpec::Unix(path) => {
            let (socket, cleanup) = unix_socket_create_and_bind(path, cwd, nonblock, cloexec)?;
            Ok((socket, Some(cleanup)))
        }
    }
}

 
fn connect_to_display_at(cwd: &OwnedFd, path: &Path) -> Result<OwnedFd, String> {
    socket_connect(&SocketSpec::Unix(path.into()), cwd, true, false)
}

 
fn connect_to_wayland_display(cwd: &OwnedFd) -> Result<OwnedFd, String> {
    let wayl_disp = std::env::var_os("WAYLAND_DISPLAY")
        .ok_or("Missing environment variable WAYLAND_DISPLAY")?;
    let leading_slash: &[u8] = b"/";

    if wayl_disp.as_encoded_bytes().starts_with(leading_slash) {
        connect_to_display_at(cwd, Path::new(&wayl_disp))
    } else if let Some(dir) = std::env::var_os("XDG_RUNTIME_DIR") {
        let mut path = PathBuf::new();
        path.push(dir);
        path.push(wayl_disp);
        connect_to_display_at(cwd, &path)
    } else {
        Err(tag!("XDG_RUNTIME_DIR was not in environment"))
    }
}

 
fn get_wayland_socket_id() -> Result<Option<i32>, String> {
    if let Some(x) = std::env::var_os("WAYLAND_SOCKET") {
        let y = x
            .into_string()
            .ok()
            .and_then(|x| x.parse::<i32>().ok())
            .ok_or("Failed to parse connection fd")?;
        Ok(Some(y))
    } else {
        Ok(None)
    }
}

 
static SIGINT_RECEIVED: AtomicBool = AtomicBool::new(false);

 
extern "C" fn sigint_handler(_signo: i32) {
    SIGINT_RECEIVED.store(true, Ordering::Release);
}

 
fn setup_sigint_handler() -> Result<(signal::SigSet, &'static AtomicBool), String> {
     
    let mut mask = signal::SigSet::empty();
    mask.add(signal::SIGINT);
    let mut pollmask = mask
        .thread_swap_mask(signal::SigmaskHow::SIG_BLOCK)
        .map_err(|x| tag!("Failed to set sigmask: {}", x))?;
    pollmask.remove(signal::SIGINT);

    let sigaction = signal::SigAction::new(
        signal::SigHandler::Handler(sigint_handler),
        signal::SaFlags::SA_NOCLDSTOP,
        signal::SigSet::empty(),
    );
    unsafe {
         
         
        signal::sigaction(signal::Signal::SIGINT, &sigaction)
            .map_err(|x| tag!("Failed to set sigaction: {}", x))?;
    }

    Ok((pollmask, &SIGINT_RECEIVED))
}

 
fn handle_client_conn(link_fd: OwnedFd, wayland_fd: OwnedFd, opts: &Options) -> Result<(), String> {
    let mut buf = [0_u8; 16];
    read_exact(&link_fd, &mut buf)
        .map_err(|x| tag!("Failed to read connection header: {:?}", x))?;

    let header = u32::from_le_bytes(buf[..4].try_into().unwrap());
    debug!("Connection header: 0x{:08x}", header);

    if header & CONN_FIXED_BIT == 0 && header & CONN_UNSET_BIT != 0 {
        error!("Possible endianness mismatch");
        return Err(tag!(
            "Header failure: possible endianness mismatch, or garbage input"
        ));
    }

    let version = (((header >> 16) & 0xff) << 4) | (header >> 3) & 0xf;
    let min_version = std::cmp::min(version, WAYPIPE_PROTOCOL_VERSION);
    debug!(
        "Connection remote version is {}, local is {}, using {}",
        version, WAYPIPE_PROTOCOL_VERSION, min_version
    );

    let comp = header & CONN_COMPRESSION_MASK;
     
    let expected_comp = match opts.compression {
        Compression::None => CONN_NO_COMPRESSION,
        Compression::Lz4(_) => CONN_LZ4_COMPRESSION,
        Compression::Zstd(_) => CONN_ZSTD_COMPRESSION,
    };

    if comp != expected_comp {
        error!("Rejecting connection header {:x} due to compression type mismatch: header has {:x} != own {:x}", header, comp, expected_comp);
        return Err(tag!("Header compression failure"));
    }

    let video = header & CONN_VIDEO_MASK;
     
    match video {
        CONN_NO_VIDEO => {
            debug!("Connected waypipe-server not receiving video");
        }
        CONN_H264_VIDEO => {
            debug!("Connected waypipe-server may send H264 video");
        }
        CONN_VP9_VIDEO => {
            debug!("Connected waypipe-server may send VP9 video");
        }
        CONN_AV1_VIDEO => {
            debug!("Connected waypipe-server may send AV1 video");
        }
        _ => {
            debug!("Unknown video format specification")
        }
    }

     
    let remote_using_dmabuf = header & CONN_NO_DMABUF_SUPPORT == 0;
    debug!(
        "Connected waypipe-server may use dmabufs: {}",
        remote_using_dmabuf
    );

    set_nonblock(&link_fd)?;
    set_nonblock(&wayland_fd)?;

    let (sigmask, sigint_received) = setup_sigint_handler()?;
    mainloop::main_interface_loop(
        link_fd,
        wayland_fd,
        opts,
        min_version,
        true,
        sigmask,
        sigint_received,
    )
}

 
fn spawn_connection_handler(
    self_path: &OsStr,
    conn_args: &[&OsStr],
    connection_fd: OwnedFd,
    wayland_display: Option<&OsStr>,
) -> Result<Child, String> {
     
    let mut process = std::process::Command::new(self_path);
    process
        .arg0(env!("CARGO_PKG_NAME"))
        .args(conn_args)
        .env(
            "WAYPIPE_CONNECTION_FD",
            format!("{}", connection_fd.as_raw_fd()),
        )
        .env_remove("WAYLAND_SOCKET");
    if let Some(disp) = wayland_display {
        process.env("WAYLAND_DISPLAY", disp);
    }

    let child = process.spawn().map_err(|x| {
        tag!(
            "Failed to run connection subprocess with path {:?}: {}",
            self_path,
            x
        )
    })?;

    drop(connection_fd);

    Ok(child)
}

 
fn run_server_oneshot(
    command: &[&std::ffi::OsStr],
    argv0: &std::ffi::OsStr,
    options: &Options,
    unlink_at_end: bool,
    socket_path: &SocketSpec,
    cwd: &OwnedFd,
) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    let (sock1, sock2) = socket::socketpair(
        socket::AddressFamily::Unix,
        socket::SockType::Stream,
        None,
        socket::SockFlag::SOCK_NONBLOCK | socket::SockFlag::SOCK_CLOEXEC,
    )
    .map_err(|x| tag!("Failed to create socketpair: {}", x))?;
    #[cfg(not(target_os = "linux"))]
    let (sock1, sock2) = {
        let (s1, s2) = socket::socketpair(
            socket::AddressFamily::Unix,
            socket::SockType::Stream,
            None,
            socket::SockFlag::empty(),
        )
        .map_err(|x| tag!("Failed to create socketpair: {}", x))?;
        set_cloexec(&s1, true)?;
        set_cloexec(&s2, true)?;
        set_nonblock(&s1)?;
        set_nonblock(&s2)?;
        (s1, s2)
    };

    let sock_str = format!("{}", sock2.as_raw_fd());
    set_cloexec(&sock2, false)?;

    let mut cmd_child: std::process::Child = std::process::Command::new(command[0])
        .arg0(argv0)
        .args(&command[1..])
        .env("WAYLAND_SOCKET", &sock_str)
        .env_remove("WAYLAND_DISPLAY")
        .spawn()
        .map_err(|x| tag!("Failed to run program {:?}: {}", command[0], x))?;
    drop(sock2);

    let link_fd = socket_connect(socket_path, cwd, false, unlink_at_end)?;

    handle_server_conn(link_fd, sock1, options, None)?;

    debug!("Waiting for only child {} to reveal status", cmd_child.id());
    let _ = cmd_child.wait();
    debug!("Status received");

    Ok(())
}

 
struct XSocketInfo {
    #[cfg(target_os = "linux")]
    abstract_socket: OwnedFd,
    unix_socket: OwnedFd,
    display: u8,
}

 
struct XCleanup {
     
    tmp_fd: OwnedFd,
     
    x11_unix_fd: OwnedFd,
     
    display: u8,
}

impl Drop for XCleanup {
    fn drop(&mut self) {
        let mut lock_file_buf = [0u8; 32];
        let lock_file_name = Path::new(OsStr::from_bytes(write_with_buffer(
            &mut lock_file_buf,
            |x| write!(x, ".X{}-lock", self.display).expect("not too long"),
        )));

        let mut socket_buf = [0u8; 16];
        let socket_name = Path::new(OsStr::from_bytes(write_with_buffer(&mut socket_buf, |x| {
            write!(x, "X{}", self.display).expect("not too long")
        })));

        debug!(
            "Trying to unlink socket created at: /tmp/.X11-unix/X{}",
            self.display
        );
        if let Err(e) = unistd::unlinkat(
            &self.x11_unix_fd,
            socket_name,
            unistd::UnlinkatFlags::NoRemoveDir,
        ) {
            error!(
                "Failed to unlink display socket at: /tmp/.X11-unix/X{}: {:?}",
                self.display, e
            )
        }

         
        debug!(
            "Trying to unlink lock file created at: /tmp/.X{}-lock",
            self.display
        );
        if let Err(e) = unistd::unlinkat(
            &self.tmp_fd,
            lock_file_name,
            unistd::UnlinkatFlags::NoRemoveDir,
        ) {
            error!(
                "Failed to unlink lock file at: /tmp/.X{}-lock: {:?}",
                self.display, e
            )
        }

         
    }
}

 
fn choose_x_display(cwd: &OwnedFd) -> Result<(XSocketInfo, XCleanup), String> {
    let tmp_fd = open_folder(Path::new("/tmp/"))?;

    match stat::mkdirat(
        &tmp_fd,
        Path::new(".X11-unix"),
        stat::Mode::from_bits_retain(0b111111111),
    ) {
        Err(Errno::EEXIST) | Ok(()) => (),
        Err(e) => {
            return Err(tag!(
                "Failed to create subfolder '.X11-unix' of '/tmp': {}",
                e
            ));
        }
    }

    let x11_unix_fd = fcntl::openat(
        &tmp_fd,
        Path::new(".X11-unix"),
        dir_flags() | fcntl::OFlag::O_CLOEXEC | fcntl::OFlag::O_NOCTTY,
        nix::sys::stat::Mode::empty(),
    )
    .map_err(|x| tag!("Failed to open subfolder '.X11-unix' of '/tmp': {}", x))?;

     
    #[cfg(target_os = "linux")]
    let unix_socket: OwnedFd = socket::socket(
        socket::AddressFamily::Unix,
        socket::SockType::Stream,
        socket::SockFlag::SOCK_NONBLOCK | socket::SockFlag::SOCK_CLOEXEC,
        None,
    )
    .map_err(|x| tag!("Failed to create socket: {}", x))?;
    #[cfg(not(target_os = "linux"))]
    let unix_socket: OwnedFd = {
        let s = socket::socket(
            socket::AddressFamily::Unix,
            socket::SockType::Stream,
            socket::SockFlag::empty(),
            None,
        )
        .map_err(|x| tag!("Failed to create socket: {}", x))?;
        set_cloexec(&s, true)?;
        set_nonblock(&s)?;
        s
    };

     
    #[cfg(target_os = "linux")]
    fn bind_sockets(
        unix_socket: &OwnedFd,
        x11_unix_fd: &OwnedFd,
        cwd: &OwnedFd,
        display: u8,
    ) -> Result<Option<OwnedFd>, String> {
        let mut abstract_path_buf = [0u8; 32];
        let abstract_path = Path::new(OsStr::from_bytes(write_with_buffer(
            &mut abstract_path_buf,
            |x| write!(x, "/tmp/.X11-unix/X{}", display).expect("not too long"),
        )));

        let abstract_addr =
            socket::UnixAddr::new_abstract(abstract_path.as_os_str().as_encoded_bytes())
                .expect("abstract address should be short");

        let abstract_socket: OwnedFd = socket::socket(
            socket::AddressFamily::Unix,
            socket::SockType::Stream,
            socket::SockFlag::SOCK_NONBLOCK | socket::SockFlag::SOCK_CLOEXEC,
            None,
        )
        .map_err(|x| tag!("Failed to create socket: {}", x))?;

        match socket::bind(abstract_socket.as_raw_fd(), &abstract_addr) {
            Err(Errno::EADDRINUSE) => {
                debug!(
                    "Skipping X display number {}, abstract socket at {:?} already in use",
                    display, abstract_path
                );
                return Ok(None);
            }
            Ok(()) => (),
            Err(e) => {
                return Err(tag!(
                    "Failed to bind abstract socket at {:?}: {}",
                    abstract_path,
                    e
                ));
            }
        }

        match bind_unix_socket(unix_socket, x11_unix_fd, cwd, display) {
            Ok(Some(())) => (),
            Ok(None) => return Ok(None),
            Err(x) => return Err(x),
        };

        Ok(Some(abstract_socket))
    }

     
    fn bind_unix_socket(
        unix_socket: &OwnedFd,
        x11_unix_fd: &OwnedFd,
        cwd: &OwnedFd,
        display: u8,
    ) -> Result<Option<()>, String> {
        let mut sockname_buf = [0u8; 16];
        let regular_sockname = Path::new(OsStr::from_bytes(write_with_buffer(
            &mut sockname_buf,
            |x| write!(x, "X{}", display).expect("not too long"),
        )));

        let reg_addr = socket::UnixAddr::new(regular_sockname).expect("filename should be short");

        if let Err(e) = unistd::fchdir(x11_unix_fd) {
            return Err(tag!("Failed to visit folder '/tmp/.X11-unix': {:?}", e));
        }

        let r = socket::bind(unix_socket.as_raw_fd(), &reg_addr);

        unistd::fchdir(cwd).map_err(|x| tag!("Failed to return to original path: {}", x))?;

        match r {
            Err(Errno::EADDRINUSE) => {
                debug!(
                    "Skipping X display number {}, unix socket at /tmp/.X11-unix/{:?} already in use",
                    display, regular_sockname
                );
                return Ok(None);
            }
            Ok(()) => (),
            Err(e) => {
                return Err(tag!(
                    "Failed to bind unix socket at /tmp/.X11-unix/{:?}: {}",
                    display,
                    e
                ));
            }
        }
        Ok(Some(()))
    }

    for display in 0..u8::MAX {
        let lock_file_name = format!(".X{}-lock", display);

        let pid = unistd::getpid();
        let lock_fd = match fcntl::openat(
            &tmp_fd,
            Path::new(&lock_file_name),
            fcntl::OFlag::O_CLOEXEC
                | fcntl::OFlag::O_NOCTTY
                | fcntl::OFlag::O_WRONLY
                | fcntl::OFlag::O_CREAT
                | fcntl::OFlag::O_EXCL,
            nix::sys::stat::Mode::from_bits_retain(0b100100100),
        ) {
            Err(Errno::EEXIST) => {
                 
                debug!(
                    "Skipping X display number {}, lock file already exists",
                    display
                );
                continue;
            }
            Ok(v) => v,
            Err(e) => {
                return Err(tag!("Failed to create X lock file in '/tmp': {}", e));
            }
        };

        let mut contents_buf = [0u8; 11];
        let contents = write_with_buffer(&mut contents_buf, |x| {
            writeln!(x, "{: >10}", pid).expect("pid should be representable in 10 digits")
        });
         
         
         
         
        if let Err(e) = write_exact(&lock_fd, contents) {
            unistd::unlinkat(
                &tmp_fd,
                Path::new(&lock_file_name),
                unistd::UnlinkatFlags::NoRemoveDir,
            )
            .map_err(|x| {
                tag!(
                    "Failed to unlink X lock file for display {}: {:?}",
                    display,
                    x
                )
            })?;
            return Err(tag!(
                "Failed to write pid to X lock file in '/tmp': {:?}",
                e
            ));
        }

        #[cfg(target_os = "linux")]
        let r = bind_sockets(&unix_socket, &x11_unix_fd, cwd, display);

        #[cfg(not(target_os = "linux"))]
        let r = bind_unix_socket(&unix_socket, &x11_unix_fd, cwd, display);

        let abstract_socket = match r {
            Ok(Some(x)) => x,
            Err(_) | Ok(None) => {
                unistd::unlinkat(
                    &tmp_fd,
                    Path::new(&lock_file_name),
                    unistd::UnlinkatFlags::NoRemoveDir,
                )
                .map_err(|x| {
                    tag!(
                        "Failed to unlink X lock file for display {}: {:?}",
                        display,
                        x
                    )
                })?;
                 
                r?;
                continue;
            }
        };

        return Ok((
            XSocketInfo {
                #[cfg(target_os = "linux")]
                abstract_socket,
                unix_socket,
                display,
            },
            XCleanup {
                tmp_fd,
                x11_unix_fd,
                display,
            },
        ));
    }

    unistd::fchdir(cwd).map_err(|x| tag!("Failed to return to original path: {}", x))?;

    Err(tag!("Failed to bind a valid X display number in 0..=255"))
}

 
fn spawn_xwls_handler(
    wayland_display: &OsStr,
    xsock: XSocketInfo,
) -> Result<std::process::Child, String> {
    let mut x_disp_buf: [u8; 11] = [0; 11];
    let x_disp_slice = write_with_buffer(&mut x_disp_buf, |x: &mut &mut [u8]| {
        write!(x, ":{}", xsock.display).expect("buffer should be long enough")
    });

    #[cfg(target_os = "linux")]
    let mut abstract_buf: [u8; 11] = [0; 11];
    #[cfg(target_os = "linux")]
    let abstract_slice = write_with_buffer(&mut abstract_buf, |x: &mut &mut [u8]| {
        write!(x, "{}", xsock.abstract_socket.as_raw_fd()).expect("buffer should be long enough")
    });
    let mut regular_buf: [u8; 11] = [0; 11];
    let regular_slice = write_with_buffer(&mut regular_buf, |x: &mut &mut [u8]| {
        write!(x, "{}", xsock.unix_socket.as_raw_fd()).expect("buffer should be long enough")
    });

    let program = OsStr::new("xwayland-satellite");
    #[cfg(target_os = "linux")]
    set_cloexec(&xsock.abstract_socket, false)?;
    set_cloexec(&xsock.unix_socket, false)?;
    let child = std::process::Command::new(program)
        .args([
            OsStr::from_bytes(x_disp_slice),
            #[cfg(target_os = "linux")]
            OsStr::new("-listenfd"),
            #[cfg(target_os = "linux")]
            OsStr::from_bytes(abstract_slice),
            OsStr::new("-listenfd"),
            OsStr::from_bytes(regular_slice),
        ])
        .env("WAYLAND_DISPLAY", wayland_display)
        .spawn()
        .map_err(|x| tag!("Failed to run program {:?}: {}", program, x))?;
     
    drop(xsock);
    Ok(child)
}

 
fn run_server_inner(
    display_socket: &OwnedFd,
    argv0: &std::ffi::OsStr,
    command_with_args: &[&OsStr],
    display_short: &OsStr,
    conn_args: &[&OsStr],
    mut opt_xsock: Option<XSocketInfo>,
    opt_xchild: &mut Option<std::process::Child>,
    connections: &mut BTreeMap<u32, std::process::Child>,
) -> Result<std::process::Child, String> {
    let mut command = std::process::Command::new(command_with_args[0]);
    command
        .arg0(argv0)
        .args(&command_with_args[1..])
        .env("WAYLAND_DISPLAY", display_short)
        .env_remove("WAYLAND_SOCKET");
    let mut x_disp_buf: [u8; 4] = [0; 4];
    if let Some(ref xsock) = opt_xsock {
        let mut len = x_disp_buf.len();
        let mut x_disp_slice: &mut [u8] = &mut x_disp_buf;
        write!(x_disp_slice, ":{}", xsock.display).unwrap();
        len -= x_disp_slice.len();
        command.env("DISPLAY", OsStr::from_bytes(&x_disp_buf[..len]));
    }

     
    socket::listen(&display_socket, socket::Backlog::MAXCONN)
        .map_err(|x| tag!("Failed to listen to socket: {}", x))?;
    if let Some(ref xsock) = opt_xsock {
        #[cfg(target_os = "linux")]
        socket::listen(&xsock.abstract_socket, socket::Backlog::MAXCONN)
            .map_err(|x| tag!("Failed to listen to socket: {}", x))?;
        socket::listen(&xsock.unix_socket, socket::Backlog::MAXCONN)
            .map_err(|x| tag!("Failed to listen to socket: {}", x))?;
    }

    let mut cmd_child: std::process::Child = command
        .spawn()
        .map_err(|x| tag!("Failed to run program {:?}: {}", command_with_args[0], x))?;

     
    let mut mask = signal::SigSet::empty();
    mask.add(signal::SIGCHLD);
    let mut pollmask = mask
        .thread_swap_mask(signal::SigmaskHow::SIG_BLOCK)
        .map_err(|x| tag!("Failed to set sigmask: {}", x))?;
    pollmask.remove(signal::SIGCHLD);

    let sigaction = signal::SigAction::new(
        signal::SigHandler::Handler(noop_signal_handler),
        signal::SaFlags::SA_NOCLDSTOP,
        signal::SigSet::empty(),
    );
    unsafe {
         
        signal::sigaction(signal::Signal::SIGCHLD, &sigaction)
            .map_err(|x| tag!("Failed to set sigaction: {}", x))?;
    }

    let self_path = env::current_exe()
        .map_err(|x| tag!("Failed to lookup path to own executable: {}", x))?
        .into_os_string();

    'outer: loop {
        loop {
             
            #[cfg(target_os = "linux")]
            let res = wait::waitid(
                wait::Id::All,
                wait::WaitPidFlag::WEXITED
                    | wait::WaitPidFlag::WNOHANG
                    | wait::WaitPidFlag::WNOWAIT,
            );
            #[cfg(not(target_os = "linux"))]
            let res = wait::waitpid(
                Option::None,
                Some(wait::WaitPidFlag::WNOHANG | wait::WaitPidFlag::WNOWAIT),
            );
            match res {
                Ok(status) => {
                    let opid = match status {
                        wait::WaitStatus::Exited(pid, _code) => Some(pid),
                        wait::WaitStatus::Signaled(pid, _signal, _bool) => Some(pid),
                        wait::WaitStatus::StillAlive => {
                            break;
                        }
                        _ => {
                            panic!("Unexpected process status: {:?}", status);
                        }
                    };
                    if let Some(pid) = opid {
                        if pid.as_raw() as u32 == cmd_child.id() {
                            let _ = cmd_child.wait();
                            debug!("Exiting, main command has stopped");
                            break 'outer;
                        }
                        let mut found = false;
                        if let Some(ref mut xchild) = opt_xchild {
                            if pid.as_raw() as u32 == xchild.id() {
                                let _ = xchild.wait();
                                error!("xwayland-satellite stopped early");
                                 
                                *opt_xchild = None;
                                found = true;
                            }
                        }
                        if !found {
                            prune_connections(connections, pid);
                        }
                    }
                }
                Err(Errno::ECHILD) => {
                    error!("Unexpected: no unwaited for children");
                    break 'outer;
                }
                Err(errno) => {
                    eprintln!("waitpid failed with unexpected error: {}", errno);
                    assert!(errno == Errno::EINTR);
                }
            }
        }

         
        let (mut pfds_with_xsock, mut pfds_base) = if let Some(ref xsock) = opt_xsock {
            (
                Some([
                    PollFd::new(display_socket.as_fd(), PollFlags::POLLIN),
                    PollFd::new(xsock.unix_socket.as_fd(), PollFlags::POLLIN),
                    #[cfg(target_os = "linux")]
                    PollFd::new(xsock.abstract_socket.as_fd(), PollFlags::POLLIN),
                ]),
                None,
            )
        } else {
            (
                None,
                Some([PollFd::new(display_socket.as_fd(), PollFlags::POLLIN)]),
            )
        };

        let pfds: &mut [PollFd] = if let Some(ref mut v) = pfds_with_xsock {
            v
        } else {
            pfds_base.as_mut().unwrap()
        };

        #[cfg(target_os = "linux")]
        let res = nix::poll::ppoll(pfds, None, Some(pollmask));
        #[cfg(not(target_os = "linux"))]
        let res = nix::poll::poll(pfds, nix::poll::PollTimeout::NONE);
        if let Err(errno) = res {
            assert!(errno == Errno::EINTR || errno == Errno::EAGAIN);
            continue;
        }

        if pfds
            .iter()
            .any(|p| p.revents().unwrap().contains(PollFlags::POLLERR))
        {
            debug!("Exiting, socket error");
            break 'outer;
        }

        let has_wayland_conn = pfds[0].revents().unwrap().contains(PollFlags::POLLIN);

        let mut has_x_conn =
            opt_xsock.is_some() && pfds[1].revents().unwrap().contains(PollFlags::POLLIN);
        if cfg!(target_os = "linux") {
            has_x_conn |=
                opt_xsock.is_some() && pfds[2].revents().unwrap().contains(PollFlags::POLLIN);
        }

        if has_x_conn {
            let xsock = opt_xsock.take().unwrap();
            debug!("X connection received, trying to spawn xwayland-satellite");

            match spawn_xwls_handler(display_short, xsock) {
                Ok(c) => *opt_xchild = Some(c),
                Err(e) => {
                     
                    error!(
                        "Failed to start xwayland-satellite to handle new X11 connection: {:?}",
                        e
                    );
                }
            }
        }

        if has_wayland_conn {
             
            debug!("Connection received");

            let res = socket::accept(display_socket.as_raw_fd());
            match res {
                Ok(conn_fd) => {
                    let wrapped_fd = unsafe {
                         
                        OwnedFd::from_raw_fd(conn_fd)
                    };
                    set_blocking(&wrapped_fd)?;

                    let child = spawn_connection_handler(&self_path, conn_args, wrapped_fd, None)?;
                    let cid = child.id();
                    if connections.insert(cid, child).is_some() {
                        return Err(tag!("Pid reuse: {}", cid));
                    }
                }
                Err(errno) => {
                    assert!(errno != Errno::EBADF && errno != Errno::EINVAL);
                     
                     
                    debug!("Failed to receive connection");
                }
            }
        }
    }

    Ok(cmd_child)
}

 
fn run_server_multi(
    command: &[&std::ffi::OsStr],
    argv0: &std::ffi::OsStr,
    options: &Options,
    unlink_at_end: bool,
    socket_path: &SocketSpec,
    display_short: &OsStr,
    display: &OsStr,
    cwd: &OwnedFd,
    run_xwls: bool,
) -> Result<(), String> {
    let mut connections = BTreeMap::new();

    let mut conn_strings = Vec::new();
    let conn_args = build_connection_command(&mut conn_strings, socket_path, options, false, false);

    let (display_socket, sock_cleanup) = unix_socket_create_and_bind(
        &PathBuf::from(display),
        cwd,
         
        true,  
        true,  
    )?;

    let (mut opt_x, mut opt_cleanup) = (None, None);
    if run_xwls {
        let (x_disp, x_cleanup) = choose_x_display(cwd)?;
        opt_x = Some(x_disp);
        opt_cleanup = Some(x_cleanup);
    }

    let mut xwls_child = None;
    let res = run_server_inner(
        &display_socket,
        argv0,
        command,
        display_short,
        &conn_args,
        opt_x,
        &mut xwls_child,
        &mut connections,
    );
     
    drop(opt_cleanup);

    if let Some(mut child) = xwls_child {
         
        if let Err(e) = child.kill() {
            debug!(
                "Failed to send kill signal to xwayland-satellite subprocess: {:?}",
                e
            );
        }
        if let Err(e) = child.wait() {
            debug!(
                "Failed to wait for xwayland-satellite subprocess to exit: {:?}",
                e
            );
        }
    }

     
    let sock_err = if unlink_at_end {
        if let SocketSpec::Unix(p) = socket_path {
            nix::unistd::unlink(p).map_err(|x| tag!("Failed to unlink socket: {}", x))
        } else {
            Ok(())
        }
    } else {
        Ok(())
    };
     
    drop(sock_cleanup);
    if let Err(err) = res {
        if let Err(e) = sock_err {
            error!("While cleaning up: {}", e);
        }
        return Err(err);
    }
    sock_err?;

    debug!("Shutting down");
     
    wait_for_connnections(connections);
    debug!("Done");
    Ok(())
}

 
fn run_client_oneshot(
    command: Option<&[&std::ffi::OsStr]>,
    options: &Options,
    wayland_fd: OwnedFd,
    socket_path: &SocketSpec,
    cwd: &OwnedFd,
) -> Result<(), String> {
    let (channel_socket, sock_cleanup) =
        socket_create_and_bind(socket_path, cwd,   false, true)?;

    socket::listen(&channel_socket, socket::Backlog::new(1).unwrap())
        .map_err(|x| tag!("Failed to listen to socket: {}", x))?;

     
    let mut cmd_child: Option<std::process::Child> = None;
    if let Some(command_seq) = command {
        cmd_child = Some(
            std::process::Command::new(command_seq[0])
                .args(&command_seq[1..])
                .env_remove("WAYLAND_DISPLAY")
                .env_remove("WAYLAND_SOCKET")
                .spawn()
                .map_err(|x| tag!("Failed to run program {:?}: {}", command_seq[0], x))?,
        );
    }
    let link_fd = loop {
        let res = socket::accept(channel_socket.as_raw_fd());
        match res {
            Ok(conn_fd) => {
                break unsafe {
                     
                    OwnedFd::from_raw_fd(conn_fd)
                };
            }
            Err(Errno::EINTR) => continue,
            Err(x) => {
                return Err(tag!("Failed to accept for socket: {}", x));
            }
        }
    };
    set_cloexec(&link_fd, true)?;
    set_blocking(&link_fd)?;

     
    drop(sock_cleanup);

    handle_client_conn(link_fd, wayland_fd, options)?;

    if let Some(mut c) = cmd_child {
        debug!("Waiting for only child {} to reveal status", c.id());
        let _ = c.wait();
        debug!("Status received");
    }
    debug!("Done");

    Ok(())
}

 
extern "C" fn noop_signal_handler(_: i32) {}

 
fn run_client_inner(
    channel_socket: &OwnedFd,
    command: Option<&[&OsStr]>,
    conn_args: &[&OsStr],
    wayland_display: &OsStr,
    connections: &mut BTreeMap<u32, std::process::Child>,
) -> Result<Option<std::process::Child>, String> {
    socket::listen(&channel_socket, socket::Backlog::MAXCONN)
        .map_err(|x| tag!("Failed to listen to socket: {}", x))?;

     
    let mut cmd_child: Option<std::process::Child> = None;
    if let Some(command_seq) = command {
        cmd_child = Some(
            std::process::Command::new(command_seq[0])
                .args(&command_seq[1..])
                .env_remove("WAYLAND_DISPLAY")
                .env_remove("WAYLAND_SOCKET")
                .spawn()
                .map_err(|x| tag!("Failed to run program {:?}: {}", command_seq[0], x))?,
        );
    }

     
    let mut mask = signal::SigSet::empty();
    mask.add(signal::SIGCHLD);
    let mut pollmask = mask
        .thread_swap_mask(signal::SigmaskHow::SIG_BLOCK)
        .map_err(|x| tag!("Failed to set sigmask: {}", x))?;
    pollmask.remove(signal::SIGCHLD);

    let sigaction = signal::SigAction::new(
        signal::SigHandler::Handler(noop_signal_handler),
        signal::SaFlags::SA_NOCLDSTOP,
        signal::SigSet::empty(),
    );
    unsafe {
         
        signal::sigaction(signal::Signal::SIGCHLD, &sigaction)
            .map_err(|x| tag!("Failed to set sigaction: {}", x))?;
    }

     
    let self_path = env::current_exe()
        .map_err(|x| tag!("Failed to lookup path to own executable: {}", x))?
        .into_os_string();

    'outer: loop {
         
        loop {
            #[cfg(target_os = "linux")]
            let res = wait::waitid(
                wait::Id::All,
                wait::WaitPidFlag::WEXITED
                    | wait::WaitPidFlag::WNOHANG
                    | wait::WaitPidFlag::WNOWAIT,
            );
            #[cfg(not(target_os = "linux"))]
            let res = wait::waitpid(
                Option::None,
                Some(wait::WaitPidFlag::WNOHANG),
            );
            match res {
                Ok(status) => match status {
                    wait::WaitStatus::Exited(pid, _code) => {
                        if let Some(ref mut c) = cmd_child {
                            if pid.as_raw() as u32 == c.id() {
                                let _ = c.wait();
                                debug!("Exiting, main command has stopped");
                                break 'outer;
                            }
                        }
                        prune_connections(connections, pid);
                    }
                    wait::WaitStatus::Signaled(pid, _signal, _bool) => {
                        if let Some(ref mut c) = cmd_child {
                            if pid.as_raw() as u32 == c.id() {
                                let _ = c.wait();
                                debug!("Exiting, main command has stopped");
                                break 'outer;
                            }
                        }
                        prune_connections(connections, pid);
                    }
                    wait::WaitStatus::StillAlive => {
                        break;
                    }
                    _ => {
                        panic!("Unexpected process status: {:?}", status);
                    }
                },
                Err(Errno::ECHILD) => {
                     
                    break;
                }
                Err(errno) => {
                    eprintln!("waitpid failed at 1793 with: {}", errno);
                    assert!(errno == Errno::EINTR);
                    break;
                }
            }
        }

         
        let mut pfds = [PollFd::new(channel_socket.as_fd(), PollFlags::POLLIN)];
        #[cfg(target_os = "linux")]
        let res = nix::poll::ppoll(&mut pfds, None, Some(pollmask));
        #[cfg(not(target_os = "linux"))]
        let res = nix::poll::poll(&mut pfds, nix::poll::PollTimeout::NONE);
        if let Err(errno) = res {
            assert!(errno == Errno::EINTR || errno == Errno::EAGAIN);
            continue;
        }

        let rev = pfds[0].revents().unwrap();
        if rev.contains(PollFlags::POLLERR) {
            debug!("Exiting, socket error");
            break 'outer;
        }
        if !rev.contains(PollFlags::POLLIN) {
            continue;
        }

         
        debug!("Connection received");

        let res = socket::accept(channel_socket.as_raw_fd());
        match res {
            Ok(conn_fd) => {
                 
                 
                let wrapped_fd = unsafe {
                     
                    OwnedFd::from_raw_fd(conn_fd)
                };

                set_blocking(&wrapped_fd)?;
                let child = spawn_connection_handler(
                    &self_path,
                    conn_args,
                    wrapped_fd,
                    Some(wayland_display),
                )?;
                let cid = child.id();
                if connections.insert(cid, child).is_some() {
                    return Err(tag!("Pid reuse: {}", cid));
                }
            }
            Err(errno) => {
                assert!(errno != Errno::EBADF && errno != Errno::EINVAL);
                 
                 
                debug!("Failed to receive connection");
            }
        }
    }
    Ok(cmd_child)
}

 
fn run_client_multi(
    command: Option<&[&std::ffi::OsStr]>,
    options: &Options,
    socket_path: &SocketSpec,
    wayland_display: &OsStr,
    anti_staircase: bool,
    cwd: &OwnedFd,
) -> Result<(), String> {
    let mut conn_strings = Vec::new();
    let conn_args = build_connection_command(
        &mut conn_strings,
        socket_path,
        options,
        true,
        anti_staircase,
    );

    let (channel_socket, sock_cleanup) = socket_create_and_bind(
        socket_path,
        cwd,
         
        true,
        true,
    )?;

    let mut connections = BTreeMap::new();
    let cmd_child = run_client_inner(
        &channel_socket,
        command,
        &conn_args,
        wayland_display,
        &mut connections,
    )?;
    drop(sock_cleanup);

    debug!("Shutting down");
    wait_for_connnections(connections);

    if let Some(mut child) = cmd_child {
        debug!(
            "Waiting for client command child {} to reveal status",
            child.id()
        );
        let _ = child.wait();
        debug!("Status received");
    }

    debug!("Done");
    Ok(())
}

 
fn run_client(
    command: Option<&[&std::ffi::OsStr]>,
    opts: &Options,
    oneshot: bool,
    socket_path: &SocketSpec,
    anti_staircase: bool,
    cwd: &OwnedFd,
    wayland_socket: Option<OwnedFd>,
    secctx: Option<&str>,
) -> Result<(), String> {
    if let Some(app_id) = secctx {
        let (wayland_disp, sock_cleanup, close_fd) = setup_secctx(cwd, app_id, wayland_socket)?;

        if oneshot {
            let c = connect_to_display_at(cwd, Path::new(&wayland_disp))?;
            drop(sock_cleanup);
            drop(close_fd);
            run_client_oneshot(command, opts, c, socket_path, cwd)
        } else {
            let r = run_client_multi(
                command,
                opts,
                socket_path,
                &wayland_disp,
                anti_staircase,
                cwd,
            );
            drop(close_fd);
            drop(sock_cleanup);
            r
        }
    } else if oneshot || wayland_socket.is_some() {
        let wayland_fd: OwnedFd = if let Some(s) = wayland_socket {
            s
        } else {
            connect_to_wayland_display(cwd)?
        };
        run_client_oneshot(command, opts, wayland_fd, socket_path, cwd)
    } else {
        let wayland_disp = std::env::var_os("WAYLAND_DISPLAY").ok_or_else(|| tag!("The environment variable WAYLAND_DISPLAY is not set, cannot connect to Wayland server."))?;
        run_client_multi(
            command,
            opts,
            socket_path,
            &wayland_disp,
            anti_staircase,
            cwd,
        )
    }
}

 
fn setup_secctx(
    cwd: &OwnedFd,
    app_id: &str,
    wayland_socket: Option<OwnedFd>,
) -> Result<(OsString, FileCleanup, OwnedFd), String> {
    let xdg_runtime = std::env::var_os("XDG_RUNTIME_DIR");
    let mut secctx_sock_path = PathBuf::from(xdg_runtime.as_deref().unwrap_or(OsStr::new("/tmp/")));
    secctx_sock_path.push(format!("waypipe-secctx-{}", std::process::id()));

    debug!(
        "Setting up security context socket at: {:?}",
        secctx_sock_path
    );

    let (sock, sock_cleanup) = unix_socket_create_and_bind(
        &secctx_sock_path,
        cwd,
         
        true,
        true,
    )?;

    socket::listen(&sock, socket::Backlog::MAXCONN)
        .map_err(|x| tag!("Failed to listen to socket: {}", x))?;

    let wayland_conn = if let Some(s) = wayland_socket {
        s
    } else {
        connect_to_wayland_display(cwd)?
    };

    let flags = fcntl::fcntl(&wayland_conn, fcntl::FcntlArg::F_GETFL)
        .map_err(|x| tag!("Failed to get wayland socket flags: {}", x))?;
    let mut flags = fcntl::OFlag::from_bits(flags).unwrap();
    flags.remove(fcntl::OFlag::O_NONBLOCK);
    fcntl::fcntl(&wayland_conn, fcntl::FcntlArg::F_SETFL(flags))
        .map_err(|x| tag!("Failed to set wayland socket flags: {}", x))?;

    #[cfg(target_os = "linux")]
    let (close_r, close_w) = unistd::pipe2(fcntl::OFlag::O_CLOEXEC | fcntl::OFlag::O_NONBLOCK)
        .map_err(|x| tag!("Failed to create pipe: {:?}", x))?;
    #[cfg(not(target_os = "linux"))]
    let (close_r, close_w) = {
        let (r, w) = unistd::pipe().map_err(|x| tag!("Failed to create pipe: {:?}", x))?;
        use nix::fcntl;
        let _ = fcntl::fcntl(&r, fcntl::FcntlArg::F_SETFD(fcntl::FdFlag::FD_CLOEXEC));
        let _ = fcntl::fcntl(&w, fcntl::FcntlArg::F_SETFD(fcntl::FdFlag::FD_CLOEXEC));
        let _ = fcntl::fcntl(&r, fcntl::FcntlArg::F_SETFL(fcntl::OFlag::O_NONBLOCK));
        let _ = fcntl::fcntl(&w, fcntl::FcntlArg::F_SETFL(fcntl::OFlag::O_NONBLOCK));
        (r, w)
    };

    secctx::provide_secctx(wayland_conn, app_id, sock, close_r)?;

    debug!("Security context is ready");
    Ok((secctx_sock_path.into_os_string(), sock_cleanup, close_w))
}

 
fn locate_openssh_cmd_hostname(ssh_args: &[&OsStr]) -> Result<(usize, bool), String> {
     
     
    let arg_letters = b"BbcDEeFIiJLlmOopQRSWw";
    let mut dst_idx = 0;
    let mut allocates_pty = false;
     
    while dst_idx < ssh_args.len() {
        let base_arg: &[u8] = ssh_args[dst_idx].as_encoded_bytes();
        if !base_arg.starts_with(b"-") {
             
            break;
        }
        if base_arg.len() == 1 {
            return Err(tag!("Failed to parse arguments after ssh: single '-'?"));
        }
        if base_arg == [b'-', b'-'] {
             
            dst_idx += 1;
            break;
        }
         
        for i in 1..base_arg.len() {
            if arg_letters.contains(&base_arg[i]) {
                if i == base_arg.len() - 1 {
                     
                    dst_idx += 1;
                } else {
                     
                }
            } else if base_arg[i] == b't' {
                allocates_pty = true;
            } else if base_arg[i] == b'T' {
                allocates_pty = false;
            } else {
                 
            }
        }
         
        dst_idx += 1;
    }
    if dst_idx >= ssh_args.len() || ssh_args[dst_idx].as_encoded_bytes().starts_with(b"-") {
        Err(tag!("Failed to locate ssh hostname in {:?}", ssh_args))
    } else {
        Ok((dst_idx, allocates_pty))
    }
}

#[test]
fn test_ssh_parsing() {
    let x: &[(&[&str], usize, bool)] = &[
        (&["-tlfoo", "host", "command"], 2, true),
        (&["-t", "-l", "foo", "host", "command"], 3, true),
        (&["host"], 0, false),
        (&["host", "-t"], 0, false),
        (&["-T", "--", "host"], 2, false),
        (&["-T", "-t", "--", "host"], 3, true),
    ];
    for entry in x {
        let y: Vec<&std::ffi::OsStr> = entry.0.iter().map(|s| OsStr::new(*s)).collect();
        let r = locate_openssh_cmd_hostname(&y);
        println!("{:?} > {:?}", entry.0, r);
        assert!(r == Ok((entry.1, entry.2)));
    }
}

 
const VERSION_STRING_CARGO: &str = concat!(
    env!("CARGO_PKG_VERSION"),
    "\nfeatures:",
    "\n  lz4: ",
    cfg!(feature = "lz4"),
    "\n  zstd: ",
    cfg!(feature = "zstd"),
    "\n  dmabuf: ",
    cfg!(feature = "dmabuf"),
    "\n  video: ",
    cfg!(feature = "video"),
);
 
pub const VERSION_STRING: &str = match option_env!("WAYPIPE_VERSION") {
    Some(x) => x,
    None => VERSION_STRING_CARGO,
};

 
fn main() -> Result<(), String> {
    let command = Command::new(env!("CARGO_PKG_NAME"))
        .disable_help_subcommand(true)
        .subcommand_required(true)
        .help_expected(true)
        .flatten_help(false)
        .subcommand_help_heading("Modes")
        .subcommand_value_name("MODE")
        .about(
            "A proxy to remotely use Wayland protocol applications\n\
            Example: waypipe ssh user@server weston-terminal\n\
            See `man 1 waypipe` for detailed help.",
        )
        .next_line_help(false)
        .version(option_env!("WAYPIPE_VERSION").unwrap_or(VERSION_STRING));
    let command = command
        .subcommand(
            Command::new("ssh")
                .about("Wrap an ssh invocation to run Waypipe on both ends of the connection, and\nautomatically forward Wayland applications")
                .disable_help_flag(true)
                 
                .arg(Arg::new("ssh_args").num_args(0..).trailing_var_arg(true).allow_hyphen_values(true).help("Arguments for ssh"))
        ).subcommand(
            Command::new("server")
            .about("Run remotely to run a process and forward application data through a socket\nto a matching `waypipe client` instance")
            .disable_help_flag(true)
             
            .arg(Arg::new("command").num_args(0..).trailing_var_arg(true).help("Command to execute")
            .allow_hyphen_values(true) )
        ).subcommand(
            Command::new("client")
                .disable_help_flag(true)
                .about("Run locally to set up a Unix socket that `waypipe server` can connect to")
                 
        ).subcommand(
            Command::new("bench")
                .about("Estimate the best compression level used to send data, for each bandwidth")
                .disable_help_flag(true)
        ).subcommand(
            Command::new("server-conn")
                .disable_help_flag(true).hide(true)
        ).subcommand(
            Command::new("client-conn")
                .disable_help_flag(true).hide(true)
        );
    let command = command
        .arg(
            Arg::new("compress")
                .short('c')
                .long("compress")
                .value_name("comp")
                .help("Choose compression method: lz4[=#], zstd[=#], none")
                .value_parser(value_parser!(Compression))
                .default_value("lz4"),
        )
        .arg(
            Arg::new("debug")
                .short('d')
                .long("debug")
                .help("Print debug messages")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("no-gpu")
                .short('n')
                .long("no-gpu")
                .help("Block protocols using GPU memory transfers (via DMABUFs)")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("oneshot")
                .short('o')
                .long("oneshot")
                .help("Only permit one connected application")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("socket")
                .short('s')
                .long("socket")
                .value_name("path")
                .help(
                    "Set the socket path to either create or connect to.\n\
                  - server default: /tmp/waypipe-server.sock\n\
                  - client default: /tmp/waypipe-client.sock\n\
                  - ssh: sets the prefix for client and server socket paths\n\
                  - vsock: [[s]CID:]port",
                )
                 
                .value_parser(value_parser!(OsString)),
        )
        .arg(
            Arg::new("display")
                .long("display")
                .value_name("display")
                .help("server,ssh: Set the Wayland display name or path")
                .value_parser(value_parser!(PathBuf)),
        )
        .arg(
            Arg::new("drm-node")
                .long("drm-node")
                .value_name("path")
                .help("Set preferred DRM node (may be ignored in ssh/client modes)")
                .value_parser(value_parser!(PathBuf)),
        )
        .arg(
            Arg::new("remote-node")
                .long("remote-node")
                .value_name("path")
                .help("ssh: Set the preferred remote DRM node")
                .value_parser(value_parser!(PathBuf)),
        )
        .arg(
            Arg::new("ssh-bin")
                .long("ssh-bin")
                .value_name("path")
                .help("ssh: Set the ssh binary to use")
                .value_parser(value_parser!(PathBuf))
                .default_value("ssh"),
        )
        .arg(
            Arg::new("remote-bin")
                .long("remote-bin")
                .value_name("path")
                .help("ssh: Set the remote Waypipe binary to use")
                .value_parser(value_parser!(PathBuf))
                .default_value(env!("CARGO_PKG_NAME")),
        )
        .arg(
            Arg::new("remote-socket")
            .long("remote-socket")
            .value_name("path")
            .help("ssh: sets prefix of the remote server socket path")
            .value_parser(value_parser!(OsString)),
        )
        .arg(
            Arg::new("login-shell")
                .long("login-shell")
                .help("server: If server command is empty, run a login shell")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("threads")
                .long("threads")
                .help("Number of worker threads to use: 0 ⇒ hardware threads/2")
                .value_parser(value_parser!(u32))
                .default_value("0"),
        )
        .arg(
            Arg::new("title-prefix")
                .long("title-prefix")
                .value_name("str")
                .help("Prepend string to all window titles")
                .default_value(""),
        )
        .arg(
            Arg::new("unlink-socket")
                .long("unlink-socket")
                .help("server: Unlink the socket that Waypipe connects to")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("video")
                .long("video")
                .value_name("options")
                .help(
                    "Video-encode DMABUFs when possible\n\
                option format: (none|h264|vp9|av1)[,bpf=<X>]",
                )
                .default_value("none")
                .value_parser(value_parser!(VideoSetting)),
        )
        .arg(
            Arg::new("vsock")
                .long("vsock")
                .help("Connect over vsock-type socket instead of unix socket")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("secctx")
                .long("secctx")
                .value_name("str")
                .help("client,ssh: Use security-context protocol with application ID")
                .value_parser(value_parser!(String)),
        )
        .arg(
            Arg::new("xwls")
            .long("xwls")
            .value_name("display")
            .help("server,ssh: Run xwayland-satellite to handle X11 clients")
            .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("anti-staircase")
                .long("anti-staircase")
                .hide(true)
                .help("Prevent staircasing effect in terminal logs")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("test-loop")
                .long("test-loop")
                .hide(true)
                .help("Test option: act like `ssh localhost` without ssh")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("test-wire-version")
            .long("test-wire-version")
            .hide(true)
            .help("Test option: set the wire protocol version tried for `waypipe server`; must be >= 16")
            .value_parser(value_parser!(u32)),
        )
        .arg(
            Arg::new("test-store-video")
            .long("test-store-video")
            .hide(true)
            .help("Test option: client,server: save received video packets to folder")
            .value_parser(value_parser!(PathBuf)),
        )
        .arg(
            Arg::new("test-skip-vulkan")
            .long("test-skip-vulkan")
            .hide(true)
            .help("Test option: make Vulkan initialization fail and fall back to gbm backend if available")
            .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("test-no-timeline-export")
            .long("test-no-timeline-export")
            .hide(true)
            .help("Test option: assume Vulkan timeline semaphore import/export is not available")
            .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("test-no-binary-semaphore-import")
            .long("test-no-binary-semaphore-import")
            .hide(true)
            .help("Test option: assume Vulkan binary semaphore import is not available")
            .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("test-fast-bench")
                .long("test-fast-bench")
                .hide(true)
                .help("Test option: run 'bench' mode on a tiny input")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("trace")
                .long("trace")
                .action(ArgAction::SetTrue)
                .help("Test option: log all Wayland messages received and sent")
                .hide(true),
        );
    let matches = command.get_matches();

    let debug = matches.get_one::<bool>("debug").unwrap();
    let trace = matches.get_one::<bool>("trace").unwrap();

    let max_level = if *trace {
        log::LevelFilter::Trace
    } else if *debug {
        log::LevelFilter::Debug
    } else {
        log::LevelFilter::Error
    };

    let (log_color, log_label) = match matches.subcommand() {
        Some(("ssh", _)) => (1, "waypipe-client"),
        Some(("client", _)) => (1, "waypipe-client"),
        Some(("server", _)) => (2, "waypipe-server"),
        Some(("client-conn", _)) => (1, "waypipe-client"),
        Some(("server-conn", _)) => (2, "waypipe-server"),
        _ => (0, "waypipe"),
    };

    let mut anti_staircase: bool = *matches.get_one::<bool>("anti-staircase").unwrap();
    if let Some(("ssh", submatch)) = matches.subcommand() {
        let subargs = submatch.get_raw("ssh_args");
        let ssh_args: Vec<&std::ffi::OsStr> = subargs.unwrap_or_default().collect();
        let (destination_idx, allocates_pty) = locate_openssh_cmd_hostname(&ssh_args)?;
        let needs_login_shell = destination_idx == ssh_args.len() - 1;
        anti_staircase = needs_login_shell || allocates_pty;
    }
    let logger = Logger {
        max_level,
        pid: std::process::id(),
        color_output: nix::unistd::isatty(std::io::stderr()).unwrap(),
        anti_staircase,
        color: log_color,
        label: log_label,
    };

    log::set_max_level(max_level);
    log::set_boxed_logger(Box::new(logger)).unwrap();

    let mut oneshot = *matches.get_one::<bool>("oneshot").unwrap();
    let no_gpu = matches.get_one::<bool>("no-gpu").unwrap();
    let sshbin = matches.get_one::<PathBuf>("ssh-bin").unwrap();
    let remotebin = matches.get_one::<PathBuf>("remote-bin").unwrap();
    let socket_arg = matches.get_one::<OsString>("socket");
    let remote_socket_arg = matches.get_one::<OsString>("remote-socket");
    let title_prefix = matches.get_one::<String>("title-prefix").unwrap();
    let display = matches.get_one::<PathBuf>("display");
    let threads = matches.get_one::<u32>("threads").unwrap();
    let unlink = *matches.get_one::<bool>("unlink-socket").unwrap();
    let mut compression: Compression = *matches.get_one::<Compression>("compress").unwrap();
    let mut video: VideoSetting = *matches.get_one::<VideoSetting>("video").unwrap();
    let login_shell = matches.get_one::<bool>("login-shell").unwrap();
    let remote_node = matches.get_one::<PathBuf>("remote-node");
    let drm_node = matches.get_one::<PathBuf>("drm-node");
    let loop_test = matches.get_one::<bool>("test-loop").unwrap();
    let fast_bench = *matches.get_one::<bool>("test-fast-bench").unwrap();
    let use_xwls = *matches.get_one::<bool>("xwls").unwrap();
    let test_wire_version: Option<u32> = matches.get_one::<u32>("test-wire-version").copied();
    let test_store_video: Option<PathBuf> = matches.get_one::<PathBuf>("test-store-video").cloned();
    let test_skip_vulkan: bool = *matches.get_one::<bool>("test-skip-vulkan").unwrap();
    let test_no_timeline_export: bool =
        *matches.get_one::<bool>("test-no-timeline-export").unwrap();
    let test_no_binary_semaphore_import: bool = *matches
        .get_one::<bool>("test-no-binary-semaphore-import")
        .unwrap();
    let secctx = matches.get_one::<String>("secctx");
    let vsock = *matches.get_one::<bool>("vsock").unwrap();

    if !oneshot && std::env::var_os("WAYLAND_SOCKET").is_some() {
        debug!("Automatically enabling oneshot mode because WAYLAND_SOCKET is present");
        oneshot = true;
    }

    if use_xwls && oneshot {
        return Err("Waypipe cannot run xwayland-satellite (option --xwls) in oneshot mode".into());
    }

    if cfg!(not(feature = "video")) && video.format.is_some() {
        error!("Waypipe was not build with video encoding support, ignoring --video command line option.");
        video.format = None;
    }

    if cfg!(not(target_os = "linux")) && vsock {
        return Err(
            "Waypipe was built with support for VSOCK-type sockets on this platform.".into(),
        );
    }

    if vsock && socket_arg.is_none() {
        return Err("Socket must be specified with --socket when --vsock option used".into());
    }
    let (client_sock_arg, server_sock_arg) = match matches.subcommand() {
        Some(("ssh", _)) => (socket_arg, remote_socket_arg.or(socket_arg)),
        Some(("client", _)) | Some(("client-conn", _)) => (socket_arg, None),
        Some(("server", _)) | Some(("server-conn", _)) => (None, socket_arg),
        _ => (None, None),
    };

    let to_socket_spec = |s: &OsStr| -> Result<SocketSpec, String> {
        if vsock {
            Ok(SocketSpec::VSock(VSockConfig::from_str(
                s.to_str().unwrap(),
            )?))
        } else {
            Ok(SocketSpec::Unix(PathBuf::from(s)))
        }
    };

    let client_socket = if let Some(s) = client_sock_arg {
        Some(to_socket_spec(s)?)
    } else {
        None
    };

    let server_socket = if let Some(s) = server_sock_arg {
        Some(to_socket_spec(s)?)
    } else {
        None
    };

    if let Compression::Lz4(_) = compression {
        if cfg!(not(feature = "lz4")) {
            error!("Waypipe was not built with lz4 compression/decompression support, downgrading compression mode to 'none'");
            compression = Compression::None;
        }
    }
    if let Compression::Zstd(_) = compression {
        if cfg!(not(feature = "zstd")) {
            error!("Waypipe was not built with zstd compression/decompression support, downgrading compression mode to 'none'");
            compression = Compression::None;
        }
    }

    let opts = Options {
        debug: *debug,
        no_gpu: *no_gpu || cfg!(not(feature = "dmabuf")),
        compression,
        video,
        threads: *threads,
        title_prefix: (*title_prefix).clone(),
        drm_node: drm_node.cloned(),
        debug_store_video: test_store_video,
        test_skip_vulkan,
        test_no_timeline_export,
        test_no_binary_semaphore_import,
    };

     
    let cwd: OwnedFd = open_folder(&PathBuf::from("."))?;

     
    let wayland_socket = if let Some(wayl_sock) = get_wayland_socket_id()? {
        let fd = unsafe {
             
             
             
            OwnedFd::from_raw_fd(RawFd::from(wayl_sock))
        };
         
        set_cloexec(&fd, true)?;
        Some(fd)
    } else {
        None
    };

    debug!(
        "waypipe version: {}",
        VERSION_STRING.split_once('\n').unwrap().0
    );
    match matches.subcommand() {
        Some(("ssh", submatch)) => {
            debug!("Starting client+ssh main process");
            let subargs = submatch.get_raw("ssh_args");
            let ssh_args: Vec<&std::ffi::OsStr> = subargs.unwrap_or_default().collect();
            let (destination_idx, _) = locate_openssh_cmd_hostname(&ssh_args)?;
             
            let needs_login_shell = destination_idx == ssh_args.len() - 1;

            let rand_tag = get_rand_tag()?;
            let mut client_sock_path = OsString::new();
            let client_sock =
                match client_socket.unwrap_or(SocketSpec::Unix(PathBuf::from("/tmp/waypipe"))) {
                    SocketSpec::Unix(path) => {
                        client_sock_path.push(&path);
                        client_sock_path.push(OsStr::new("-client-"));
                        client_sock_path.push(OsStr::from_bytes(&rand_tag));
                        client_sock_path.push(OsStr::new(".sock"));
                        SocketSpec::Unix(PathBuf::from(client_sock_path.clone()))
                    }
                    SocketSpec::VSock(v) => SocketSpec::VSock(v),
                };
            let mut server_sock_path = OsString::new();
            match server_socket.unwrap_or(SocketSpec::Unix(PathBuf::from("/tmp/waypipe"))) {
                SocketSpec::Unix(path) => {
                    server_sock_path.push(&path);
                    server_sock_path.push(OsStr::new("-server-"));
                    server_sock_path.push(OsStr::from_bytes(&rand_tag));
                    server_sock_path.push(OsStr::new(".sock"));
                }
                SocketSpec::VSock(v) => {
                    server_sock_path = OsString::from(v.to_string());
                }
            };
            if *loop_test && !vsock {
                let client_path = PathBuf::from(&client_sock_path);
                let server_path = PathBuf::from(&server_sock_path);
                unistd::symlinkat(&client_path, fcntl::AT_FDCWD, &server_path).map_err(|x| {
                    tag!(
                        "Failed to create symlink from {:?} to {:?}: {}",
                        client_path,
                        server_path,
                        x
                    )
                })?;
            }
            let mut linkage = OsString::new();
            linkage.push(server_sock_path.clone());
            linkage.push(OsStr::new(":"));
            linkage.push(client_sock_path.clone());
            let mut wayland_display = OsString::new();
            if let Some(p) = display {
                wayland_display.push(p);
            } else {
                wayland_display.push(OsStr::new("wayland-"));
                wayland_display.push(OsStr::from_bytes(&rand_tag));
            }

            let mut ssh_cmd: Vec<&std::ffi::OsStr> = Vec::new();
            if !loop_test {
                ssh_cmd.push(OsStr::new(sshbin));
                if needs_login_shell {
                    ssh_cmd.push(OsStr::new("-t"));
                }
                if matches!(client_sock, SocketSpec::Unix(_)) {
                    ssh_cmd.push(OsStr::new("-R"));
                    ssh_cmd.push(&linkage);
                }
                ssh_cmd.extend_from_slice(&ssh_args[..=destination_idx]);
            }
            ssh_cmd.push(OsStr::new("--"));
            ssh_cmd.push(OsStr::new(remotebin));
            if opts.debug {
                ssh_cmd.push(OsStr::new("--debug"));
            }
            if oneshot {
                ssh_cmd.push(OsStr::new("--oneshot"));
            }
            if needs_login_shell {
                ssh_cmd.push(OsStr::new("--login-shell"));
            }
            if opts.no_gpu {
                ssh_cmd.push(OsStr::new("--no-gpu"));
            }
            ssh_cmd.push(OsStr::new("--unlink-socket"));
            ssh_cmd.push(OsStr::new("--threads"));
            let arg_nthreads = OsString::from(opts.threads.to_string());
            ssh_cmd.push(&arg_nthreads);

            let arg_drm_node_val;
            if let Some(r) = remote_node {
                arg_drm_node_val = r.clone().into_os_string();
                ssh_cmd.push(OsStr::new("--drm-node"));
                ssh_cmd.push(&arg_drm_node_val);
            }

            ssh_cmd.push(OsStr::new("--compress"));
            let arg_compress_val = OsString::from(compression.to_string());
            ssh_cmd.push(&arg_compress_val);
            let arg_video = OsString::from(format!("--video={}", video));
            if video.format.is_some() {
                ssh_cmd.push(&arg_video);
            }

            if matches!(client_sock, SocketSpec::VSock(_)) {
                ssh_cmd.push(OsStr::new("--vsock"));
            }
            ssh_cmd.push(OsStr::new("--socket"));
            ssh_cmd.push(&server_sock_path);
            if !oneshot {
                ssh_cmd.push(OsStr::new("--display"));
                ssh_cmd.push(&wayland_display);
            }
            if use_xwls {
                ssh_cmd.push(OsStr::new("--xwls"));
            }
            if opts.test_skip_vulkan {
                ssh_cmd.push(OsStr::new("--test-skip-vulkan"));
            }
            if opts.test_no_timeline_export {
                ssh_cmd.push(OsStr::new("--test-no-timeline-export"));
            }
            if opts.test_no_binary_semaphore_import {
                ssh_cmd.push(OsStr::new("--test-no-binary-semaphore-import"));
            }
            ssh_cmd.push(OsStr::new("server"));
            ssh_cmd.extend_from_slice(&ssh_args[destination_idx + 1..]);

            run_client(
                Some(&ssh_cmd),
                &opts,
                oneshot,
                &client_sock,
                anti_staircase,
                &cwd,
                wayland_socket,
                secctx.map(|x| x.as_str()),
            )
        }
        Some(("client", _submatch)) => {
            debug!("Starting client main process");
            let socket_path: SocketSpec = client_socket
                .unwrap_or(SocketSpec::Unix(PathBuf::from("/tmp/waypipe-client.sock")));

            run_client(
                None,
                &opts,
                oneshot,
                &socket_path,
                false,
                &cwd,
                wayland_socket,
                secctx.map(|x| x.as_str()),
            )
        }
        Some(("server", submatch)) => {
            debug!("Starting server main process");
            let subargs = submatch.get_raw("command");

            let (shell, shell_argv0) = if let Some(shell) = std::env::var_os("SHELL") {
                let bt = shell.as_bytes();
                let mut a = OsString::new();
                a.push("-");
                if let Some(idx) = bt.iter().rposition(|x| *x == b'/') {
                    let sl: &[u8] = &bt[idx + 1..];
                    a.push(OsStr::from_bytes(sl));
                } else {
                    a.push(shell.clone());
                };
                (shell.clone(), a)
            } else {
                (OsString::from("/bin/sh"), OsString::from("-sh"))
            };

            let (command, argv0): (Vec<&std::ffi::OsStr>, &std::ffi::OsStr) =
                if let Some(s) = subargs {
                    let x: Vec<_> = s.collect();
                    let y: &std::ffi::OsStr = x[0];
                    (x, y)
                } else {
                    let sv: Vec<&std::ffi::OsStr> = vec![&shell];
                    (sv, &shell_argv0)
                };
            let argv0 = if *login_shell { argv0 } else { command[0] };

            let socket_path: SocketSpec = server_socket
                .unwrap_or(SocketSpec::Unix(PathBuf::from("/tmp/waypipe-server.sock")));

            if oneshot {
                run_server_oneshot(&command, argv0, &opts, unlink, &socket_path, &cwd)
            } else {
                let display_val: PathBuf = if let Some(s) = display {
                    s.clone()
                } else {
                    let rand_tag = get_rand_tag()?;
                    let mut w = OsString::from("wayland-");
                    w.push(OsStr::from_bytes(&rand_tag));
                    PathBuf::from(w)
                };
                let display_path: PathBuf = if display_val.is_absolute() {
                    display_val.clone()
                } else {
                    let runtime_dir = std::env::var_os("XDG_RUNTIME_DIR")
                        .ok_or_else(|| tag!("Environment variable XDG_RUNTIME_DIR not present"))?;
                    PathBuf::from(runtime_dir).join(&display_val)
                };

                run_server_multi(
                    &command,
                    argv0,
                    &opts,
                    unlink,
                    &socket_path,
                    display_val.as_ref(),
                    display_path.as_ref(),
                    &cwd,
                    use_xwls,
                )
            }
        }
        Some(("bench", _)) => bench::run_benchmark(&opts, fast_bench),
        Some(("server-conn", _)) => {
            debug!("Starting server connection process");

            let env_sock = std::env::var_os("WAYPIPE_CONNECTION_FD")
                .ok_or_else(|| tag!("Connection fd not provided for server-conn mode"))?;
            let opt_fd = env_sock
                .into_string()
                .ok()
                .and_then(|x| x.parse::<i32>().ok())
                .ok_or_else(|| tag!("Failed to parse connection fd"))?;

             
             
            let wayland_fd = unsafe {
                 
                 
                 
                OwnedFd::from_raw_fd(RawFd::from(opt_fd))
            };

            set_cloexec(&wayland_fd, true)?;

            let link_fd = if let Some(s) = wayland_socket {
                 
                s
            } else {
                socket_connect(
                    &server_socket.ok_or_else(|| tag!("Socket path not provided"))?,
                    &cwd,
                    false,
                    false,
                )?
            };

            handle_server_conn(link_fd, wayland_fd, &opts, test_wire_version)
        }
        Some(("client-conn", _)) => {
            debug!("Starting client connection process");

            let env_sock = std::env::var_os("WAYPIPE_CONNECTION_FD")
                .ok_or_else(|| tag!("Connection fd not provided for client-conn mode"))?;
            let opt_fd = env_sock
                .into_string()
                .ok()
                .and_then(|x| x.parse::<i32>().ok())
                .ok_or("Failed to parse connection fd")?;
            let link_fd = unsafe {
                 
                 
                 
                OwnedFd::from_raw_fd(RawFd::from(opt_fd))
            };

            let wayland_fd = if let Some(s) = wayland_socket {
                 
                s
            } else {
                connect_to_wayland_display(&cwd)?
            };
            debug!("have read initial bytes");

            handle_client_conn(link_fd, wayland_fd, &opts)
        }
        _ => unreachable!(),
    }
}
