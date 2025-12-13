 
 
use crate::compress::*;
#[cfg(feature = "dmabuf")]
use crate::dmabuf::*;
#[cfg(feature = "gbmfallback")]
use crate::gbm::*;
use crate::kernel::*;
use crate::mirror::*;
use crate::read::*;
#[cfg(any(not(feature = "video"), not(feature = "gbmfallback")))]
use crate::stub::*;
use crate::tracking::*;
use crate::util::*;
use crate::wayland::*;
use crate::wayland_gen::*;
use crate::WAYPIPE_PROTOCOL_VERSION;
use crate::{tag, Compression, VideoFormat, VideoSetting, MIN_PROTOCOL_VERSION};

use log::{debug, error};
use nix::errno::Errno;
use nix::fcntl;
use nix::libc;
use nix::poll::{PollFd, PollFlags};
use nix::sys::{signal, socket, time, uio};
use nix::unistd;
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::io::{IoSlice, IoSliceMut};
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd, RawFd};
use std::path::PathBuf;
use std::rc::{Rc, Weak};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};

 
#[derive(PartialEq, Eq, Debug)]
enum DirectionState {
    On,
    Drain,
    Off,
}

 
#[derive(PartialEq, Eq, PartialOrd, Ord, Debug, Clone, Copy)]
#[repr(transparent)]
pub struct Rid(i32);
impl std::fmt::Display for Rid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

 
fn allocate_rid(max_local_id: &mut i32) -> Rid {
    let v: i32 = *max_local_id;
    *max_local_id = max_local_id.checked_add(max_local_id.signum()).unwrap();
    Rid(v)
}

 
pub enum DmabufDevice {
     
    Unknown,
     
    Unavailable,
     
    VulkanSetup(Arc<VulkanInstance>),
     
     
    Vulkan((Arc<VulkanInstance>, Arc<VulkanDevice>)),
     
    Gbm(Rc<GBMDevice>),
}

 
pub struct Globals {
     
    pub map: BTreeMap<Rid, Weak<RefCell<ShadowFd>>>,
     
    fresh: BTreeMap<Rid, Rc<RefCell<ShadowFd>>>,
     
    pipes: Vec<Rc<RefCell<ShadowFd>>>,

    pub on_display_side: bool,
    pub max_local_id: i32,

     
    pub dmabuf_device: DmabufDevice,

     
     
     
    pub objects: BTreeMap<ObjId, WpObject>,
     
    pub max_buffer_uid: u64,
     
    pub presentation_clock: Option<u32>,
     
    pub advertised_modifiers: BTreeMap<u32, Vec<u64>>,
     
    pub screencopy_restrictions: BTreeMap<u32, Vec<u64>>,

    pub opts: Options,

     
    wire_version: u32,
     
    has_first_message: bool,
     
    has_received_close: bool,
}

 
struct WaylandInputRing<'a> {
     
     
    data: &'a mut [u8],
    len: usize,
    fds: VecDeque<OwnedFd>,
}

 
pub const MAX_OUTGOING_FDS: usize = 8;

 
struct TransferWayland<'a> {
    data: &'a mut [u8],
    start: usize,
    len: usize,
     
     
    fds: VecDeque<Rc<RefCell<ShadowFd>>>,
}

 
 
 
struct FromChannel<'a> {
    state: DirectionState,
     
    input: ReadBuffer,
     
    next_msg: Option<ReadBufferView>,

    rid_queue: VecDeque<Rc<RefCell<ShadowFd>>>,
    output: TransferWayland<'a>,  

     
    waiting_for: Option<Rid>,

     
     
    message_counter: usize,
}

 
#[derive(Clone)]
struct DecompTaskDmabuf {
    mirror: Option<Arc<Mirror>>,
    dst: Arc<VulkanDmabuf>,
    view_row_stride: Option<u32>,
}

 
struct DecompTaskFile {
    skip_mirror: bool,  
    target: Arc<ShadowFdFileCore>,
}

 
#[derive(Clone)]
struct DecompTaskMirror {
    mirror: Arc<Mirror>,
     
    notify_on_completion: bool,
}

 
struct ApplyTaskDmabuf {
    target: DecompTaskDmabuf,
     
    orig_start: usize,
    orig_end: usize,
}

 
enum ApplyTaskTarget {
    Dmabuf(ApplyTaskDmabuf),
    Shm(DecompTaskFile),
    MirrorOnly(DecompTaskMirror),
}

 
struct ApplyTask {
     
    sequence: u64,
    remote_id: Rid,
    data: Vec<u8>,
    is_diff_type: bool,  
    ntrailing: usize,
    target: ApplyTaskTarget,
     
    region_start: usize,
    region_end: usize,
}

 
enum DecompTarget {
    Dmabuf(DecompTaskDmabuf),
    File(DecompTaskFile),
    MirrorOnly(DecompTaskMirror),
}

 
struct DecompTask {
    sequence: Option<u64>,
    msg_view: ReadBufferView,
    file_size: usize,
    compression: Compression,  
    target: DecompTarget,
}

 
struct TaskSet {
     
    construct: VecDeque<WorkTask>,
     
    waiting_for_implicit_acquire: BTreeMap<Rid, Vec<WorkTask>>,
     
    last_seqno: u64,
    decompress: VecDeque<DecompTask>,
     
    in_progress_decomp: BTreeSet<u64>,
     
    apply: BTreeMap<u64, ApplyTask>,
     
    in_progress_apply: BTreeSet<(u64, (usize, usize))>,  

     
    apply_operations: Vec<(u64, Rid)>,

     
     
    dmabuf_fill_tasks: Vec<FillDmabufTask2>,
    dmabuf_diff_tasks: Vec<DiffDmabufTask2>,

     
    stop: bool,
}

 
struct TaskSystem {
    task_notify: Condvar,
    tasks: Mutex<TaskSet>,
    wake_fd: OwnedFd,
}

 
struct ThreadCacheComp {
    lz4_c: Option<LZ4CCtx>,
    zstd_c: Option<ZstdCCtx>,
    zstd_d: Option<ZstdDCtx>,
}

 
struct ThreadCache {
     
    large: Vec<u8>,
    cmd_pool: Option<Arc<VulkanCommandPool>>,
     
    copy_ops: Vec<VulkanCopyHandle>,
     
    decode_ops: Vec<VulkanDecodeOpHandle>,  
     
    comp: ThreadCacheComp,
}

 
struct VulkanWaitThreadInner {
    pipe_w: OwnedFd,
     
    pending_read: bool,
    target_sequence_point: Option<u64>,
    stop: bool,
     
    error: Option<String>,
}

 
struct VulkanWaitThread {
    notify: Condvar,
    inner: Mutex<VulkanWaitThreadInner>,
}

 
struct VulkanWaitThreadShutdown {
    state: Arc<VulkanWaitThread>,
}

 
struct TransferQueue<'a> {
     
    protocol_data: &'a mut [u8],
    protocol_len: usize,
    protocol_header_added: bool,
    protocol_rids: Vec<Rc<RefCell<ShadowFd>>>,  

    last_ack: u32,         
    needs_new_ack: bool,   
    ack_msg_cur: [u8; 8],  
    ack_msg_nxt: [u8; 8],  
    ack_nwritten: usize,

     
     
    other_messages: Vec<Vec<u8>>,
    recv_msgs: Receiver<TaskResult>,  
    expected_recvd_msgs: u64,  

     
     
    nbytes_written: usize,
}

 
struct FromWayland<'a> {
    state: DirectionState,

    input: WaylandInputRing<'a>,  
    output: TransferQueue<'a>,
}

 
struct ExplicitSyncAcquires {
    explicit: Vec<(Arc<VulkanTimelineSemaphore>, u64)>,
    implicit: Option<VulkanBinarySemaphore>,
}

 
struct DiffTask {
    rid: Rid,
    compression: Compression,
     
    region: Option<(u32, u32)>,
     
    intervals: Vec<(u32, u32)>,
    trailing: u32,
    target: Arc<ShadowFdFileCore>,
}

 
struct DiffDmabufTask {
    rid: Rid,
    compression: Compression,
     
    region: Option<(u32, u32)>,
     
    intervals: Vec<(u32, u32)>,
    trailing: u32,

    img: Arc<VulkanDmabuf>,
    mirror: Arc<Mirror>,
    view_row_stride: Option<u32>,
    acquires: ExplicitSyncAcquires,
}

enum ReadDmabufResult {
    Vulkan(Arc<VulkanBuffer>),
    Shm(Vec<u8>),
}

 
struct DiffDmabufTask2 {
    rid: Rid,
    compression: Compression,
     
    region: Option<(u32, u32)>,
     
    intervals: Vec<(u32, u32)>,
    trailing: u32,

    wait_until: u64,
    nominal_size: usize,
    read_buf: ReadDmabufResult,
    mirror: Arc<Mirror>,
}

 
struct FillDmabufTask {
    rid: Rid,
    compression: Compression,
    region_start: u32,
    region_end: u32,
     
    mirror: Option<Arc<Mirror>>,
    dst: Arc<VulkanDmabuf>,
    view_row_stride: Option<u32>,
    acquires: ExplicitSyncAcquires,
}

 
struct FillDmabufTask2 {
    rid: Rid,
    compression: Compression,
    region_start: u32,
    region_end: u32,

    wait_until: u64,  
    mirror: Option<Arc<Mirror>>,
    read_buf: ReadDmabufResult,
}

 
struct VideoEncodeTask {
    vulk: Arc<VulkanDevice>,
    state: Arc<VideoEncodeState>,
    remote_id: Rid,
    acquires: ExplicitSyncAcquires,
}
 
struct VideoDecodeTask {
    msg: ReadBufferView,
    remote_id: Rid,
    vulk: Arc<VulkanDevice>,
    state: Arc<VideoDecodeState>,
}

 
enum WorkTask {
    FillDmabuf(FillDmabufTask),
    FillDmabuf2(FillDmabufTask2),
    Diff(DiffTask),
    DiffDmabuf(DiffDmabufTask),
    DiffDmabuf2(DiffDmabufTask2),
    Decomp(DecompTask),
    Apply(ApplyTask),
    VideoEncode(VideoEncodeTask),
    VideoDecode(VideoDecodeTask),
}

 
enum TaskOutput {
     
    MirrorApply,
     
    Msg(Vec<u8>),
     
    ApplyDone(Rid),
     
    DmabufApplyOp((u64, Rid)),
}

 
type TaskResult = Result<TaskOutput, String>;

 
#[derive(PartialEq, Eq, Debug)]
pub enum Damage {
    Everything,
    Intervals(Vec<(usize, usize)>),
}

 
pub struct ShadowFdFileCore {
    pub mem_mirror: Mirror,
    pub mapping: ExternalMapping,
}

 
pub struct ShadowFdFile {
    pub buffer_size: usize,
    pub remote_bufsize: usize,
    readonly: bool,
    pub damage: Damage,
     
     
    pub core: Option<Arc<ShadowFdFileCore>>,
    pub fd: OwnedFd,
     
    pub pending_apply_tasks: u64,
}

 
pub enum DmabufImpl {
    Vulkan(Arc<VulkanDmabuf>),
    Gbm(GBMDmabuf),
}

 
pub struct ShadowFdDmabuf {
    pub buf: DmabufImpl,
     
    mirror: Option<Arc<Mirror>>,
    pub drm_format: u32,
    pub drm_modifier: u64,
    pub damage: Damage,

     
    pub view_row_stride: Option<u32>,

     
    first_damage: bool,

     
    pub export_planes: Vec<AddDmabufPlane>,

     
    video_decode: Option<Arc<VideoDecodeState>>,
    video_encode: Option<Arc<VideoEncodeState>>,

     
    pub using_implicit_sync: bool,

     
    pub acquires: Vec<(u64, Rc<RefCell<ShadowFd>>)>,
     
    pub releases: BTreeMap<(Rid, u64), Rc<RefCell<ShadowFd>>>,

     
    pub pending_apply_tasks: u64,

     
    pub debug_wayland_id: ObjId,
}

 
enum ShadowFdPipeBuffer {
     
    ReadFromWayland((Box<[u8; 4096]>, usize)),
     
     
     
    ReadFromChannel(VecDeque<u8>),
}

 
pub struct ShadowFdPipe {
    buf: ShadowFdPipeBuffer,

    program_closed: bool,  
    channel_closed: bool,
     
    fd: OwnedFd,
     
    export_fd: Option<OwnedFd>,
}
 
pub struct ShadowFdTimeline {
    pub timeline: Arc<VulkanTimelineSemaphore>,
    export_fd: Option<OwnedFd>,
     
    pub debug_wayland_id: ObjId,
     
    pub releases: Vec<(u64, Rc<RefCell<ShadowFd>>)>,
}
 
pub enum ShadowFdVariant {
    File(ShadowFdFile),
    Pipe(ShadowFdPipe),
    Dmabuf(ShadowFdDmabuf),
    Timeline(ShadowFdTimeline),
}
 
pub struct ShadowFd {
    pub remote_id: Rid,
     
    pub only_here: bool,
    pub data: ShadowFdVariant,
}

 
fn read_from_socket(socket: &OwnedFd, buf: &mut WaylandInputRing<'_>) -> Result<bool, String> {
     
    if buf.len == buf.data.len() {
        panic!(
            "no remaining space: {} used, {} total",
            buf.len,
            buf.data.len()
        );
    }

    let mut iovs = [IoSliceMut::new(&mut buf.data[buf.len..])];
     
    let mut cmsg_fds = nix::cmsg_space!([RawFd; 32]);

    let r = socket::recvmsg::<socket::UnixAddr>(
        socket.as_raw_fd(),
        &mut iovs,
        Some(&mut cmsg_fds),
        socket::MsgFlags::empty(),
    );
    match r {
        Ok(resp) => {
            buf.len += resp.bytes;
            for msg in resp
                .cmsgs()
                .map_err(|x| tag!("Failed to get cmsgs: {:?}", x))?
            {
                match msg {
                    socket::ControlMessageOwned::ScmRights(tfds) => {
                        for f in &tfds {
                            if *f == -1 {
                                return Err(tag!("Received too many file descriptors"));
                            }
                            buf.fds.push_back(unsafe {
                                 
                                OwnedFd::from_raw_fd(*f)
                            });
                        }
                    }
                    _ => {
                        error!("Unexpected control message: {:?}, ignoring", msg);
                    }
                }
            }

            Ok(resp.bytes == 0)
        }
        Err(nix::errno::Errno::ECONNRESET) => Ok(true),
        Err(nix::errno::Errno::EINTR) | Err(nix::errno::Errno::EAGAIN) => {
             
             
             
             
            Ok(false)
        }
        Err(x) => Err(tag!("Error reading from socket: {:?}", x)),
    }
}

 
fn write_to_socket(socket: &OwnedFd, buf: &mut TransferWayland<'_>) -> Result<bool, String> {
    assert!(buf.len > 0);

     
    let mut raw_fds: [RawFd; 16] = [0; 16];
    let mut nfds_sent = 0;
    let mut i = 0;
    let mut trunc = false;
    while let Some(r) = buf.fds.get(i) {
        i += 1;
        let sfd = r.borrow();
        match sfd.data {
            ShadowFdVariant::File(ref data) => {
                if nfds_sent + 1 > 16 {
                    trunc = true;
                    break;
                }
                raw_fds[nfds_sent] = data.fd.as_raw_fd();
                nfds_sent += 1;
            }
            ShadowFdVariant::Pipe(ref data) => {
                if nfds_sent + 1 > 16 {
                    trunc = true;
                    break;
                }
                raw_fds[nfds_sent] = data.export_fd.as_ref().unwrap().as_raw_fd();
                nfds_sent += 1;
            }
            ShadowFdVariant::Timeline(ref data) => {
                if nfds_sent + 1 > 16 {
                    trunc = true;
                    break;
                }
                raw_fds[nfds_sent] = data.export_fd.as_ref().unwrap().as_raw_fd();
                nfds_sent += 1;
            }
            ShadowFdVariant::Dmabuf(ref data) => {
                if nfds_sent + data.export_planes.len() > 16 {
                    trunc = true;
                    break;
                }
                for (i, e) in data.export_planes.iter().enumerate() {
                    raw_fds[nfds_sent + i] = e.fd.as_raw_fd();
                }
                nfds_sent += data.export_planes.len();
            }
        }
    }

     
    let nbytes_sent = if trunc { 1 } else { buf.len };
    let iovs = [IoSlice::new(&buf.data[buf.start..buf.start + nbytes_sent])];
    let cmsgs = [nix::sys::socket::ControlMessage::ScmRights(
        &raw_fds[..nfds_sent],
    )];

    let r = nix::sys::socket::sendmsg::<()>(
        socket.as_raw_fd(),
        &iovs,
        if nfds_sent > 0 { &cmsgs } else { &[] },
        nix::sys::socket::MsgFlags::empty(),
        None,
    );
    match r {
        Ok(s) => {
             
            buf.start += s;
            buf.len -= s;
            if buf.len == 0 {
                buf.start = 0;
            }

             
            while nfds_sent > 0 {
                let r = buf.fds.pop_front().unwrap();

                let mut sfd = r.borrow_mut();
                match sfd.data {
                    ShadowFdVariant::File(_) => {
                        nfds_sent -= 1;
                    }
                    ShadowFdVariant::Pipe(ref mut data) => {
                        data.export_fd = None;
                        nfds_sent -= 1;
                    }
                    ShadowFdVariant::Timeline(ref mut data) => {
                        data.export_fd = None;
                        nfds_sent -= 1;
                    }
                    ShadowFdVariant::Dmabuf(ref mut data) => {
                        nfds_sent -= data.export_planes.len();
                        data.export_planes = Vec::new();
                    }
                }
            }

            Ok(false)
        }
        Err(nix::errno::Errno::EINTR) | Err(nix::errno::Errno::EAGAIN) => {
             
             
             
             
            Ok(false)
        }
        Err(nix::errno::Errno::ECONNRESET) | Err(nix::errno::Errno::EPIPE) => {
             
            Ok(true)
        }
        Err(e) => Err(tag!("Error writing to socket: {:?}", e)),
    }
}

 
fn write_to_channel(socket: &OwnedFd, queue: &mut TransferQueue) -> Result<bool, String> {
    let send_protocol = queue.protocol_len > 0 && queue.expected_recvd_msgs == 0;

    let mut net_len = if send_protocol { queue.protocol_len } else { 0 };
    for v in &queue.other_messages {
        net_len += v.len();
    }
    assert!(net_len % 4 == 0);

    debug!(
        "Write to channel: {} protocol bytes (send: {}), {} net, {} written, {} needs-ack, {} ack-written ",
        queue.protocol_len, send_protocol, net_len, queue.nbytes_written, queue.needs_new_ack, queue.ack_nwritten,
    );
    if net_len == 0 && !queue.needs_new_ack && queue.ack_nwritten == 0 {
         
        return Ok(false);
    }

     
    assert!(queue.nbytes_written < net_len || (queue.needs_new_ack || queue.ack_nwritten > 0));

    let mut nwritten = queue.nbytes_written;
    let mut iovs: Vec<IoSlice> = Vec::new();
    if queue.ack_nwritten > 0 {
        iovs.push(IoSlice::new(&queue.ack_msg_cur[queue.ack_nwritten..]));
    }
    let opt_whole_ack: Option<&[u8]> = if queue.needs_new_ack {
        Some(if queue.ack_nwritten > 0 {
            &queue.ack_msg_nxt
        } else {
            &queue.ack_msg_cur
        })
    } else {
        None
    };
    let mut injected_whole_ack = false;
    let mut first_partial_len = 0;

    for v in &queue.other_messages {
        if v.len() <= nwritten {
            nwritten -= v.len();
        } else {
            if let Some(ackmsg) = opt_whole_ack {
                 
                if !injected_whole_ack && nwritten == 0 {
                    iovs.push(IoSlice::new(ackmsg));
                    injected_whole_ack = true;
                }
            }
            if nwritten > 0 {
                first_partial_len = v.len() - nwritten;
            }

            iovs.push(IoSlice::new(&v[nwritten..]));
            nwritten = 0;
        }
    }

    if let Some(ackmsg) = opt_whole_ack {
         
        if !injected_whole_ack && nwritten == 0 {
            iovs.push(IoSlice::new(ackmsg));
            injected_whole_ack = true;
        }
    }

    if nwritten < queue.protocol_len && send_protocol {
        iovs.push(IoSlice::new(
            &queue.protocol_data[nwritten..queue.protocol_len],
        ));
    }

    let r = uio::writev(socket, &iovs);
    match r {
        Ok(mut len) => {
            debug!("Wrote: {} bytes", len);
             
            if queue.ack_nwritten > 0 {
                let absorbed = std::cmp::min(8 - queue.ack_nwritten, len);
                debug!("Absorbed {} bytes from partial ack message", absorbed);
                queue.ack_nwritten += absorbed;
                len -= absorbed;
                if queue.ack_nwritten == 8 {
                     
                    queue.ack_nwritten = 0;
                    queue.ack_msg_cur = queue.ack_msg_nxt;
                }
            }

             
            if first_partial_len > 0 {
                let absorbed = std::cmp::min(first_partial_len, len);
                debug!("Absorbed {} bytes from first partial message", absorbed);
                queue.nbytes_written += absorbed;
                len -= absorbed;
            }

             
            if injected_whole_ack {
                let absorbed = std::cmp::min(8, len);
                debug!("Absorbed {} bytes from whole ack message", absorbed);
                len -= absorbed;
                if absorbed < 8 {
                     
                    queue.ack_nwritten = absorbed;
                }

                 
                queue.needs_new_ack = false;
            }

             
            debug!("Absorbed {} bytes from the rest", len);
            queue.nbytes_written += len;
            if queue.nbytes_written == net_len {
                debug!("Completed write to channel of total length: {}", net_len);
                if send_protocol {
                     
                    queue.protocol_len = 0;
                    queue.protocol_header_added = false;
                }
                 
                queue.other_messages.clear();
                queue.nbytes_written = 0;
            }
            Ok(false)
        }
        Err(nix::errno::Errno::EPIPE) | Err(nix::errno::Errno::ECONNRESET) => Ok(true),
        Err(nix::errno::Errno::EINTR) | Err(nix::errno::Errno::EAGAIN) => Ok(false),
        Err(x) => Err(tag!("Error writing to socket: {:?}", x)),
    }
}

 
fn read_from_channel(socket: &OwnedFd, from_chan: &mut FromChannel) -> Result<bool, String> {
    let eof = from_chan.input.read_more(socket)?;

    if from_chan.next_msg.is_none() {
        from_chan.next_msg = from_chan.input.pop_next_msg();
    }

    Ok(eof)
}

 
pub fn try_setup_dmabuf_instance_light(
    opts: &Options,
    device: Option<u64>,
) -> Result<DmabufDevice, String> {
    if !opts.test_skip_vulkan {
        if let Some(instance) = setup_vulkan_instance(
            opts.debug,
            &opts.video,
            opts.test_no_timeline_export,
            opts.test_no_binary_semaphore_import,
        )? {
            if instance.has_device(device) {
                return Ok(DmabufDevice::VulkanSetup(instance));
            }
        }
    }
     
    if let Some(dev) = setup_gbm_device(device)? {
        return Ok(DmabufDevice::Gbm(dev));
    }
    Ok(DmabufDevice::Unavailable)
}

 
pub fn try_setup_dmabuf_instance_full(
    opts: &Options,
    device: Option<u64>,
) -> Result<DmabufDevice, String> {
    if !opts.test_skip_vulkan {
        if let Some(instance) = setup_vulkan_instance(
            opts.debug,
            &opts.video,
            opts.test_no_timeline_export,
            opts.test_no_binary_semaphore_import,
        )? {
            if let Some(device) = setup_vulkan_device(&instance, device, &opts.video, opts.debug)? {
                return Ok(DmabufDevice::Vulkan((instance, device)));
            }
        };
    }
     
    if let Some(dev) = setup_gbm_device(device)? {
        return Ok(DmabufDevice::Gbm(dev));
    }
    Ok(DmabufDevice::Unavailable)
}
 
pub fn complete_dmabuf_setup(
    opts: &Options,
    device: Option<u64>,
    dmabuf_dev: &mut DmabufDevice,
) -> Result<(), String> {
    if matches!(dmabuf_dev, DmabufDevice::VulkanSetup(_)) {
        let mut tmp = DmabufDevice::Unknown;
        std::mem::swap(dmabuf_dev, &mut tmp);
        let DmabufDevice::VulkanSetup(instance) = tmp else {
            unreachable!();
        };
        let device = setup_vulkan_device(&instance, device, &opts.video, opts.debug)?
            .expect("Vulkan device existence should already have been checked");
        *dmabuf_dev = DmabufDevice::Vulkan((instance, device));
    }
    Ok(())
}
pub fn dmabuf_dev_supports_format(dmabuf_dev: &DmabufDevice, format: u32, modifier: u64) -> bool {
    match dmabuf_dev {
        DmabufDevice::Unknown | DmabufDevice::Unavailable | DmabufDevice::VulkanSetup(_) => {
            unreachable!()
        }

        DmabufDevice::Vulkan((_, vulk)) => vulk.supports_format(format, modifier),
        DmabufDevice::Gbm(gbm) => gbm_supported_modifiers(gbm, format).contains(&modifier),
    }
}
pub fn dmabuf_dev_modifier_list(dmabuf_dev: &DmabufDevice, format: u32) -> &[u64] {
    match dmabuf_dev {
        DmabufDevice::Unknown | DmabufDevice::Unavailable | DmabufDevice::VulkanSetup(_) => {
            unreachable!()
        }

        DmabufDevice::Vulkan((_, vulk)) => vulk.get_supported_modifiers(format),
        DmabufDevice::Gbm(gbm) => gbm_supported_modifiers(gbm, format),
    }
}
pub fn dmabuf_dev_get_id(dmabuf_dev: &DmabufDevice) -> u64 {
    match dmabuf_dev {
        DmabufDevice::Unknown | DmabufDevice::Unavailable | DmabufDevice::VulkanSetup(_) => {
            unreachable!()
        }
        DmabufDevice::Vulkan((_, vulk)) => vulk.get_device(),
        DmabufDevice::Gbm(gbm) => gbm_get_device_id(gbm),
    }
}
 
pub fn dmabuf_post_apply_task_operations(data: &mut ShadowFdDmabuf) -> Result<(), String> {
    if let DmabufImpl::Gbm(ref mut buf) = data.buf {
         
        let len = buf.nominal_size(data.view_row_stride);
        let src = data
            .mirror
            .as_ref()
            .unwrap()
            .get_mut_range(0..len)
            .ok_or_else(|| tag!("Failed to get entire mirror, to apply changes to DMABUF"))?;
        buf.copy_onto_dmabuf(data.view_row_stride, src.data)?;
    }
    Ok(())
}

 
pub fn dmabuf_setup_implicit_sync(
    data: &ShadowFdDmabuf,
) -> Result<(Option<VulkanSyncFile>, bool), String> {
    if data.using_implicit_sync {
         
        if let DmabufImpl::Vulkan(ref vulk_buf) = data.buf {
            if !vulk_buf.vulk.supports_binary_semaphore_import() {
                Ok((None, true))
            } else if let Some(f) = vulk_buf.export_sync_file()? {
                Ok((Some(f), false))
            } else {
                Ok((None, true))
            }
        } else {
            Ok((None, false))
        }
    } else {
        Ok((None, false))
    }
}

 
pub fn translate_shm_fd(
    fd: OwnedFd,
    size_lb: usize,
    map: &mut BTreeMap<Rid, Weak<RefCell<ShadowFd>>>,
    max_local_id: &mut i32,
    default_damage: bool,
    readonce: bool,
    local_only: bool,
) -> Result<Rc<RefCell<ShadowFd>>, String> {
    let remote_id = allocate_rid(max_local_id);

    let mapping: ExternalMapping = ExternalMapping::new(&fd, size_lb, readonce)?;

    let mir_size = if readonce { 0 } else { size_lb };
    let core = Some(Arc::new(ShadowFdFileCore {
         
        mem_mirror: Mirror::new(mir_size, !readonce)?,
        mapping,
    }));

    let sfd = Rc::new(RefCell::new(ShadowFd {
        remote_id,
        only_here: !local_only,
        data: ShadowFdVariant::File(ShadowFdFile {
            fd,
            buffer_size: size_lb,
            readonly: readonce,
            remote_bufsize: 0,
            damage: if default_damage {
                Damage::Everything
            } else {
                Damage::Intervals(Vec::new())
            },
            pending_apply_tasks: 0,
            core,
        }),
    }));

    map.insert(remote_id, Rc::downgrade(&sfd));

    Ok(sfd)
}

 
pub fn translate_dmabuf_fd(
    width: u32,
    height: u32,
    drm_format: u32,
    planes: Vec<AddDmabufPlane>,
    opts: &Options,
    device: &DmabufDevice,
    max_local_id: &mut i32,
    map: &mut BTreeMap<Rid, Weak<RefCell<ShadowFd>>>,
    wayland_id: ObjId,
) -> Result<Rc<RefCell<ShadowFd>>, String> {
    let remote_id = allocate_rid(max_local_id);

    debug!("Translating dmabuf fd");

    let drm_modifier = planes[0].modifier;
    let (buf, video_encode) = match device {
        DmabufDevice::Unknown | DmabufDevice::Unavailable | DmabufDevice::VulkanSetup(_) => {
            unreachable!()
        }
        DmabufDevice::Vulkan((_, vulk)) => {
            let mut use_video = false;
            if let Some(ref f) = opts.video.format {
                if supports_video_format(vulk, *f, drm_format, width, height) {
                    use_video = true;
                }
            }
            if use_video {
                if vulk
                    .can_import_image(drm_format, width, height, &planes, true)
                    .is_err()
                {
                    use_video = false;
                }
            }
            if !use_video {
                if let Err(e) = vulk.can_import_image(drm_format, width, height, &planes, false) {
                    return Err(tag!("Cannot import DMABUF, format/size/modifier combination: {:x}, {}x{}, {:x}, has: {}", drm_format, width, height, planes[0].modifier, e));
                }
            }

            let buf = vulkan_import_dmabuf(vulk, planes, width, height, drm_format, use_video)?;

            let video_encode = if use_video {
                if let Some(f) = opts.video.format {
                    Some(Arc::new(setup_video_encode(
                        &buf,
                        f,
                        opts.video.bits_per_frame,
                    )?))
                } else {
                    None
                }
            } else {
                None
            };

            (DmabufImpl::Vulkan(buf), video_encode)
        }
        DmabufDevice::Gbm(gbm) => (
            DmabufImpl::Gbm(gbm_import_dmabuf(gbm, planes, width, height, drm_format)?),
            None,
        ),
    };

    let sfd = Rc::new(RefCell::new(ShadowFd {
        remote_id,
        only_here: true,
        data: ShadowFdVariant::Dmabuf(ShadowFdDmabuf {
            buf,
            mirror: None,
            drm_format,
            drm_modifier,
            first_damage: true,
            export_planes: Vec::new(),
            damage: Damage::Intervals(Vec::new()),
            video_decode: None,
            video_encode,
            acquires: Vec::new(),
            releases: BTreeMap::new(),
            pending_apply_tasks: 0,
            using_implicit_sync: false,
             
            view_row_stride: None,
            debug_wayland_id: wayland_id,
        }),
    }));

    map.insert(remote_id, Rc::downgrade(&sfd));

    Ok(sfd)
}

 
pub fn translate_timeline(
    fd: OwnedFd,
    glob: &mut Globals,
    object_id: ObjId,
) -> Result<Rc<RefCell<ShadowFd>>, String> {
    let remote_id = allocate_rid(&mut glob.max_local_id);

    debug!("Translating timeline semaphore fd");

    let DmabufDevice::Vulkan((_, ref vulk)) = glob.dmabuf_device else {
        unreachable!();
    };

    let tm = vulkan_import_timeline(vulk, fd)?;

    let sfd = Rc::new(RefCell::new(ShadowFd {
        remote_id,
        only_here: true,
        data: ShadowFdVariant::Timeline(ShadowFdTimeline {
            timeline: tm,
            export_fd: None,
            debug_wayland_id: object_id,
            releases: Vec::new(),
        }),
    }));

    glob.map.insert(remote_id, Rc::downgrade(&sfd));

    Ok(sfd)
}

 
pub fn translate_pipe_fd(
    fd: OwnedFd,
    glob: &mut Globals,
    reading_from_channel: bool,
) -> Result<Rc<RefCell<ShadowFd>>, String> {
    let remote_id = allocate_rid(&mut glob.max_local_id);

    debug!(
        "Translating pipe fd: reading from channel: {}",
        reading_from_channel
    );

    let sfd = Rc::new(RefCell::new(ShadowFd {
        remote_id,
        only_here: true,
        data: ShadowFdVariant::Pipe(ShadowFdPipe {
            fd,
            export_fd: None,
            buf: if reading_from_channel {
                ShadowFdPipeBuffer::ReadFromChannel(VecDeque::new())
            } else {
                ShadowFdPipeBuffer::ReadFromWayland((Box::new([0; 4096]), 0))
            },
            program_closed: false,
            channel_closed: false,
        }),
    }));

    glob.map.insert(remote_id, Rc::downgrade(&sfd));
    glob.pipes.push(sfd.clone());

    Ok(sfd)
}

 
pub fn update_core_for_new_size(
    fd: &OwnedFd,
    size: usize,
    core: &mut Option<Arc<ShadowFdFileCore>>,
) -> Result<(), String> {
    let mapping: ExternalMapping = ExternalMapping::new(fd, size, false)?;

     
    let mut alt: Option<Arc<ShadowFdFileCore>> = None;
    std::mem::swap(core, &mut alt);

    let mut inner = Arc::<ShadowFdFileCore>::into_inner(alt.unwrap())
        .ok_or("ExtendFile invoked without exclusive access to ShadowFd")?;

    inner.mem_mirror.extend(size)?;

    let mut new: Option<Arc<ShadowFdFileCore>> = Some(Arc::new(ShadowFdFileCore {
        mem_mirror: inner.mem_mirror,
        mapping,
    }));
    std::mem::swap(core, &mut new);

    Ok(())
}

 
fn get_sfd(
    map: &BTreeMap<Rid, Weak<RefCell<ShadowFd>>>,
    rid: Rid,
) -> Option<Rc<RefCell<ShadowFd>>> {
    map.get(&rid)?.upgrade()
}

 
fn process_sfd_msg(
    typ: WmsgType,
    length: usize,
    msg_view: ReadBufferView,
    glob: &mut Globals,
    tasksys: &TaskSystem,
) -> Result<(), String> {
    let msg = &msg_view.get()[..length];

    if msg.len() < 8 {
        return Err(tag!(
            "message to shadowfd is too short, {} bytes",
            msg.len()
        ));
    }

    let remote_id = Rid(i32::from_le_bytes(msg[4..8].try_into().unwrap()));
    match typ {
        WmsgType::OpenFile => {
             

             
            let size = i32::from_le_bytes(msg[8..12].try_into().unwrap());

            let local_fd = crate::util::create_anon_file()
            .map_err(|x| tag!("Failed to create memfd: {:?}", x))?;

            unistd::ftruncate(&local_fd, size as libc::off_t)
                .map_err(|x| tag!("Failed to resize memfd: {:?}", x))?;

             
             
            let mirror = Mirror::new(size as usize, true)?;

             
            let mapping: ExternalMapping = ExternalMapping::new(&local_fd, size as usize, false)?;
            let core = Some(Arc::new(ShadowFdFileCore {
                mem_mirror: mirror,
                mapping,
            }));

            let sfd = Rc::new(RefCell::new(ShadowFd {
                remote_id,
                only_here: false,
                data: ShadowFdVariant::File(ShadowFdFile {
                    fd: local_fd,
                    buffer_size: size as usize,
                    readonly: false,
                    remote_bufsize: size as usize,
                    damage: Damage::Intervals(Vec::new()),
                    core,
                    pending_apply_tasks: 0,
                }),
            }));

            glob.map.insert(remote_id, Rc::downgrade(&sfd));
            glob.fresh.insert(remote_id, sfd);

            Ok(())
        }
        WmsgType::ExtendFile => {
            let size = i32::from_le_bytes(msg[8..12].try_into().unwrap());
            let new_size: usize = size
                .try_into()
                .map_err(|_| tag!("Invalid size: {}", size))?;

            let sfd_handle = get_sfd(&glob.map, remote_id).ok_or("RID does not exist")?;
            let mut sfd = sfd_handle.borrow_mut();
            let ShadowFdVariant::File(data) = &mut sfd.data else {
                return Err(tag!("Applying ExtendFile to non-File ShadowFd"));
            };

            if data.buffer_size > new_size {
                return Err(tag!("ExtendFile would shrink size"));
            } else if data.buffer_size == new_size {
                return Ok(());  
            }

            if data.readonly {
                return Err(tag!("Tried to apply ExtendFile to read-only file"));
            }

            debug!(
                "Extending file at RID={} from {} to {} bytes",
                remote_id, data.buffer_size, new_size
            );
            unistd::ftruncate(&data.fd, new_size.try_into().unwrap())
                .map_err(|x| tag!("Failed to resize file: {:?}", x))?;

            data.buffer_size = new_size;
            data.remote_bufsize = new_size;
            update_core_for_new_size(&data.fd, new_size, &mut data.core)?;

            Ok(())
        }
        WmsgType::BufferDiff => {
            let sfd_handle = get_sfd(&glob.map, remote_id).ok_or("RID not in map")?;
            let mut sfd = sfd_handle.borrow_mut();

            match &mut sfd.data {
                ShadowFdVariant::Dmabuf(data) => {
                    match data.buf {
                        DmabufImpl::Vulkan(ref buf) => {
                             
                             

                            data.pending_apply_tasks += 1;
                            let t = DecompTask {
                                sequence: None,
                                msg_view,
                                file_size: buf.nominal_size(data.view_row_stride),
                                compression: glob.opts.compression,
                                target: DecompTarget::Dmabuf(DecompTaskDmabuf {
                                    dst: buf.clone(),
                                    view_row_stride: data.view_row_stride,
                                    mirror: data.mirror.clone(),
                                }),
                            };
                            tasksys.tasks.lock().unwrap().decompress.push_back(t);
                            tasksys.task_notify.notify_one();
                        }
                        DmabufImpl::Gbm(ref buf) => {
                            data.pending_apply_tasks += 1;
                            let nominal_size = buf.nominal_size(data.view_row_stride);
                            if data.mirror.is_none() {
                                data.mirror = Some(Arc::new(Mirror::new(nominal_size, false)?));
                            }
                            let t = DecompTask {
                                sequence: None,
                                msg_view,
                                file_size: nominal_size,
                                compression: glob.opts.compression,
                                target: DecompTarget::MirrorOnly(DecompTaskMirror {
                                    mirror: data.mirror.as_ref().unwrap().clone(),
                                    notify_on_completion: true,
                                }),
                            };
                            tasksys.tasks.lock().unwrap().decompress.push_back(t);
                            tasksys.task_notify.notify_one();
                        }
                    }

                    Ok(())
                }
                ShadowFdVariant::File(data) => {
                    if data.readonly {
                        return Err(tag!("Applying BufferDiff to readonly ShadowFd"));
                    }

                    data.pending_apply_tasks += 1;
                    let t = DecompTask {
                        sequence: None,
                        msg_view,
                        file_size: data.buffer_size,
                        compression: glob.opts.compression,
                        target: DecompTarget::File(DecompTaskFile {
                            skip_mirror: data.readonly,
                            target: data.core.as_ref().unwrap().clone(),
                        }),
                    };
                    tasksys.tasks.lock().unwrap().decompress.push_back(t);
                    tasksys.task_notify.notify_one();
                    Ok(())
                }
                _ => Err(tag!("Applying BufferDiff to non-File ShadowFd")),
            }
        }

        WmsgType::BufferFill => {
            let sfd_handle = get_sfd(&glob.map, remote_id).ok_or("RID not in map")?;
            let mut sfd = sfd_handle.borrow_mut();

            match &mut sfd.data {
                ShadowFdVariant::Dmabuf(data) => {
                    match data.buf {
                        DmabufImpl::Vulkan(ref buf) => {
                            data.pending_apply_tasks += 1;
                            let t = DecompTask {
                                sequence: None,
                                msg_view,
                                compression: glob.opts.compression,
                                file_size: buf.nominal_size(data.view_row_stride),
                                target: DecompTarget::Dmabuf(DecompTaskDmabuf {
                                    dst: buf.clone(),
                                    view_row_stride: data.view_row_stride,
                                    mirror: data.mirror.clone(),
                                }),
                            };
                            tasksys.tasks.lock().unwrap().decompress.push_back(t);
                            tasksys.task_notify.notify_one();
                        }
                        DmabufImpl::Gbm(ref buf) => {
                            data.pending_apply_tasks += 1;
                            let nominal_size = buf.nominal_size(data.view_row_stride);
                            if data.mirror.is_none() {
                                data.mirror = Some(Arc::new(Mirror::new(nominal_size, false)?));
                            }
                            let t = DecompTask {
                                sequence: None,
                                msg_view,
                                compression: glob.opts.compression,
                                file_size: nominal_size,
                                target: DecompTarget::MirrorOnly(DecompTaskMirror {
                                    mirror: data.mirror.as_ref().unwrap().clone(),
                                    notify_on_completion: true,
                                }),
                            };
                            tasksys.tasks.lock().unwrap().decompress.push_back(t);
                            tasksys.task_notify.notify_one();
                             
                        }
                    }

                    Ok(())
                }
                ShadowFdVariant::File(data) => {
                    data.pending_apply_tasks += 1;
                    let t = DecompTask {
                        sequence: None,
                        msg_view,
                        file_size: data.buffer_size,
                        target: DecompTarget::File(DecompTaskFile {
                            skip_mirror: data.readonly,
                            target: data.core.as_ref().unwrap().clone(),
                        }),
                        compression: glob.opts.compression,
                    };
                    tasksys.tasks.lock().unwrap().decompress.push_back(t);
                    tasksys.task_notify.notify_one();
                    Ok(())
                }
                _ => Err(tag!("Applying BufferFill to non-File ShadowFd")),
            }
        }

        WmsgType::OpenDMABUF => {
             
            let width = u32::from_le_bytes(msg[12..16].try_into().unwrap());
            let height = u32::from_le_bytes(msg[16..20].try_into().unwrap());
            let drm_format = u32::from_le_bytes(msg[20..24].try_into().unwrap());
             

             
            let view_row_stride = Some(dmabuf_slice_get_first_stride(
                msg[12..76].try_into().unwrap(),
            ));

             
            let modifier_list = glob.screencopy_restrictions.get(&drm_format).or_else(|| glob.advertised_modifiers.get(&drm_format)).map(|x| &x[..]).unwrap_or_else(|| {
                debug!("No advertised modifiers for {}, falling back to arbitrary supported modifier", drm_format);
                dmabuf_dev_modifier_list(&glob.dmabuf_device, drm_format)
            }
            );

            let (buf, nom_size, add_planes) = match glob.dmabuf_device {
                DmabufDevice::Unknown
                | DmabufDevice::Unavailable
                | DmabufDevice::VulkanSetup(_) => {
                    return Err(tag!("Received OpenDMABUF too early"));
                }
                DmabufDevice::Vulkan((_, ref vulk)) => {
                    let (buf, add_planes) = vulkan_create_dmabuf(
                        vulk,
                        width,
                        height,
                        drm_format,
                        modifier_list,
                         
                         
                        false,
                    )?;

                    let nom_size = buf.nominal_size(view_row_stride);
                    (DmabufImpl::Vulkan(buf), nom_size, add_planes)
                }
                DmabufDevice::Gbm(ref gbm) => {
                    let mods = gbm_supported_modifiers(gbm, drm_format);
                    let (buf, add_planes) =
                        gbm_create_dmabuf(gbm, width, height, drm_format, mods)?;
                    let nom_size = buf.nominal_size(view_row_stride);
                    (DmabufImpl::Gbm(buf), nom_size, add_planes)
                }
            };

             
             
            let mirror = Some(Arc::new(Mirror::new(nom_size, false)?));
            let drm_modifier = add_planes[0].modifier;

            let sfd = Rc::new(RefCell::new(ShadowFd {
                remote_id,
                only_here: false,
                data: ShadowFdVariant::Dmabuf(ShadowFdDmabuf {
                    buf,
                    mirror,
                    drm_format,
                    drm_modifier,
                    view_row_stride,
                    first_damage: true,
                    damage: Damage::Intervals(Vec::new()),
                    export_planes: add_planes,
                    video_decode: None,
                    video_encode: None,
                    using_implicit_sync: false,
                    acquires: Vec::new(),
                    releases: BTreeMap::new(),
                    pending_apply_tasks: 0,
                    debug_wayland_id: ObjId(0),
                }),
            }));

            glob.map.insert(remote_id, Rc::downgrade(&sfd));
            glob.fresh.insert(remote_id, sfd);

            Ok(())
        }

        WmsgType::OpenDMAVidDstV2 => {
             
            let vid_flags = u32::from_le_bytes(msg[12..16].try_into().unwrap());

            let width = u32::from_le_bytes(msg[16..20].try_into().unwrap());
            let height = u32::from_le_bytes(msg[20..24].try_into().unwrap());
            let drm_format = u32::from_le_bytes(msg[24..28].try_into().unwrap());
             

             
             
            let view_row_stride = Some(dmabuf_slice_get_first_stride(
                msg[12..76].try_into().unwrap(),
            ));

            const H264: u32 = VideoFormat::H264 as u32;
            const VP9: u32 = VideoFormat::VP9 as u32;
            const AV1: u32 = VideoFormat::AV1 as u32;
            let vid_type: VideoFormat = match vid_flags & 0xff {
                H264 => VideoFormat::H264,
                VP9 => VideoFormat::VP9,
                AV1 => VideoFormat::AV1,
                _ => {
                    return Err(tag!("Unidentified video format {}", vid_flags & 0xff));
                }
            };

            let DmabufDevice::Vulkan((_, ref vulk)) = glob.dmabuf_device else {
                return Err(tag!(
                    "Received OpenDMAVidDstV2 before Vulkan device was set up"
                ));
            };
            if !supports_video_format(vulk, vid_type, drm_format, width, height) {
                return Err(tag!(
                    "Video format {:?} at {}x{} is not supported",
                    vid_type,
                    width,
                    height
                ));
            }

            let modifier_list = glob.screencopy_restrictions.get(&drm_format).or_else(|| glob.advertised_modifiers.get(&drm_format)).map(|x| &x[..]).unwrap_or_else(|| {
                debug!("No advertised modifiers for {}, falling back to arbitrary supported modifier", drm_format);
                dmabuf_dev_modifier_list(&glob.dmabuf_device, drm_format)
            }
            );

            let (buf, add_planes) = vulkan_create_dmabuf(
                vulk,
                width,
                height,
                drm_format,
                modifier_list,
                 
                 
                true,
            )?;
            let drm_modifier = add_planes[0].modifier;

            let video_decode_state = setup_video_decode(&buf, vid_type)?;

            let sfd = Rc::new(RefCell::new(ShadowFd {
                remote_id,
                only_here: false,
                data: ShadowFdVariant::Dmabuf(ShadowFdDmabuf {
                    buf: DmabufImpl::Vulkan(buf),
                    mirror: None,
                    drm_format,
                    drm_modifier,
                    view_row_stride,
                    first_damage: true,
                    damage: Damage::Intervals(Vec::new()),
                    export_planes: add_planes,
                    video_decode: Some(Arc::new(video_decode_state)),
                    video_encode: None,
                    using_implicit_sync: false,
                    acquires: Vec::new(),
                    releases: BTreeMap::new(),
                    pending_apply_tasks: 0,
                    debug_wayland_id: ObjId(0),
                }),
            }));

            glob.map.insert(remote_id, Rc::downgrade(&sfd));
            glob.fresh.insert(remote_id, sfd);

            Ok(())
        }
        WmsgType::SendDMAVidPacket => {
            let sfd_handle = get_sfd(&glob.map, remote_id).ok_or("RID not in map")?;
            let mut sfd = sfd_handle.borrow_mut();
            let ShadowFdVariant::Dmabuf(data) = &mut sfd.data else {
                return Err(tag!("Applying DMAVid to non-DMABUF ShadowFd"));
            };
            let Some(ref video_decode) = data.video_decode else {
                return Err(tag!(
                    "Applying DMAVid to non-DMABUF video-decode-type ShadowFd"
                ));
            };

            if let Some(ref folder) = glob.opts.debug_store_video {
                 
                let mut full_path = folder.clone();
                let filename = format!("packets-{}-{}", unistd::getpid(), remote_id);
                full_path.push(std::ffi::OsStr::new(&filename));
                let mut logfile = std::fs::File::options()
                    .create(true)
                    .append(true)
                    .open(full_path)
                    .unwrap();
                let packet = &msg[8..];
                use std::io::Write;
                logfile.write_all(packet).unwrap();
            }

            let DmabufDevice::Vulkan((_, ref vulk)) = glob.dmabuf_device else {
                unreachable!();
            };

            let task = VideoDecodeTask {
                msg: msg_view,
                remote_id,
                vulk: vulk.clone(),
                state: video_decode.clone(),
            };
            tasksys
                .tasks
                .lock()
                .unwrap()
                .construct
                .push_back(WorkTask::VideoDecode(task));
            tasksys.task_notify.notify_one();

            data.pending_apply_tasks += 1;

            Ok(())
        }

        WmsgType::OpenTimeline => {
            let DmabufDevice::Vulkan((_, ref vulk)) = glob.dmabuf_device else {
                return Err(tag!(
                    "Received OpenTimeline before Vulkan device was set up"
                ));
            };

            let start_pt = u64::from_le_bytes(msg[8..16].try_into().unwrap());
            let (timeline, fd) = vulkan_create_timeline(vulk, start_pt)?;

            let sfd = Rc::new(RefCell::new(ShadowFd {
                remote_id,
                only_here: false,
                data: ShadowFdVariant::Timeline(ShadowFdTimeline {
                    timeline,
                    export_fd: Some(fd),
                    debug_wayland_id: ObjId(0),
                    releases: Vec::new(),
                }),
            }));

            glob.map.insert(remote_id, Rc::downgrade(&sfd));
            glob.fresh.insert(remote_id, sfd);

            Ok(())
        }
        WmsgType::SignalTimeline => {
            let sfd_handle = get_sfd(&glob.map, remote_id).ok_or("RID not in map")?;
            let mut sfd = sfd_handle.borrow_mut();
            let ShadowFdVariant::Timeline(ref mut data) = sfd.data else {
                return Err(tag!("Applying SignalTimeline to non-timeline ShadowFd"));
            };
            if glob.on_display_side {
                return Err(tag!("Received SignalTimeline on compositor side"));
            }

            let pt = u64::from_le_bytes(msg[8..16].try_into().unwrap());

            prune_releases(&mut data.releases, pt, remote_id);
            data.timeline.signal_timeline_pt(pt)
        }

        WmsgType::OpenIRPipe | WmsgType::OpenIWPipe => {
            let (pipe_r, pipe_w) =
                crate::util::create_pipe()
                    .map_err(|x| tag!("Failed to create pipe: {:?}", x))?;

            let (local_pipe, export_pipe) = if typ == WmsgType::OpenIRPipe {
                (pipe_r, pipe_w)
            } else {
                (pipe_w, pipe_r)
            };

            let sfd = Rc::new(RefCell::new(ShadowFd {
                remote_id,
                only_here: false,
                data: ShadowFdVariant::Pipe(ShadowFdPipe {
                    fd: local_pipe,
                    export_fd: Some(export_pipe),
                    buf: if typ == WmsgType::OpenIWPipe {
                        ShadowFdPipeBuffer::ReadFromChannel(VecDeque::new())
                    } else {
                        ShadowFdPipeBuffer::ReadFromWayland((Box::new([0; 4096]), 0))
                    },
                    program_closed: false,
                    channel_closed: false,
                }),
            }));

            glob.map.insert(remote_id, Rc::downgrade(&sfd));
            glob.pipes.push(sfd.clone());
            glob.fresh.insert(remote_id, sfd);

            Ok(())
        }

        WmsgType::PipeTransfer => {
            let Some(sfd_handle) = get_sfd(&glob.map, remote_id) else {
                debug!("PipeTransfer message directed to RID={}, which does not exist or was already closed and deleted. Ignoring message, because it may have been sent before the remote was notified.",
                       remote_id);
                return Ok(());
            };
            let mut sfd = sfd_handle.borrow_mut();
            let ShadowFdVariant::Pipe(data) = &mut sfd.data else {
                return Err(tag!("Applying PipeTransfer to non-Pipe ShadowFd"));
            };
            if data.program_closed {
                debug!("Received transfer to pipe with program connection closed, dropping");
                 
                return Ok(());
            }
            let add = &msg[8..];
            let ShadowFdPipeBuffer::ReadFromChannel(ref mut x) = data.buf else {
                return Err(tag!(
                    "Applying PipeTransfer to pipe ShadowFd not reading from channel"
                ));
            };
            x.extend(add.iter());

            Ok(())
        }
        WmsgType::PipeShutdownR | WmsgType::PipeShutdownW => {
            let map = &mut glob.map;
            let mut delete = false;
            let Some(sfd_handle) = get_sfd(map, remote_id) else {
                debug!("Shutdown message directed to RID={}, which does not exist or was already closed and deleted. Ignoring message, because it may have been sent before the remote was notified.",
                       remote_id);
                return Ok(());
            };
            let mut sfd = sfd_handle.borrow_mut();
            let ShadowFdVariant::Pipe(data) = &mut sfd.data else {
                return Err(tag!("Applying PipeTransfer to non-Pipe ShadowFd"));
            };
             

            if let ShadowFdPipeBuffer::ReadFromChannel(ref v) = data.buf {
                if typ != WmsgType::PipeShutdownW {
                    return Err(tag!(
                        "Did not receive (remote) write shutdown when reading from channel"
                    ));
                }

                data.channel_closed = true;
                if !v.is_empty() {
                     
                    debug!(
                        "Received write shutdown for RID={}, {} bytes pending",
                        remote_id,
                        v.len()
                    );
                } else {
                     
                    debug!(
                        "Received write shutdown for RID={}, nothing pending",
                        remote_id
                    );
                    delete = true;
                }
            } else {
                if typ != WmsgType::PipeShutdownR {
                    return Err(tag!(
                        "Did not receive (remote) read shutdown when writing to channel"
                    ));
                }

                 
                debug!("Received read shutdown for RID={}", remote_id);
                data.channel_closed = true;
                delete = true;
            }

            drop(sfd);
            if delete {
                 
                let pos = glob
                    .pipes
                    .iter()
                    .position(|x| x.borrow().remote_id == remote_id)
                    .unwrap();
                glob.pipes.remove(pos);
                map.remove(&remote_id);
            }

            Ok(())
        }

        _ => unreachable!("Unhandled message type: {:?}", typ),
    }
}

 
const DIFF_CHUNKSIZE: u32 = 262144;

 
fn construct_fill_transfers(
    rid: Rid,
    bufsize: usize,
    mapping: &ExternalMapping,
    way_msg_output: &mut TransferQueue,
    compression: Compression,
) -> Result<(), String> {
     
    let div_intv = (0_u32, (bufsize / 64) as u32);
    let len = div_intv.1 - div_intv.0;
    let mut nshards = ceildiv(len, DIFF_CHUNKSIZE / 64);
    let trail_size = bufsize % 64;
    if nshards == 0 && trail_size > 0 {
        nshards = 1;
    }

    for i in 0..nshards {
        let start = 64 * split_interval(div_intv.0, div_intv.1, nshards, i);
        let mut end = 64 * split_interval(div_intv.0, div_intv.1, nshards, i + 1);

        if i == nshards - 1 {
            end += trail_size as u32;
        }
        let space = (end - start) as usize;
        let mut output: Vec<u8> = vec![0; space];
         
         
        copy_from_mapping(&mut output, mapping, start as usize);

        let compressed: Vec<u8> = match compression {
            Compression::None => output,
            Compression::Lz4(lvl) => {
                let mut ctx = lz4_make_cctx().unwrap();
                lz4_compress_to_vec(&mut ctx, &output, lvl, 0, 0)
            }
            Compression::Zstd(lvl) => {
                let mut ctx = zstd_make_cctx().unwrap();
                zstd_compress_to_vec(&mut ctx, &output, lvl, 0, 0)
            }
        };

        let header = cat4x4(
            build_wmsg_header(WmsgType::BufferFill, (16 + compressed.len()) as usize).to_le_bytes(),
            rid.0.to_le_bytes(),
            start.to_le_bytes(),
            end.to_le_bytes(),
        );
        way_msg_output.other_messages.push(Vec::from(header));
        let pad = align4(compressed.len()) - compressed.len();
        way_msg_output.other_messages.push(compressed);
        if pad > 0 {
            way_msg_output.other_messages.push(vec![0; pad]);
        }
    }
    Ok(())
}

 
 
fn split_damage(intervals: &[(usize, usize)], buf_size: usize) -> (usize, Vec<Vec<(u32, u32)>>) {
    let final_chunk = 64 * (buf_size / 64);

    let mut net_len = 0;
    let mut has_trailing = false;
    for r in intervals.iter() {
        assert!(r.0 < r.1);
        assert!(r.0 % 64 == 0 && r.1 % 64 == 0);
        if r.1 > final_chunk {
             
            if r.0 < buf_size {
                has_trailing = true;
                assert!(final_chunk >= r.0);
                net_len += final_chunk - r.0;
            }
        } else {
            net_len += r.1 - r.0;
        }
    }

     
     
     
    let net_len: u32 = net_len.try_into().unwrap();
    assert!(net_len % 64 == 0);

    if net_len == 0 {
        if has_trailing {
            return (buf_size % 64, vec![Vec::new()]);
        } else {
            return (0, Vec::new());
        }
    }

    let nshards = ceildiv(net_len / 64, DIFF_CHUNKSIZE / 64);
    let trail_size = buf_size % 64;

    let mut intv_iter = intervals.iter();
    let mut prev_interval: Option<(usize, usize)> = None;
    let mut parts = Vec::new();
    for i in 0..nshards {
        let start = 64 * split_interval(0, net_len / 64, nshards, i);
        let end = 64 * split_interval(0, net_len / 64, nshards, i + 1);
        let mut remaining = end - start;
        let mut output = Vec::<(u32, u32)>::new();
        while remaining > 0 {
            let mut cur = if let Some(x) = prev_interval {
                prev_interval = None;
                x
            } else if let Some(y) = intv_iter.next() {
                *y
            } else {
                 
                break;
            };

            if cur.1 > final_chunk {
                 
                if cur.0 >= final_chunk {
                     
                    break;
                }
                 
                cur = (cur.0, final_chunk);
            }

            if (cur.1 - cur.0) as u32 <= remaining {
                remaining -= (cur.1 - cur.0) as u32;
                output.push((cur.0 as u32, cur.1 as u32));
            } else {
                output.push((cur.0 as u32, cur.0 as u32 + remaining));
                prev_interval = Some((cur.0 + remaining as usize, cur.1));
                break;
            }
        }
        parts.push(output);
    }

    (if has_trailing { trail_size } else { 0 }, parts)
}

 
fn collect_updates(
    sfd: &mut ShadowFd,
    way_msg_output: &mut TransferQueue,
    compression: Compression,
    tasksys: &TaskSystem,
    opts: &Options,
) -> Result<bool, String> {
    match &mut sfd.data {
        ShadowFdVariant::File(data) => {
            let first_visit = sfd.only_here;
            if sfd.only_here {
                 
                let msg = cat3x4(
                    build_wmsg_header(WmsgType::OpenFile, 12).to_le_bytes(),
                    sfd.remote_id.0.to_le_bytes(),
                    (data.buffer_size as u32).to_le_bytes(),
                );
                way_msg_output.other_messages.push(Vec::from(msg));
                data.remote_bufsize = data.buffer_size;
                sfd.only_here = false;
            }
            if data.remote_bufsize < data.buffer_size {
                assert!(!data.readonly);
                 
                let msg = cat3x4(
                    build_wmsg_header(WmsgType::ExtendFile, 12).to_le_bytes(),
                    sfd.remote_id.0.to_le_bytes(),
                    (data.buffer_size as u32).to_le_bytes(),
                );
                way_msg_output.other_messages.push(Vec::from(msg));
                data.remote_bufsize = data.buffer_size;
            }

            if data.readonly {
                if !first_visit {
                     
                    return Ok(true);
                }

                 
                construct_fill_transfers(
                    sfd.remote_id,
                    data.buffer_size,
                    &data.core.as_ref().unwrap().mapping,
                    way_msg_output,
                    compression,
                )?;
                return Ok(true);
            }

            let full_region = &[(0, align(data.buffer_size, 64))];
            let damaged_intervals: &[(usize, usize)] = match &data.damage {
                Damage::Everything => full_region,
                Damage::Intervals(ref x) => &x[..],
            };
            if damaged_intervals.is_empty() {
                 
                return Ok(true);
            }

            let (trail_len, parts) = split_damage(damaged_intervals, data.buffer_size);
            let nparts = parts.len();
            for (i, output) in parts.into_iter().enumerate() {
                 
                if i == nparts - 1 {
                    assert!(output.len() + trail_len > 0);
                } else {
                    assert!(!output.is_empty());
                }
                let region = if !output.is_empty() {
                    Some((output.first().unwrap().0, output.last().unwrap().1))
                } else {
                    None
                };

                let t = WorkTask::Diff(DiffTask {
                    rid: sfd.remote_id,
                    region,
                    intervals: output,
                    trailing: if i == nparts - 1 && trail_len > 0 {
                        trail_len as u32
                    } else {
                        0
                    },
                    compression,
                    target: data.core.as_ref().unwrap().clone(),
                });
                way_msg_output.expected_recvd_msgs += 1;
                tasksys.tasks.lock().unwrap().construct.push_back(t);
                tasksys.task_notify.notify_one();
            }

             
            data.damage = Damage::Intervals(Vec::new());
            Ok(true)
        }
        ShadowFdVariant::Dmabuf(data) => {
            if let Some(ref vid_enc) = data.video_encode {
                let DmabufImpl::Vulkan(ref buf) = data.buf else {
                    unreachable!();
                };

                if sfd.only_here {
                    let slice_data = dmabuf_slice_make_ideal(
                        data.drm_format,
                        buf.width,
                        buf.height,
                        buf.get_bpp(),
                    );
                    let vid_flags: u32 = 0xff & (opts.video.format.unwrap() as u32);
                    let msg = cat4x4(
                        build_wmsg_header(WmsgType::OpenDMAVidDstV2, 16 + slice_data.len())
                            .to_le_bytes(),
                        sfd.remote_id.0.to_le_bytes(),
                        (buf.nominal_size(data.view_row_stride) as u32).to_le_bytes(),
                        vid_flags.to_le_bytes(),
                    );

                    way_msg_output.other_messages.push(Vec::from(msg));
                    way_msg_output.other_messages.push(Vec::from(slice_data));
                    sfd.only_here = false;
                }

                 
                let full_region = &[(0, align(buf.nominal_size(data.view_row_stride), 64))];
                let damaged_intervals: &[(usize, usize)] = match &data.damage {
                    Damage::Everything => full_region,
                    Damage::Intervals(ref x) => &x[..],
                };
                if damaged_intervals.is_empty() {
                     
                    return Ok(true);
                }

                let mut acquires = Vec::new();
                for acq in data.acquires.drain(..) {
                    let (pt, sfd) = acq;
                    let b = sfd.borrow_mut();
                    let ShadowFdVariant::Timeline(ref timeline_data) = &b.data else {
                        panic!("Expected timeline sfd");
                    };
                    acquires.push((timeline_data.timeline.clone(), pt));
                }

                let mut implicit_semaphore = None;
                let (implicit_sync_file, must_poll) = dmabuf_setup_implicit_sync(data)?;
                data.using_implicit_sync = false;
                if let Some(sync_file) = implicit_sync_file {
                     
                    implicit_semaphore = Some(sync_file.export_binary_semaphore()?);
                }

                data.damage = Damage::Intervals(Vec::new());

                let task = VideoEncodeTask {
                    remote_id: sfd.remote_id,
                    vulk: buf.vulk.clone(),
                    state: vid_enc.clone(),
                    acquires: ExplicitSyncAcquires {
                        explicit: acquires,
                        implicit: implicit_semaphore,
                    },
                };

                let mut g = tasksys.tasks.lock().unwrap();
                if must_poll {
                    g.waiting_for_implicit_acquire
                        .entry(sfd.remote_id)
                        .or_default()
                        .push(WorkTask::VideoEncode(task));
                    drop(g);
                } else {
                    g.construct.push_back(WorkTask::VideoEncode(task));
                    drop(g);
                    tasksys.task_notify.notify_one();
                };

                way_msg_output.expected_recvd_msgs += 1;

                return Ok(true);
            }

            let nominal_size = match data.buf {
                DmabufImpl::Vulkan(ref vulk_buf) => vulk_buf.nominal_size(data.view_row_stride),
                DmabufImpl::Gbm(ref gbm_buf) => gbm_buf.nominal_size(data.view_row_stride),
            };

            if sfd.only_here {
                 
                let (width, height, bpp) = match data.buf {
                    DmabufImpl::Vulkan(ref vulk_buf) => {
                        (vulk_buf.width, vulk_buf.height, vulk_buf.get_bpp())
                    }
                    DmabufImpl::Gbm(ref gbm_buf) => {
                        (gbm_buf.width, gbm_buf.height, gbm_buf.get_bpp())
                    }
                };
                let slice_data = dmabuf_slice_make_ideal(data.drm_format, width, height, bpp);
                let msg = cat3x4(
                    build_wmsg_header(WmsgType::OpenDMABUF, 12 + slice_data.len()).to_le_bytes(),
                    sfd.remote_id.0.to_le_bytes(),
                    (nominal_size as u32).to_le_bytes(),
                );

                way_msg_output.other_messages.push(Vec::from(msg));
                way_msg_output.other_messages.push(Vec::from(slice_data));
                sfd.only_here = false;
            }

             
            let full_region = &[(0, align(nominal_size, 64))];
            let damaged_intervals: &[(usize, usize)] = match &data.damage {
                Damage::Everything => full_region,
                Damage::Intervals(ref x) => &x[..],
            };
            if damaged_intervals.is_empty() {
                 
                return Ok(true);
            }

             
            let mut acquires = Vec::new();
            for acq in data.acquires.drain(..) {
                let (pt, sfd) = acq;
                let b = sfd.borrow_mut();
                let ShadowFdVariant::Timeline(ref timeline_data) = &b.data else {
                    panic!("Expected timeline sfd");
                };
                acquires.push((timeline_data.timeline.clone(), pt));
            }

            let (implicit_sync_file, must_poll) = dmabuf_setup_implicit_sync(data)?;
            data.using_implicit_sync = false;

            let copied = if let DmabufImpl::Gbm(ref mut gbm_buf) = data.buf {
                 
                let mut v = vec![0; nominal_size];
                gbm_buf.copy_from_dmabuf(data.view_row_stride, &mut v)?;
                v
            } else {
                Vec::new()
            };

            if data.first_damage || !copied.is_empty() {
                 
                data.first_damage = false;

                if data.mirror.is_none() {
                     
                    data.mirror = Some(Arc::new(Mirror::new(nominal_size, false)?));
                }

                let div_intv = (0_u32, (nominal_size / 64) as u32);
                let len = div_intv.1 - div_intv.0;
                let nshards = ceildiv(len, DIFF_CHUNKSIZE / 64);
                let trail_size = nominal_size % 64;

                for i in 0..nshards {
                    let start = 64 * split_interval(div_intv.0, div_intv.1, nshards, i);
                    let mut end = 64 * split_interval(div_intv.0, div_intv.1, nshards, i + 1);

                    if i == nshards - 1 {
                        end += trail_size as u32;
                    }

                    let t = match data.buf {
                        DmabufImpl::Vulkan(ref vulk_buf) => {
                            let implicit_semaphore = if let Some(ref s) = implicit_sync_file {
                                Some(s.export_binary_semaphore()?)
                            } else {
                                None
                            };
                            WorkTask::FillDmabuf(FillDmabufTask {
                                rid: sfd.remote_id,
                                compression,
                                region_start: start,
                                region_end: end,
                                mirror: data.mirror.clone(),
                                dst: vulk_buf.clone(),
                                view_row_stride: data.view_row_stride,
                                acquires: ExplicitSyncAcquires {
                                    explicit: acquires.clone(),
                                    implicit: implicit_semaphore,
                                },
                            })
                        }
                        DmabufImpl::Gbm(_) => WorkTask::FillDmabuf2(FillDmabufTask2 {
                            rid: sfd.remote_id,
                            compression,
                            region_start: start,
                            region_end: end,
                            mirror: data.mirror.clone(),
                            wait_until: 0,
                            read_buf: ReadDmabufResult::Shm(Vec::from(
                                &copied[start as usize..end as usize],
                            )),
                        }),
                    };

                    way_msg_output.expected_recvd_msgs += 1;

                    let mut g = tasksys.tasks.lock().unwrap();
                    if must_poll {
                        g.waiting_for_implicit_acquire
                            .entry(sfd.remote_id)
                            .or_default()
                            .push(t);
                        drop(g);
                    } else {
                        g.construct.push_back(t);
                        drop(g);
                        tasksys.task_notify.notify_one();
                    };
                }
            } else {
                 
                 
                 
                let (trail_len, parts) = split_damage(damaged_intervals, nominal_size);
                let nparts = parts.len();

                for (i, output) in parts.into_iter().enumerate() {
                     
                    if i == nparts - 1 {
                        assert!(output.len() + trail_len > 0);
                    } else {
                        assert!(!output.is_empty());
                    }
                    let region = if !output.is_empty() {
                        Some((output.first().unwrap().0, output.last().unwrap().1))
                    } else {
                        None
                    };
                    let trailing = if i == nparts - 1 && trail_len > 0 {
                        trail_len as u32
                    } else {
                        0
                    };

                    let t = match data.buf {
                        DmabufImpl::Vulkan(ref vulk_buf) => {
                            let implicit_semaphore = if let Some(ref s) = implicit_sync_file {
                                Some(s.export_binary_semaphore()?)
                            } else {
                                None
                            };
                            WorkTask::DiffDmabuf(DiffDmabufTask {
                                rid: sfd.remote_id,
                                region,
                                intervals: output,
                                trailing,
                                compression,
                                mirror: data.mirror.as_ref().unwrap().clone(),
                                img: vulk_buf.clone(),
                                view_row_stride: data.view_row_stride,
                                acquires: ExplicitSyncAcquires {
                                    explicit: acquires.clone(),
                                    implicit: implicit_semaphore,
                                },
                            })
                        }
                        DmabufImpl::Gbm(_) => todo!(),
                    };
                    way_msg_output.expected_recvd_msgs += 1;

                    let mut g = tasksys.tasks.lock().unwrap();
                    if must_poll {
                        g.waiting_for_implicit_acquire
                            .entry(sfd.remote_id)
                            .or_default()
                            .push(t);
                    } else {
                        g.construct.push_back(t);
                    };
                    drop(g);
                    tasksys.task_notify.notify_one();
                }
            }
            data.damage = Damage::Intervals(Vec::new());

            Ok(true)
        }
        ShadowFdVariant::Timeline(_data) => {
             
            if sfd.only_here {
                 
                 
                let pt: u64 = 0;
                let pt_val = pt.to_le_bytes();
                let msg = cat4x4(
                    build_wmsg_header(WmsgType::OpenTimeline, 16).to_le_bytes(),
                    sfd.remote_id.0.to_le_bytes(),
                    pt_val[..4].try_into().unwrap(),
                    pt_val[4..].try_into().unwrap(),
                );

                way_msg_output.other_messages.push(Vec::from(msg));

                sfd.only_here = false;
            }
            Ok(true)
        }
        ShadowFdVariant::Pipe(data) => {
            let reading_from_channel = match data.buf {
                ShadowFdPipeBuffer::ReadFromWayland(_) => false,
                ShadowFdPipeBuffer::ReadFromChannel(_) => true,
            };

            if sfd.only_here {
                 
                let mtype = if reading_from_channel {
                    WmsgType::OpenIRPipe
                } else {
                    WmsgType::OpenIWPipe
                };
                let msg = cat2x4(
                    build_wmsg_header(mtype, 8).to_le_bytes(),
                    sfd.remote_id.0.to_le_bytes(),
                );
                way_msg_output.other_messages.push(Vec::from(msg));
                sfd.only_here = false;
                debug!("Queueing message: {:?}", mtype);
            }

             
            if let ShadowFdPipeBuffer::ReadFromWayland((ref mut buf, ref mut len)) = data.buf {
                if *len > 0 {
                    let sz = 8 + *len;
                    let msg_header = cat2x4(
                        build_wmsg_header(WmsgType::PipeTransfer, sz).to_le_bytes(),
                        sfd.remote_id.0.to_le_bytes(),
                    );
                    let pad = align4(*len) - *len;
                    way_msg_output.other_messages.push(Vec::from(msg_header));
                    way_msg_output.other_messages.push(Vec::from(&buf[..*len]));
                    if pad > 0 {
                        way_msg_output.other_messages.push(vec![0; pad]);
                    }
                    debug!("Queueing message: {:?}", WmsgType::PipeTransfer);

                    *len = 0;
                }
            }

            if data.program_closed {
                 
                 
                let ctype = if reading_from_channel {
                    WmsgType::PipeShutdownR
                } else {
                    WmsgType::PipeShutdownW
                };
                let msg = cat2x4(
                    build_wmsg_header(ctype, 8).to_le_bytes(),
                    sfd.remote_id.0.to_le_bytes(),
                );
                way_msg_output.other_messages.push(Vec::from(msg));
                debug!("Queueing message: {:?}", ctype);

                 
                Ok(false)
            } else {
                Ok(true)
            }
        }
    }
}

 
fn prune_releases(
    releases: &mut Vec<(u64, Rc<RefCell<ShadowFd>>)>,
    current_pt: u64,
    this_timeline: Rid,
) {
    releases.retain(|(pt, sfd)| {
        if *pt > current_pt {
             
            return true;
        }

        let mut c = sfd.borrow_mut();
        let ShadowFdVariant::Dmabuf(ref mut dmabuf) = c.data else {
            panic!();
        };
        dmabuf.releases.remove(&(this_timeline, *pt)).unwrap();
        false
    });
}

 
pub fn signal_timeline_acquires(
    acquires: &mut Vec<(u64, Rc<RefCell<ShadowFd>>)>,
) -> Result<(), String> {
    for acq in acquires.drain(..) {
        let (pt, timeline) = acq;
        let c = timeline.borrow_mut();
        let ShadowFdVariant::Timeline(ref timeline_data) = c.data else {
            panic!("expected timeline-type shadowfd");
        };
        debug!(
            "Signalling timeline acquire for {}, pt {}",
            timeline_data.debug_wayland_id, pt
        );
        timeline_data.timeline.signal_timeline_pt(pt)?;
    }
    Ok(())
}

 
fn diff_inner(task: &DiffTask, dst: &mut [u8]) -> Result<(u32, u32), String> {
    let buflen = task.target.mapping.get_u8().len();

     
    let diff_len = if let Some((start, end)) = task.region {
        assert!(start % 64 == 0);
        assert!(end % 64 == 0);
        let mirror = task
            .target
            .mem_mirror
            .get_mut_range((start as usize)..(end as usize))
            .ok_or("Failed to acquire mirror for diff")?;

        construct_diff(
            dst,
            &task.target.mapping,
            &task.intervals[..],
            mirror.data,
            start,
        ) as u32
    } else {
        0
    };

    let mut ntrailing: u32 = 0;
    if task.trailing > 0 {
        let trail_mirror = task
            .target
            .mem_mirror
            .get_mut_range((buflen - (task.trailing as usize))..buflen)
            .ok_or("Failed to acquire trailing mirror")?;
        let tail_changed = copy_tail_if_diff(
            &mut dst[(diff_len as usize)..],
            &task.target.mapping,
            task.trailing as usize,
            trail_mirror.data,
        );
        if tail_changed {
            ntrailing = task.trailing;
        }
    }

    let region = task.region.unwrap_or((0, 0));
    debug!(
        "{} mid diff task: {}..{},+{} -> diff {} {}",
        std::thread::current().name().unwrap_or(""),
        region.0,
        region.1,
        task.trailing,
        diff_len,
        ntrailing
    );

    Ok((diff_len, ntrailing))
}

 
fn run_diff_task(task: &DiffTask, cache: &mut ThreadCache) -> TaskResult {
    debug!(
        "{} running diff task: {}..{},+{}",
        std::thread::current().name().unwrap_or(""),
        task.region.unwrap_or((0, 0)).0,
        task.region.unwrap_or((0, 0)).1,
        task.trailing
    );

     
    let mut diffspace = 0;
    for t in task.intervals.iter() {
        diffspace += 8 + t.1 - t.0;
    }
    let space = diffspace + task.trailing;

    let (mut msg, unpadded_len, diff_len, ntrailing) = match task.compression {
        Compression::None => {
            let mut buf: Vec<u8> = vec![0; align4(space as usize) + 16];
            let (diff_len, ntrailing) = diff_inner(task, &mut buf[16..(16 + space as usize)])?;
            if diff_len == 0 && ntrailing == 0 {
                 
                return Ok(TaskOutput::Msg(Vec::new()));
            }
            let raw_len = (diff_len + ntrailing) as usize;

            (buf, 16 + raw_len, diff_len, ntrailing)
        }
        Compression::Lz4(_) | Compression::Zstd(_) => {
            cache
                .large
                .resize(std::cmp::max(cache.large.len(), space as usize), 0);
            let (diff_len, ntrailing) = diff_inner(task, &mut cache.large[..(space as usize)])?;
            if diff_len == 0 && ntrailing == 0 {
                 
                return Ok(TaskOutput::Msg(Vec::new()));
            }
            let raw_len = (diff_len + ntrailing) as usize;

            let nxt = comp_into_vec(
                task.compression,
                &mut cache.comp,
                &cache.large[..raw_len],
                16,
                4,
            )?;
            let sz = nxt.len() - 4 - 16;
            (nxt, 16 + sz, diff_len, ntrailing)
        }
    };

     
    assert!(msg.len() >= align4(unpadded_len));
    msg.truncate(align4(unpadded_len));

     
    for i in 0..(align4(unpadded_len) - unpadded_len) {
        msg[unpadded_len + i] = 0;
    }

    let header = cat4x4(
        build_wmsg_header(WmsgType::BufferDiff, unpadded_len).to_le_bytes(),
        task.rid.0.to_le_bytes(),
        diff_len.to_le_bytes(),
        ntrailing.to_le_bytes(),
    );
    msg[..16].copy_from_slice(&header);

    Ok(TaskOutput::Msg(msg))
}

 
fn diff_dmabuf_inner(task: &DiffDmabufTask2, dst: &mut [u8]) -> Result<(u32, u32), String> {
    let img_len = task.nominal_size;
    let data = match task.read_buf {
        ReadDmabufResult::Vulkan(ref buf) => {
            buf.prepare_read()?;
            buf.get_read_view().data
        }
        ReadDmabufResult::Shm(ref v) => &v[..],
    };

    let mut dst_view = dst;
    let diff_len: u32 = if let Some((region_start, region_end)) = task.region {
        assert!(region_start % 64 == 0);
        assert!(region_end % 64 == 0);

         
        let mirror = task
            .mirror
            .get_mut_range((region_start as usize)..(region_end as usize))
            .ok_or("Failed to acquire mirror for diff")?;

        let mut start: usize = 0;
        let mut diff_len: u32 = 0;
        for intv in task.intervals.iter() {
            let intv_len = (intv.1 - intv.0) as usize;

            let mirr_range = &mut mirror.data
                [((intv.0 - region_start) as usize)..((intv.1 - region_start) as usize)];

            let mut diff_segment_len = construct_diff_segment_two(
                dst_view,
                &data[start..start + intv_len],
                mirr_range,
                intv.0,
                32,  
            );
            if false {
                 
                dst_view[..4].copy_from_slice((intv.0 / 4).to_le_bytes().as_slice());
                dst_view[4..8].copy_from_slice((intv.1 / 4).to_le_bytes().as_slice());
                dst_view[8..8 + intv_len].copy_from_slice(&data[start..start + intv_len]);
                diff_segment_len = (intv_len + 8) as u32;
            }

            dst_view = &mut std::mem::take(&mut dst_view)[diff_segment_len as usize..];

            diff_len += diff_segment_len;
            start += intv_len;
        }
        assert!(start + (task.trailing as usize) == data.len());
        diff_len
    } else {
        0
    };

    let mut ntrailing: u32 = 0;
    if task.trailing > 0 {
        let trail_mirror = task
            .mirror
            .get_mut_range((img_len - (task.trailing as usize))..img_len)
            .ok_or("Failed to acquire trailing mirror")?;
        let tail_segment: &[u8] = &data[data.len() - task.trailing as usize..];
        assert!(tail_segment.len() == trail_mirror.data.len());
        if tail_segment != trail_mirror.data {
            trail_mirror.data.copy_from_slice(tail_segment);
            dst_view[..task.trailing as usize].copy_from_slice(tail_segment);
            ntrailing = task.trailing;
        }
    }

    debug!(
        "{} mid dmabuf diff task: {}..{},+{} -> diff {} {}",
        std::thread::current().name().unwrap_or(""),
        task.region.unwrap_or((0, 0)).0,
        task.region.unwrap_or((0, 0)).1,
        task.trailing,
        diff_len,
        ntrailing
    );

    Ok((diff_len, ntrailing))
}

 
fn run_diff_dmabuf_task(
    task: DiffDmabufTask,
    cache: &mut ThreadCache,
) -> Result<DiffDmabufTask2, String> {
    debug!(
        "{} running diff task for dmabuf: {}..{},+{}",
        std::thread::current().name().unwrap_or(""),
        task.region.unwrap_or((0, 0)).0,
        task.region.unwrap_or((0, 0)).1,
        task.trailing
    );

    let mut segments = Vec::new();
    let mut start = 0;
    for intv in task.intervals.iter() {
        segments.push((start, intv.0, intv.1));
        start += intv.1 - intv.0;
    }
    let nom_len = task.img.nominal_size(task.view_row_stride);
    if task.trailing > 0 {
        segments.push((start, (nom_len as u32) - task.trailing, (nom_len as u32)));
    }

    let pool: &Arc<VulkanCommandPool> = cache.get_cmd_pool(&task.img.vulk)?;

    let buf_len = start + task.trailing;
    let read_buf = Arc::new(vulkan_get_buffer(&task.img.vulk, buf_len as usize, true)?);

     
    let handle = start_copy_segments_from_dmabuf(
        &task.img,
        &read_buf,
        pool,
        &segments[..],
        task.view_row_stride,
        &task.acquires.explicit[..],
        task.acquires.implicit.as_slice(),
    )?;
    let pt = handle.get_timeline_point();
    cache.copy_ops.push(handle);

    Ok(DiffDmabufTask2 {
        rid: task.rid,
        compression: task.compression,
        region: task.region,
        intervals: task.intervals,
        trailing: task.trailing,
        wait_until: pt,
        read_buf: ReadDmabufResult::Vulkan(read_buf),
        mirror: task.mirror,
        nominal_size: task.img.nominal_size(task.view_row_stride),
    })
}
 
fn run_diff_dmabuf_task_2(task: DiffDmabufTask2, cache: &mut ThreadCache) -> TaskResult {
     
    let mut diffspace = 0;
    for t in task.intervals.iter() {
        diffspace += 8 + t.1 - t.0;
    }
    let space = diffspace + task.trailing;

     
     
     

    let (mut msg, unpadded_len, diff_len, ntrailing) = match task.compression {
        Compression::None => {
            let mut buf: Vec<u8> = vec![0; align4(space as usize) + 16];
            let (diff_len, ntrailing) =
                diff_dmabuf_inner(&task, &mut buf[16..(16 + space as usize)])?;
            if diff_len == 0 && ntrailing == 0 {
                 
                return Ok(TaskOutput::Msg(Vec::new()));
            }
            let raw_len = (diff_len + ntrailing) as usize;

            (buf, 16 + raw_len, diff_len, ntrailing)
        }
        Compression::Lz4(_) | Compression::Zstd(_) => {
            cache
                .large
                .resize(std::cmp::max(cache.large.len(), space as usize), 0);
            let (diff_len, ntrailing) =
                diff_dmabuf_inner(&task, &mut cache.large[..(space as usize)])?;
            if diff_len == 0 && ntrailing == 0 {
                 
                return Ok(TaskOutput::Msg(Vec::new()));
            }
            let raw_len = (diff_len + ntrailing) as usize;

            let nxt = comp_into_vec(
                task.compression,
                &mut cache.comp,
                &cache.large[..raw_len],
                16,
                4,
            )?;
            let sz = nxt.len() - 4 - 16;
            (nxt, 16 + sz, diff_len, ntrailing)
        }
    };

     
    assert!(msg.len() >= align4(unpadded_len));
    msg.truncate(align4(unpadded_len));

     
    for i in 0..(align4(unpadded_len) - unpadded_len) {
        msg[unpadded_len + i] = 0;
    }

    let header = cat4x4(
        build_wmsg_header(WmsgType::BufferDiff, unpadded_len).to_le_bytes(),
        task.rid.0.to_le_bytes(),
        diff_len.to_le_bytes(),
        ntrailing.to_le_bytes(),
    );
    msg[..16].copy_from_slice(&header);

    Ok(TaskOutput::Msg(msg))
}

impl ThreadCache {
    fn get_cmd_pool<'a>(
        &'a mut self,
        vulk: &Arc<VulkanDevice>,
    ) -> Result<&'a Arc<VulkanCommandPool>, String> {
        if self.cmd_pool.is_none() {
            let p = vulkan_get_cmd_pool(vulk)?;
            self.cmd_pool = Some(p);
        }
        Ok(self.cmd_pool.as_ref().unwrap())
    }
}
impl ThreadCacheComp {
    fn get_lz4_cctx(&mut self) -> Result<&mut LZ4CCtx, String> {
        if self.lz4_c.is_none() {
            self.lz4_c = Some(
                lz4_make_cctx().ok_or_else(|| tag!("Failed to make LZ4 compression context"))?,
            );
        }
        Ok(self.lz4_c.as_mut().unwrap())
    }
    fn get_zstd_cctx(&mut self) -> Result<&mut ZstdCCtx, String> {
        if self.zstd_c.is_none() {
            self.zstd_c = Some(
                zstd_make_cctx().ok_or_else(|| tag!("Failed to make LZ4 compression context"))?,
            );
        }
        Ok(self.zstd_c.as_mut().unwrap())
    }
    fn get_zstd_dctx(&mut self) -> Result<&mut ZstdDCtx, String> {
        if self.zstd_d.is_none() {
            self.zstd_d = Some(
                zstd_make_dctx().ok_or_else(|| tag!("Failed to make LZ4 compression context"))?,
            );
        }
        Ok(self.zstd_d.as_mut().unwrap())
    }
}

 
fn run_fill_dmabuf_task(
    task: FillDmabufTask,
    cache: &mut ThreadCache,
) -> Result<FillDmabufTask2, String> {
    debug!(
        "{} running fill task: {}..{}",
        std::thread::current().name().unwrap_or(""),
        task.region_start,
        task.region_end,
    );

    let pool: &Arc<VulkanCommandPool> = cache.get_cmd_pool(&task.dst.vulk)?;

    let space = task.region_end - task.region_start;
    let read_buf = Arc::new(vulkan_get_buffer(&task.dst.vulk, space as usize, true)?);

     
    let handle = start_copy_segments_from_dmabuf(
        &task.dst,
        &read_buf,
        pool,
        &[(0, task.region_start, task.region_end)],
        task.view_row_stride,
        &task.acquires.explicit[..],
        task.acquires.implicit.as_slice(),
    )?;
    let pt = handle.get_timeline_point();
    cache.copy_ops.push(handle);

    Ok(FillDmabufTask2 {
        rid: task.rid,
        compression: task.compression,
        region_start: task.region_start,
        region_end: task.region_end,
        mirror: task.mirror,
        read_buf: ReadDmabufResult::Vulkan(read_buf),
        wait_until: pt,
    })
}

 
fn run_dmabuf_fill_task_2(task: FillDmabufTask2, cache: &mut ThreadCache) -> TaskResult {
    let data = match task.read_buf {
        ReadDmabufResult::Vulkan(ref buf) => {
            buf.prepare_read()?;
            buf.get_read_view().data
        }
        ReadDmabufResult::Shm(ref v) => &v[..],
    };

     
     
    if let Some(mir) = &task.mirror {
        let range = mir
            .get_mut_range(task.region_start as usize..task.region_end as usize)
            .ok_or("failed to acquire mirror range")?;
        range.data.copy_from_slice(data);
    }

    let mut msg: Vec<u8> = comp_into_vec(task.compression, &mut cache.comp, data, 16, 4)?;
    let msg_len = msg.len() - 4;
    msg.truncate(align4(msg_len));

    let header = cat4x4(
        build_wmsg_header(WmsgType::BufferFill, msg_len as usize).to_le_bytes(),
        task.rid.0.to_le_bytes(),
        task.region_start.to_le_bytes(),
        task.region_end.to_le_bytes(),
    );
    msg[..16].copy_from_slice(&header);
    Ok(TaskOutput::Msg(msg))
}

 
enum DecompReturn {
    Shm(ApplyTask),
    Dmabuf((u64, Rid, Option<ApplyTask>)),
}

 
fn is_segment_texel_aligned(start: usize, end: usize, bpp: usize) -> bool {
    start % bpp == 0 && end % bpp == 0
}

 
fn decomp_into_slice(
    comp: Compression,
    cache: &mut ThreadCacheComp,
    src: &[u8],
    dst: &mut [u8],
) -> Result<(), String> {
    match comp {
        Compression::None => dst.copy_from_slice(src),
        Compression::Lz4(_) => {
            lz4_decompress_to_slice(src, dst).ok_or_else(|| tag!("Failed to decompress"))?
        }
        Compression::Zstd(_) => zstd_decompress_to_slice(cache.get_zstd_dctx()?, src, dst)
            .ok_or_else(|| tag!("Failed to decompress"))?,
    };
    Ok(())
}

 
fn decomp_into_vec(
    comp: Compression,
    cache: &mut ThreadCacheComp,
    src: &[u8],
    uncomp_len: usize,
) -> Result<Vec<u8>, String> {
    Ok(match comp {
        Compression::None => {
            assert!(src.len() == uncomp_len);
            Vec::from(src)
        }
        Compression::Lz4(_) => {
            lz4_decompress_to_vec(src, uncomp_len).ok_or_else(|| tag!("Failed to decompress"))?
        }
        Compression::Zstd(_) => zstd_decompress_to_vec(cache.get_zstd_dctx()?, src, uncomp_len)
            .ok_or_else(|| tag!("Failed to decompress"))?,
    })
}

 
fn comp_into_vec(
    comp: Compression,
    cache: &mut ThreadCacheComp,
    src: &[u8],
    pad_pre: usize,
    pad_post: usize,
) -> Result<Vec<u8>, String> {
    Ok(match comp {
        Compression::None => {
            let mut v = vec![0; pad_pre];
            v.extend_from_slice(src);
            v.resize(pad_pre + src.len() + pad_post, 0);
            v
        }
        Compression::Lz4(lvl) => {
            lz4_compress_to_vec(cache.get_lz4_cctx()?, src, lvl, pad_pre, pad_post)
        }
        Compression::Zstd(lvl) => {
            zstd_compress_to_vec(cache.get_zstd_cctx()?, src, lvl, pad_pre, pad_post)
        }
    })
}

 
fn run_decomp_task(task: &DecompTask, cache: &mut ThreadCache) -> Result<DecompReturn, String> {
    let msg = task.msg_view.get();

    let header = u32::from_le_bytes(msg[0..4].try_into().unwrap());
    let (len, t) = parse_wmsg_header(header).unwrap();
    let remote_id = Rid(i32::from_le_bytes(msg[4..8].try_into().unwrap()));
    if t == WmsgType::BufferDiff {
        let diff_size = u32::from_le_bytes(msg[8..12].try_into().unwrap());
        let ntrailing = u32::from_le_bytes(msg[12..16].try_into().unwrap());

        match &task.target {
            DecompTarget::Dmabuf(target) => {
                let decomp_len = (diff_size + ntrailing) as usize;

                 
                 
                let write_buf = Arc::new(vulkan_get_buffer(&target.dst.vulk, decomp_len, true)?);
                let write_view = write_buf.get_write_view();
                decomp_into_slice(
                    task.compression,
                    &mut cache.comp,
                    &msg[16..len],
                    write_view.data,
                )?;
                drop(write_view);
                write_buf.complete_write()?;

                 
                let reread_view = write_buf.get_read_view();
                let (region_start, region_end) =
                    compute_diff_span(reread_view.data, ntrailing as usize, task.file_size)?;

                let mut misaligned: bool = false;
                let bpp = target.dst.get_bpp();

                let mut segments: Vec<(u32, u32, u32)> = Vec::new();
                let mut pos: usize = 0;
                while pos + 8 <= (diff_size as usize) {
                    let span_start =
                        u32::from_le_bytes(reread_view.data[pos..pos + 4].try_into().unwrap());
                    let span_end =
                        u32::from_le_bytes(reread_view.data[pos + 4..pos + 8].try_into().unwrap());
                    if (4 * span_start) % bpp != 0 || (4 * span_end) % bpp != 0 {
                        misaligned = true;
                    }

                    pos += 8;
                     
                    segments.push((pos as u32, 4 * span_start, 4 * span_end));
                    pos += (span_end - span_start) as usize * 4;
                }
                assert!(pos == diff_size as usize);
                if ntrailing > 0 {
                    segments.push((
                        diff_size,
                        task.file_size as u32 - ntrailing,
                        task.file_size as u32,
                    ));
                }

                if !misaligned {
                     
                    let pool: &Arc<VulkanCommandPool> = cache.get_cmd_pool(&target.dst.vulk)?;
                    let copy_handle: VulkanCopyHandle = start_copy_segments_onto_dmabuf(
                        &target.dst,
                        &write_buf,
                        pool,
                        &segments[..],
                        target.view_row_stride,
                        &[],
                    )?;
                    let copy_id = copy_handle.get_timeline_point();
                     
                    cache.copy_ops.push(copy_handle);

                    let apply_task = if let Some(mir) = &target.mirror.as_ref() {
                         
                        let data = Vec::from(reread_view.data);
                        Some(ApplyTask {
                            sequence: task.sequence.unwrap(),
                            data,
                            is_diff_type: true,
                            ntrailing: ntrailing as usize,
                            target: ApplyTaskTarget::MirrorOnly(DecompTaskMirror {
                                mirror: (*mir).clone(),
                                notify_on_completion: false,
                            }),
                            region_start,
                            region_end,
                            remote_id,
                        })
                    } else {
                        None
                    };

                    Ok(DecompReturn::Dmabuf((copy_id, remote_id, apply_task)))
                } else {
                     
                    debug!("Using slow path for diff application, a segment is not pixel aligned");

                    let diff = decomp_into_vec(
                        task.compression,
                        &mut cache.comp,
                        &msg[16..len],
                        decomp_len,
                    )?;
                    let b = target.dst.get_bpp() as usize;
                    let (ext_start, ext_end) = (b * (region_start / b), align(region_end, b));

                     
                    Ok(DecompReturn::Shm(ApplyTask {
                        sequence: task.sequence.unwrap(),
                        data: diff,
                        is_diff_type: true,
                        ntrailing: ntrailing as usize,
                        target: ApplyTaskTarget::Dmabuf(ApplyTaskDmabuf {
                            target: target.clone(),
                            orig_start: region_start,
                            orig_end: region_end,
                        }),
                        region_start: ext_start,
                        region_end: ext_end,
                        remote_id,
                    }))
                }
            }
            DecompTarget::File(target) => {
                let diff = decomp_into_vec(
                    task.compression,
                    &mut cache.comp,
                    &msg[16..len],
                    (diff_size + ntrailing) as usize,
                )?;

                 
                let (region_start, region_end) =
                    compute_diff_span(&diff, ntrailing as usize, task.file_size)?;

                Ok(DecompReturn::Shm(ApplyTask {
                    sequence: task.sequence.unwrap(),
                    data: diff,
                    is_diff_type: true,
                    ntrailing: ntrailing as usize,
                    target: ApplyTaskTarget::Shm(DecompTaskFile {
                        skip_mirror: target.skip_mirror,
                        target: target.target.clone(),
                    }),
                    region_start,
                    region_end,
                    remote_id,
                }))
            }
            DecompTarget::MirrorOnly(target) => {
                let diff = decomp_into_vec(
                    task.compression,
                    &mut cache.comp,
                    &msg[16..len],
                    (diff_size + ntrailing) as usize,
                )?;

                 
                let (region_start, region_end) =
                    compute_diff_span(&diff, ntrailing as usize, task.file_size)?;

                Ok(DecompReturn::Shm(ApplyTask {
                    sequence: task.sequence.unwrap(),
                    data: diff,
                    is_diff_type: true,
                    ntrailing: ntrailing as usize,
                    target: ApplyTaskTarget::MirrorOnly(target.clone()),
                    region_start,
                    region_end,
                    remote_id,
                }))
            }
        }
    } else if t == WmsgType::BufferFill {
        let region_start = u32::from_le_bytes(msg[8..12].try_into().unwrap()) as usize;
        let region_end = u32::from_le_bytes(msg[12..16].try_into().unwrap()) as usize;
        if region_end <= region_start {
            return Err(tag!("Invalid region: {} {}", region_start, region_end));
        }
        let reg_len = region_end - region_start;

        match &task.target {
            DecompTarget::Dmabuf(target) => {
                 
                 

                if is_segment_texel_aligned(region_start, region_end, target.dst.get_bpp() as usize)
                {
                     
                    let write_buf = Arc::new(vulkan_get_buffer(&target.dst.vulk, reg_len, true)?);
                    let write_view = write_buf.get_write_view();
                    decomp_into_slice(
                        task.compression,
                        &mut cache.comp,
                        &msg[16..len],
                        write_view.data,
                    )?;
                    drop(write_view);
                    write_buf.complete_write()?;

                    let pool: &Arc<VulkanCommandPool> = cache.get_cmd_pool(&target.dst.vulk)?;
                    let copy_handle: VulkanCopyHandle = start_copy_segments_onto_dmabuf(
                        &target.dst,
                        &write_buf,
                        pool,
                        &[(0, region_start as u32, region_end as u32)],
                        target.view_row_stride,
                        &[],
                    )?;
                    let copy_id = copy_handle.get_timeline_point();
                    cache.copy_ops.push(copy_handle);

                     
                    let mir_task = if let Some(mir) = &target.mirror.as_ref() {
                         
                         
                        let reread_view = write_buf.get_read_view();
                        let data = Vec::from(reread_view.data);
                         
                         
                         
                         

                        Some(ApplyTask {
                            sequence: task.sequence.unwrap(),
                            data,
                            is_diff_type: false,
                            ntrailing: 0,
                            target: ApplyTaskTarget::MirrorOnly(DecompTaskMirror {
                                mirror: (*mir).clone(),
                                notify_on_completion: false,
                            }),
                            region_start,
                            region_end,
                            remote_id,
                        })
                    } else {
                        None
                    };

                    Ok(DecompReturn::Dmabuf((copy_id, remote_id, mir_task)))
                } else {
                     
                    debug!(
                        "Using slow path for fill application {}..{} is not bpp={} aligned",
                        region_start,
                        region_end,
                        target.dst.get_bpp()
                    );
                    let fill =
                        decomp_into_vec(task.compression, &mut cache.comp, &msg[16..len], reg_len)?;
                    let b = target.dst.get_bpp() as usize;
                    let (ext_start, ext_end) = (b * (region_start / b), align(region_end, b));

                     
                    Ok(DecompReturn::Shm(ApplyTask {
                        sequence: task.sequence.unwrap(),
                        data: fill,
                        is_diff_type: false,
                        ntrailing: 0,
                        target: ApplyTaskTarget::Dmabuf(ApplyTaskDmabuf {
                            target: target.clone(),
                            orig_start: region_start,
                            orig_end: region_end,
                        }),
                        region_start: ext_start,
                        region_end: ext_end,
                        remote_id,
                    }))
                }
            }
            DecompTarget::File(target) => {
                let fill =
                    decomp_into_vec(task.compression, &mut cache.comp, &msg[16..len], reg_len)?;
                Ok(DecompReturn::Shm(ApplyTask {
                    sequence: task.sequence.unwrap(),
                    data: fill,
                    is_diff_type: false,
                    ntrailing: 0,
                    target: ApplyTaskTarget::Shm(DecompTaskFile {
                        skip_mirror: target.skip_mirror,
                        target: target.target.clone(),
                    }),
                    region_start,
                    region_end,
                    remote_id,
                }))
            }
            DecompTarget::MirrorOnly(target) => {
                let fill =
                    decomp_into_vec(task.compression, &mut cache.comp, &msg[16..len], reg_len)?;
                Ok(DecompReturn::Shm(ApplyTask {
                    sequence: task.sequence.unwrap(),
                    data: fill,
                    is_diff_type: false,
                    ntrailing: 0,
                    target: ApplyTaskTarget::MirrorOnly(target.clone()),
                    region_start,
                    region_end,
                    remote_id,
                }))
            }
        }
    } else {
        unreachable!();
    }
}

 
fn run_apply_task(task: &ApplyTask, cache: &mut ThreadCache) -> TaskResult {
    match task.target {
        ApplyTaskTarget::MirrorOnly(ref d) => {
            if task.is_diff_type {
                let v = d
                    .mirror
                    .get_mut_range(task.region_start..task.region_end)
                    .ok_or_else(|| {
                        tag!(
                            "Failed to get mirror segment {}..{} from mirror of length {}",
                            task.region_start,
                            task.region_end,
                            d.mirror.len(),
                        )
                    })?;
                apply_diff_one(&task.data, task.ntrailing, task.region_start, v.data)?;
            } else {
                let v = d
                    .mirror
                    .get_mut_range(task.region_start..task.region_end)
                    .ok_or_else(|| {
                        tag!(
                            "Failed to get mirror segment {}..{} from mirror of length {}",
                            task.region_start,
                            task.region_end,
                            d.mirror.len(),
                        )
                    })?;
                v.data.copy_from_slice(&task.data);
            }

            if d.notify_on_completion {
                Ok(TaskOutput::ApplyDone(task.remote_id))
            } else {
                Ok(TaskOutput::MirrorApply)
            }
        }
        ApplyTaskTarget::Dmabuf(ref d) => {
            assert!(
                task.region_start <= d.orig_start
                    && d.orig_start <= d.orig_end
                    && d.orig_end <= task.region_end
            );
            let copy_handle = if task.is_diff_type {
                let mirror = d.target.mirror.as_ref().unwrap();
                let v = mirror
                    .get_mut_range(task.region_start..task.region_end)
                    .ok_or_else(|| {
                        tag!(
                            "Failed to get mirror segment {}..{}",
                            task.region_start,
                            task.region_end
                        )
                    })?;

                 
                apply_diff_one(
                    &task.data,
                    task.ntrailing,
                    d.orig_start,
                    &mut v.data
                        [(d.orig_start - task.region_start)..(d.orig_end - task.region_start)],
                )?;

                 
                let write_len = task.region_end - task.region_start;
                let write_buf = Arc::new(vulkan_get_buffer(&d.target.dst.vulk, write_len, false)?);
                let write_view = write_buf.get_write_view();
                write_view.data.copy_from_slice(v.data);
                drop(write_view);
                write_buf.complete_write()?;

                let pool: &Arc<VulkanCommandPool> = cache.get_cmd_pool(&d.target.dst.vulk)?;
                let copy_handle: VulkanCopyHandle = start_copy_segments_onto_dmabuf(
                    &d.target.dst,
                    &write_buf,
                    pool,
                    &[(0, task.region_start as u32, task.region_end as u32)],
                    d.target.view_row_stride,
                    &[],
                )?;

                copy_handle
            } else {
                let mirror = d.target.mirror.as_ref().unwrap();
                let v = mirror
                    .get_mut_range(task.region_start..task.region_end)
                    .ok_or_else(|| {
                        tag!(
                            "Failed to get mirror segment: {}..{} (orig {}..{}); bufsize {}",
                            task.region_start,
                            task.region_end,
                            d.orig_start,
                            d.orig_end,
                            d.target.dst.nominal_size(d.target.view_row_stride)
                        )
                    })?;

                let write_len = task.region_end - task.region_start;
                let write_buf = Arc::new(vulkan_get_buffer(&d.target.dst.vulk, write_len, false)?);
                let write_view = write_buf.get_write_view();

                write_view.data[0..d.orig_start - task.region_start]
                    .copy_from_slice(&v.data[0..d.orig_start - task.region_start]);
                write_view.data[d.orig_start - task.region_start..(d.orig_end - task.region_start)]
                    .copy_from_slice(&task.data);
                write_view.data
                    [(d.orig_end - task.region_start)..(task.region_end - task.region_start)]
                    .copy_from_slice(
                        &v.data[(d.orig_end - task.region_start)
                            ..(task.region_end - task.region_start)],
                    );

                drop(write_view);
                write_buf.complete_write()?;

                let pool: &Arc<VulkanCommandPool> = cache.get_cmd_pool(&d.target.dst.vulk)?;
                let copy_handle: VulkanCopyHandle = start_copy_segments_onto_dmabuf(
                    &d.target.dst,
                    &write_buf,
                    pool,
                    &[(0, task.region_start as u32, task.region_end as u32)],
                    d.target.view_row_stride,
                    &[],
                )?;

                 
                v.data[d.orig_start - task.region_start..(d.orig_end - task.region_start)]
                    .copy_from_slice(&task.data);

                copy_handle
            };

            let copy_id = copy_handle.get_timeline_point();
            cache.copy_ops.push(copy_handle);
            Ok(TaskOutput::DmabufApplyOp((copy_id, task.remote_id)))
        }
        ApplyTaskTarget::Shm(ref d) => {
            if task.is_diff_type {
                debug!(
                    "Applying diff: seq: {} len: {} ntrailing {} region {}..{} buflen {}",
                    task.sequence,
                    task.data.len(),
                    task.ntrailing,
                    task.region_start,
                    task.region_end,
                    d.target.mapping.get_u8().len()
                );
                 
                let m = d
                    .target
                    .mem_mirror
                    .get_mut_range(task.region_start..task.region_end)
                    .ok_or_else(|| {
                        tag!(
                            "Failed to acquire mirror range {}..{} when applying",
                            task.region_start,
                            task.region_end
                        )
                    })?;
                apply_diff(
                    &task.data,
                    task.ntrailing,
                     
                    &d.target.mapping,
                    task.region_start,
                    m.data,
                )?;
            } else {
                debug!(
                    "Applying fill: seq: {} len: {} region {}..{} buflen {}",
                    task.sequence,
                    task.data.len(),
                    task.region_start,
                    task.region_end,
                    d.target.mapping.get_u8().len()
                );
                copy_onto_mapping(&task.data, &d.target.mapping, task.region_start);
                if !d.skip_mirror {
                    let m = d
                        .target
                        .mem_mirror
                        .get_mut_range(task.region_start..task.region_end)
                        .ok_or_else(|| {
                            tag!(
                                "Failed to acquire mirror range {}..{} when applying",
                                task.region_start,
                                task.region_end
                            )
                        })?;
                    m.data.copy_from_slice(&task.data);
                }
            }

            Ok(TaskOutput::ApplyDone(task.remote_id))
        }
    }
}

 
fn run_encode_task(task: VideoEncodeTask, cache: &mut ThreadCache) -> TaskResult {
    let pool: &Arc<VulkanCommandPool> = cache.get_cmd_pool(&task.vulk)?;
    let packet = start_dmavid_encode(
        &task.state,
        pool,
        &task.acquires.explicit[..],
        task.acquires.implicit.as_slice(),
    )?;

    let npadding = align4(packet.len()) - packet.len();
    let update_header = cat2x4(
        build_wmsg_header(WmsgType::SendDMAVidPacket, 8 + packet.len()).to_le_bytes(),
        task.remote_id.0.to_le_bytes(),
    );
     
    let mut msg = Vec::from(update_header);
    msg.extend_from_slice(&packet);
    if npadding > 0 {
        msg.extend_from_slice(&vec![0; npadding]);
    }
    Ok(TaskOutput::Msg(msg))
}

 
fn run_decode_task(task: VideoDecodeTask, cache: &mut ThreadCache) -> TaskResult {
    let pool: &Arc<VulkanCommandPool> = cache.get_cmd_pool(&task.vulk)?;
    let msg = &task.msg.get();
    let (len, _t) = parse_wmsg_header(u32::from_le_bytes(msg[..4].try_into().unwrap())).unwrap();
    let packet = &msg[8..len];
    let decode_handle = start_dmavid_apply(&task.state, pool, packet)?;

    let completion_point = decode_handle.get_timeline_point();
    cache.decode_ops.push(decode_handle);
    Ok(TaskOutput::DmabufApplyOp((
        completion_point,
        task.remote_id,
    )))
}

 
fn process_channel(
    chan_msg: &mut FromChannel,
    glob: &mut Globals,
    tasksys: &TaskSystem,
) -> Result<(), String> {
    debug!("Process channel");

    loop {
        let Some(ref mut msg_view) = &mut chan_msg.next_msg else {
             
            break;
        };

        let data = msg_view.get_mut();
        let header = u32::from_le_bytes(data[0..4].try_into().unwrap());
        let (length, typ) = parse_wmsg_header(header)
            .ok_or_else(|| tag!("Failed to parse wmsg header: {:x}", header))?;
        if typ != WmsgType::Close && typ != WmsgType::AckNblocks && typ != WmsgType::Restart {
            chan_msg.message_counter += 1;
        }
         
        let msg = &mut data[..length];
        debug!("Received {:?} message of length {}", typ, length);

        let is_first = if !glob.has_first_message {
            glob.has_first_message = true;
            true
        } else {
            false
        };

        match typ {
            WmsgType::Version => {
                let version = u32::from_le_bytes(data[4..8].try_into().unwrap());
                if version > WAYPIPE_PROTOCOL_VERSION {
                    return Err(tag!(
                        "waypipe client replied with larger protocol version ({}) than requested ({})", version, WAYPIPE_PROTOCOL_VERSION
                    ));
                }
                if version < MIN_PROTOCOL_VERSION {
                    return Err(tag!(
                        "waypipe client requested too small of a version: {}",
                        version
                    ));
                }
                if !is_first {
                    return Err(tag!(
                        "Version message must be the first sent by waypipe-client"
                    ));
                }
                if glob.on_display_side {
                    return Err(tag!("waypipe-server should not send Version message"));
                }
                glob.wire_version = version;
                debug!("Wire version has been set to: {}", version);
            }

            WmsgType::InjectRIDs => {
                 
                if length % 4 != 0 {
                    return Err(tag!("InjectRIDs length {} not divisible by four", length));
                }
                let nnew = (length - 4) / 4;
                for i in 0..nnew {
                    let rid = Rid(i32::from_le_bytes(
                        msg[4 + 4 * i..8 + 4 * i].try_into().unwrap(),
                    ));
                     
                    let sfd = glob.fresh.remove(&rid).ok_or_else(|| {
                        tag!(
                            "Injecting RID {} which has no matching created ShadowFd",
                            rid
                        )
                    })?;

                    chan_msg.rid_queue.push_back(sfd);
                }
            }
            WmsgType::Protocol => {
                 
                assert!(chan_msg.output.start == 0);

                let mut msg_region = &msg[4..];
                if msg_region.is_empty() {
                    debug!("Note: received empty protocol message");
                }

                while !msg_region.is_empty() {
                    if msg_region.len() < 8 {
                        return Err(tag!("Truncated Wayland message inside Protocol message"));
                    }
                    let header2 = u32::from_le_bytes(msg_region[4..8].try_into().unwrap());
                    let length = (header2 >> 16) as usize;
                    if length < 8 || length % 4 != 0 || length > msg_region.len() {
                        return Err(tag!("invalid Wayland message: bad length field: {} (compare region length {})", length, msg_region.len()));
                    }
                    let (waymsg, tail) = msg_region.split_at(length);

                    let mut chan_out_tail = &mut chan_msg.output.data[chan_msg.output.len..];
                    let orig_tail_len = chan_out_tail.len();

                    let aux = TranslationInfo::FromChannel((
                        &mut chan_msg.rid_queue,
                        &mut chan_msg.output.fds,
                    ));
                    let ret = process_way_msg(waymsg, &mut chan_out_tail, aux, glob)?;
                    let new_dst_len = chan_out_tail.len();
                    match ret {
                        ProcMsg::Done => {
                            log_way_msg_output(
                                waymsg,
                                &chan_msg.output.data
                                    [chan_msg.output.len..chan_msg.output.data.len() - new_dst_len],
                                &glob.objects,
                                glob.on_display_side,
                            );
                        }
                        ProcMsg::WaitFor(r) => {
                            chan_msg.waiting_for = Some(r);
                            break;
                        }
                        ProcMsg::NeedsSpace((nbytes, nfds)) => {
                            if nbytes > chan_msg.output.data.len() || nfds > MAX_OUTGOING_FDS {
                                 
                                return Err(tag!("Failed to send message(s): not enough space, ({}, {}) vs ({}, {})",
                                    nbytes, nfds, chan_msg.output.data.len(), MAX_OUTGOING_FDS));
                            }
                            debug!("Skipping last message: not enough space");
                             
                            break;
                        }
                    }
                     
                    chan_msg.output.len += orig_tail_len - new_dst_len;

                     
                    msg_region = tail;
                }
                let unproc = msg_region.len();
                 
                if unproc > 0 {
                    debug!(
                        "Adjusting protocol message for {} unprocessed bytes",
                        unproc
                    );
                    let trail_length = unproc + 4;
                    let skip = msg.len() - trail_length;

                    msg[skip..skip + 4].copy_from_slice(
                        &build_wmsg_header(WmsgType::Protocol, trail_length).to_le_bytes(),
                    );
                    chan_msg.message_counter -= 1;

                     
                    msg_view.advance(skip);
                     

                     
                    return Ok(());
                }
                 
            }
            WmsgType::AckNblocks => {
                 
                if glob.wire_version > 16 {
                    return Err(tag!("Received AckNBlocks message, but reconnection support is explicitly disabled at wire version {}", glob.wire_version));
                }
            }
            WmsgType::Restart => {
                return Err("Unsupported Restart message".into());
            }
            WmsgType::Close => {
                glob.has_received_close = true;
            }
            _ => {
                let mut tmp = None;
                std::mem::swap(&mut tmp, &mut chan_msg.next_msg);
                process_sfd_msg(typ, length, tmp.unwrap(), glob, tasksys)?;
            }
        }

        chan_msg.next_msg = chan_msg.input.pop_next_msg();
    }
    Ok(())
}

 
fn process_wayland_1(
    way_msg: &mut FromWayland,
    glob: &mut Globals,
    tasksys: &TaskSystem,
) -> Result<(), String> {
    debug!("Process wayland 1: {} bytes", way_msg.input.len);

     
    let max_len = way_msg.input.data.len();
    let mut region: &[u8] = &way_msg.input.data[..way_msg.input.len];
    let mut nread: usize = 0;
    loop {
        if region.len() < 8 {
             
            break;
        }
        let header1 = u32::from_le_bytes(region[0..4].try_into().unwrap());
        let header2 = u32::from_le_bytes(region[4..8].try_into().unwrap());
        let length = (header2 >> 16) as usize;
        if length < 8 || length % 4 != 0 {
            error!("Bad length field: {}", length);
            return Err(tag!("invalid Wayland message: bad length field {}", length));
        }
        if length >= max_len {
            return Err(tag!(
                "Message to object {} (length {}) is longer than {}-byte receive buffer",
                header1,
                length,
                max_len
            ));
        }

        if length > region.len() {
             
            break;
        }
        let (msg, tail) = region.split_at(length);
        region = tail;

        let data_max_len = way_msg.output.protocol_data.len();
        let mut dst = &mut way_msg.output.protocol_data[way_msg.output.protocol_len..];
        let orig_dst_len = dst.len();

        let aux = TranslationInfo::FromWayland((
            &mut way_msg.input.fds,
            &mut way_msg.output.protocol_rids,
        ));
        let ret = process_way_msg(msg, &mut dst, aux, glob)?;
        let new_dst_len = dst.len();
        match ret {
            ProcMsg::Done => {
                log_way_msg_output(
                    msg,
                    &way_msg.output.protocol_data[way_msg.output.protocol_len
                        ..way_msg.output.protocol_data.len() - new_dst_len],
                    &glob.objects,
                    !glob.on_display_side,
                );
            }
            ProcMsg::WaitFor(_) => {
                unreachable!("Unexpected ProcMsg::WaitFor")
            }
            ProcMsg::NeedsSpace((nbytes, nfds)) => {
                if nbytes > data_max_len || nfds > MAX_OUTGOING_FDS {
                     
                    return Err(tag!(
                        "Failed to send message(s): not enough space, ({}, {}) vs ({}, {})",
                        nbytes,
                        nfds,
                        way_msg.output.protocol_data.len(),
                        MAX_OUTGOING_FDS
                    ));
                }
                debug!(
                    "Skipping last message: not enough space ({},{}) vs ({},{})",
                    nbytes,
                    nfds,
                    new_dst_len,
                    MAX_OUTGOING_FDS - way_msg.output.protocol_rids.len()
                );
                break;
            }
        }

         
        nread += length;
        way_msg.output.protocol_len += orig_dst_len - new_dst_len;
    }
    way_msg.input.len -= nread;
    way_msg
        .input
        .data
        .copy_within(nread..(nread + way_msg.input.len), 0);

     
    let comp = glob.opts.compression;
    let mut drop_list: Vec<Rid> = Vec::new();
    for (rid, sfd) in &glob.map {
         
        let Some(s) = sfd.upgrade() else {
             
            continue;
        };

        let keep = collect_updates(
            &mut s.borrow_mut(),
            &mut way_msg.output,
            comp,
            tasksys,
            &glob.opts,
        )?;
        if !keep {
            drop_list.push(*rid);
        }
    }

    let mut delete_idxs: Vec<usize> = Vec::new();
    for (i, sfd) in glob.pipes.iter().enumerate() {
        let rid = sfd.borrow().remote_id;
        if drop_list.contains(&rid) {
            delete_idxs.push(i);
        }
    }

    for drop_pos in delete_idxs.iter().rev() {
        glob.pipes.remove(*drop_pos);
    }

    for rid in drop_list {
        debug!("Dropping RID {} from pipe list", rid);
        glob.map.remove(&rid);
    }

    Ok(())
}

 
fn process_wayland_2(way_msg: &mut FromWayland) {
     

    if !way_msg.output.protocol_rids.is_empty() {
        debug!(
            "Inserting RID message with {} rids",
            way_msg.output.protocol_rids.len()
        );
        let len = way_msg.output.protocol_rids.len() * 4 + 4;
        let rid_header: u32 = build_wmsg_header(WmsgType::InjectRIDs, len);

        let mut v = Vec::with_capacity(len);
        for e in rid_header.to_le_bytes() {
            v.push(e);
        }
        for rid in &way_msg.output.protocol_rids {
            let r = rid.borrow().remote_id;
            for e in r.0.to_le_bytes() {
                v.push(e);
            }
        }
        way_msg.output.other_messages.push(v);

        way_msg.output.protocol_rids.clear();
    }

    if way_msg.output.protocol_len > 0 && !way_msg.output.protocol_header_added {
        debug!(
            "Inserting protocol header, for {} bytes of content",
            way_msg.output.protocol_len
        );
        let proto_header: u32 = build_wmsg_header(
            WmsgType::Protocol,
            way_msg.output.protocol_len + std::mem::size_of::<u32>(),
        );
        let mut v = Vec::new();
        for e in proto_header.to_le_bytes() {
            v.push(e);
        }
        way_msg.output.other_messages.push(v);

        way_msg.output.protocol_header_added = true;

         
         
    }
}

 
fn process_vulkan_updates(
    glob: &mut Globals,
    tasksys: &TaskSystem,
    from_chan: &mut FromChannel,
) -> Result<(), String> {
    let DmabufDevice::Vulkan((_, ref vulk)) = glob.dmabuf_device else {
        unreachable!();
    };
    let current: u64 = vulk.get_current_timeline_pt()?;

    let mut g = tasksys.tasks.lock().unwrap();
     
     
     

    retain_err(&mut g.apply_operations, |x| -> Result<bool, String> {
        if x.0 <= current {
            let rid = x.1;
            let Some(wsfd) = glob.map.get(&rid) else {
                return Err(tag!("ShadowFd no longer in map"));
            };
            let Some(sfd) = wsfd.upgrade() else {
                return Err(tag!("ShadowFd no longer strongly referenced"));
            };
            let mut b = sfd.borrow_mut();
            let rid = b.remote_id;
            let ShadowFdVariant::Dmabuf(ref mut data) = &mut b.data else {
                panic!();
            };
            data.pending_apply_tasks = data
                .pending_apply_tasks
                .checked_sub(1)
                .ok_or("Task miscount")?;

             
            if data.pending_apply_tasks == 0 {
                debug!(
                    "Tasks completed, signalling {} acquires for (dmabuf rid={}, wlbuf={})",
                    data.acquires.len(),
                    rid,
                    data.debug_wayland_id
                );
                if !data.acquires.is_empty() {
                    assert!(glob.on_display_side);
                    signal_timeline_acquires(&mut data.acquires)?;
                }
            }

            Ok(false)
        } else {
            Ok(true)
        }
    })?;

     
    if let Some(rid) = from_chan.waiting_for {
        let sfd = glob.map.get(&rid).unwrap().upgrade().unwrap();
        let b = sfd.borrow_mut();
        if let ShadowFdVariant::Dmabuf(ref data) = &b.data {
            if data.pending_apply_tasks == 0 {
                from_chan.waiting_for = None;
            }
        };
    }

    let mut new_tasks = false;
    for i in (0..g.dmabuf_fill_tasks.len()).rev() {
        if g.dmabuf_fill_tasks[i].wait_until <= current {
            let t = g.dmabuf_fill_tasks.remove(i);
            g.construct.push_back(WorkTask::FillDmabuf2(t));
            new_tasks = true;
        }
    }
    for i in (0..g.dmabuf_diff_tasks.len()).rev() {
        if g.dmabuf_diff_tasks[i].wait_until <= current {
            let t = g.dmabuf_diff_tasks.remove(i);
            g.construct.push_back(WorkTask::DiffDmabuf2(t));
            new_tasks = true;
        }
    }
    if new_tasks {
        tasksys.task_notify.notify_all();
    }

     
    Ok(())
}

 
fn is_from_way_processable(way_msg_input: &WaylandInputRing, glob: &Globals) -> bool {
     
    for sfd in &glob.pipes {
        let x = sfd.borrow();
        if let ShadowFdVariant::Pipe(data) = &x.data {
            if data.program_closed {
                 
                return true;
            }
            if let ShadowFdPipeBuffer::ReadFromWayland((_, len)) = data.buf {
                if len > 0 {
                     
                    return true;
                }
            }
        }
    }

     
    if way_msg_input.len < 8 {
        return false;
    }
    let header2 = u32::from_le_bytes(way_msg_input.data[4..8].try_into().unwrap());
    let length = header2 >> 16;
    if length >= way_msg_input.data.len() as u32 {
         
        return true;
    }
     
    way_msg_input.len >= length as usize
}

 
fn is_from_chan_processable(chan_msg: &FromChannel) -> bool {
    chan_msg.next_msg.is_some()
}
 
fn has_from_way_output(from_way_output: &TransferQueue) -> bool {
    let send_protocol =
        from_way_output.protocol_len > 0 && from_way_output.expected_recvd_msgs == 0;

    !from_way_output.other_messages.is_empty()
        || (send_protocol && from_way_output.protocol_len > 0)
        || from_way_output.needs_new_ack
        || from_way_output.ack_nwritten > 0
}
 
fn has_from_chan_output(from_chan_output: &TransferWayland) -> bool {
    from_chan_output.len > 0
}

 
fn wakeup_fd(fd: &OwnedFd, val: &[u8]) -> Result<(), ()> {
    loop {
        match unistd::write(fd, val) {
            Ok(_) => return Ok(()),
            Err(nix::errno::Errno::EINTR) => continue,
            Err(nix::errno::Errno::EAGAIN) => {
                 
                return Ok(());
            }
            Err(nix::errno::Errno::EPIPE) => {
                 
                return Err(());
            }
            Err(e) => {
                 
                 
                panic!("Pipe wakeup failed {:?}", e);
            }
        }
    }
}

 
fn interval_overlaps(a: &(usize, usize), b: &(usize, usize)) -> bool {
    b.0 < a.1 && a.0 < b.1
}

 
fn has_pending_compute_tasks(from_way_output: &TransferQueue) -> bool {
    from_way_output.expected_recvd_msgs > 0
}

 
fn pop_task(tasksys: &mut TaskSet) -> Option<WorkTask> {
     

     
    if let Some((_, t)) = tasksys.apply.first_key_value() {
         
        let mut ok: bool = true;
        for q in &tasksys.in_progress_decomp {
            if *q < t.sequence {
                ok = false;
                break;
            }
        }
        for (v, (l, h)) in &tasksys.in_progress_apply {
            if interval_overlaps(&(*l, *h), &(t.region_start, t.region_end)) {
                assert!(*v < t.sequence);
                ok = false;
                break;
            }
        }

        if ok {
            let (_, r) = tasksys.apply.pop_first().unwrap();
            tasksys
                .in_progress_apply
                .insert((r.sequence, (r.region_start, r.region_end)));
            return Some(WorkTask::Apply(r));
        }
    }

    if let Some(mut x) = tasksys.decompress.pop_front() {
        let s = tasksys.last_seqno;
        tasksys.last_seqno += 1;
        x.sequence = Some(s);
        tasksys.in_progress_decomp.insert(s);
        return Some(WorkTask::Decomp(x));
    }

     
    tasksys.construct.pop_front()
}

 
fn work_thread(tasksys: &TaskSystem, output: Sender<TaskResult>) {
    let notify: &Condvar = &tasksys.task_notify;
    let mtx: &Mutex<_> = &tasksys.tasks;

    let mut cache = ThreadCache {
        large: Vec::new(),
        cmd_pool: None,
        copy_ops: Vec::new(),
        decode_ops: Vec::new(),
        comp: ThreadCacheComp {
            lz4_c: None,
            zstd_c: None,
            zstd_d: None,
        },
    };

    let mut guard = mtx.lock().unwrap();
    while !guard.stop {
         
        let Some(task) = pop_task(&mut guard) else {
             
            guard = match notify.wait(guard) {
                Ok(g) => g,
                Err(_) => {
                    error!("Mutex poisoned, stopping worker");
                    break;
                }
            };
            continue;
        };
        drop(guard);

         
        match task {
            WorkTask::Diff(x) => {
                let result = run_diff_task(&x, &mut cache);
                 
                output.send(result).unwrap();
            }
            WorkTask::DiffDmabuf(x) => {
                let result = run_diff_dmabuf_task(x, &mut cache);
                match result {
                    Err(z) => {
                        output.send(Err(z)).unwrap();
                    }
                    Ok(t) => {
                        let mut g = mtx.lock().unwrap();
                        g.dmabuf_diff_tasks.push(t);
                        drop(g);
                    }
                }
            }
            WorkTask::DiffDmabuf2(x) => {
                let result = run_diff_dmabuf_task_2(x, &mut cache);
                 
                output.send(result).unwrap();
            }
            WorkTask::FillDmabuf(x) => {
                let result = run_fill_dmabuf_task(x, &mut cache);
                match result {
                    Err(z) => {
                        output.send(Err(z)).unwrap();
                    }
                    Ok(t) => {
                        let mut g = mtx.lock().unwrap();
                        g.dmabuf_fill_tasks.push(t);
                        drop(g);
                    }
                }
            }
            WorkTask::FillDmabuf2(x) => {
                let result = run_dmabuf_fill_task_2(x, &mut cache);
                 
                output.send(result).unwrap();
            }
            WorkTask::Decomp(x) => {
                let y = run_decomp_task(&x, &mut cache);
                match y {
                    Err(z) => {
                        output.send(Err(z)).unwrap();
                    }
                    Ok(t) => {
                        let mut g = mtx.lock().unwrap();
                         
                        g.in_progress_decomp.remove(&x.sequence.unwrap());
                        match t {
                            DecompReturn::Shm(x) => {
                                g.apply.insert(x.sequence, x);
                            }
                            DecompReturn::Dmabuf((seq, rid, mir_task)) => {
                                 

                                 
                                 
                                g.apply_operations.push((seq, rid));
                                if let Some(m) = mir_task {
                                    g.apply.insert(m.sequence, m);
                                }
                            }
                        }
                        drop(g);
                    }
                }
            }
            WorkTask::Apply(x) => {
                let y = run_apply_task(&x, &mut cache);
                let mut g = mtx.lock().unwrap();
                 
                if let Ok(TaskOutput::DmabufApplyOp(x)) = y {
                     
                    g.apply_operations.push(x);
                } else if let Ok(TaskOutput::MirrorApply) = y {
                     
                } else {
                     
                    output.send(y).unwrap();
                }
                g.in_progress_apply
                    .remove(&(x.sequence, (x.region_start, x.region_end)));
                drop(g);
            }

            WorkTask::VideoEncode(x) => {
                let y = run_encode_task(x, &mut cache);
                 
                output.send(y).unwrap();
            }
            WorkTask::VideoDecode(x) => {
                let y = run_decode_task(x, &mut cache);
                if let Ok(TaskOutput::DmabufApplyOp(x)) = y {
                     
                    let mut g = mtx.lock().unwrap();
                    g.apply_operations.push(x);
                    drop(g);
                } else {
                     
                    output.send(y).unwrap();
                }
            }
        };

         
         
         
         
         
        let zero = [0];
        if wakeup_fd(&tasksys.wake_fd, &zero).is_err() {
             
            break;
        }

         
        if let Some(c) = &cache.cmd_pool {
            let current: u64 = c.vulk.get_current_timeline_pt().unwrap();

             
            for i in (0..cache.copy_ops.len()).rev() {
                if cache.copy_ops[i].get_timeline_point() <= current {
                    cache.copy_ops.remove(i);
                }
            }
            for i in (0..cache.decode_ops.len()).rev() {
                if cache.decode_ops[i].get_timeline_point() <= current {
                    cache.decode_ops.remove(i);
                }
            }
        }

        guard = mtx.lock().unwrap();
    }

    if !cache.copy_ops.is_empty() || !cache.decode_ops.is_empty() {
        let final_pt = cache
            .copy_ops
            .iter()
            .map(|x| x.get_timeline_point())
            .chain(cache.decode_ops.iter().map(|x| x.get_timeline_point()))
            .max()
            .unwrap();

        debug!(
            "Work thread {} waiting for Vulkan timeline point {}",
            std::thread::current().name().unwrap_or("unknown"),
            final_pt,
        );
        cache
            .cmd_pool
            .as_ref()
            .unwrap()
            .vulk
            .wait_for_timeline_pt(final_pt, u64::MAX)
            .unwrap();
    }
    debug!(
        "Work thread {} complete",
        std::thread::current().name().unwrap_or("unknown")
    );
}

 
fn vulkan_wait_thread(state: Arc<VulkanWaitThread>, vulk: Arc<VulkanDevice>) {
    let notify: &Condvar = &state.notify;
    let mtx: &Mutex<_> = &state.inner;

    debug!(
        "Vulkan waiting thread {} started",
        std::thread::current().name().unwrap_or("unknown")
    );

    let mut guard = mtx.lock().unwrap();
    while !guard.stop {
        let Some(pt) = guard.target_sequence_point else {
             
            guard = match notify.wait(guard) {
                Ok(g) => g,
                Err(_) => {
                    error!("Mutex poisoned, stopping Vulkan waiter thread");
                    break;
                }
            };
            continue;
        };

        drop(guard);
         
        let res = vulk.wait_for_timeline_pt(pt, u64::MAX);
        guard = mtx.lock().unwrap();

         
        if !guard.pending_read {
             
            let long_zero = [0u8; 8];
            if wakeup_fd(&guard.pipe_w, &long_zero).is_err() {
                 
                break;
            }
            guard.pending_read = true;
        }

        match res {
            Err(e) => {
                guard.error = Some(e);
                break;
            }
            Ok(true) => {
                 
                guard.target_sequence_point = None;
            }
             
            Ok(false) => (),
        }
    }

    debug!(
        "Vulkan waiting thread {} complete",
        std::thread::current().name().unwrap_or("unknown")
    );
}

impl Drop for VulkanWaitThreadShutdown {
    fn drop(&mut self) {
        match self.state.inner.lock() {
            Ok(mut guard) => {
                guard.stop = true;
            }
            Err(_) => {
                error!("Mutex poisoned, Vulkan wait thread expected to shut down");
            }
        }
        self.state.notify.notify_all();
    }
}

 
fn loop_inner<'a>(
    scope: &'a std::thread::Scope<'a, '_>,
    glob: &mut Globals,
    from_chan: &mut FromChannel,
    from_way: &mut FromWayland,
    tasksys: &TaskSystem,
    wake_r: OwnedFd,
    chanfd: &OwnedFd,
    progfd: &OwnedFd,
    pollmask: &signal::SigSet,
    sigint_received: &AtomicBool,
) -> Result<(), String> {
    let mut vulkan_wait_state: Option<Arc<VulkanWaitThread>> = None;
    let mut _vulkan_wait_thread_shutdown: Option<VulkanWaitThreadShutdown> = None;
    let mut vulkan_wait_pipe_r: Option<OwnedFd> = None;

    while from_chan.state != DirectionState::Off || from_way.state != DirectionState::Off {
        let has_chan_output =
            has_from_way_output(&from_way.output) && from_way.state != DirectionState::Off;
        let has_way_output =
            has_from_chan_output(&from_chan.output) && from_chan.state != DirectionState::Off;

        let read_chan_input = !is_from_chan_processable(from_chan)
            && from_chan.state == DirectionState::On
            && !has_from_chan_output(&from_chan.output);
        let read_way_input = !is_from_way_processable(&from_way.input, glob)
            && from_way.state == DirectionState::On
            && !has_from_way_output(&from_way.output)
            && !has_pending_compute_tasks(&from_way.output);

         
        let work_way = is_from_way_processable(&from_way.input, glob)
            && !has_pending_compute_tasks(&from_way.output)
            && (from_way.state == DirectionState::On || from_way.state == DirectionState::Drain);
        let work_chan = is_from_chan_processable(from_chan)
            && from_chan.waiting_for.is_none()
            && (from_chan.state == DirectionState::On || from_chan.state == DirectionState::Drain);
        let work_to_do_now = (work_way && !has_chan_output) || (work_chan && !has_way_output);

        debug!(
            "poll: from_chan ({:?}{}{}{}; wait={}) from_way ({:?}{}{}{}; wait={}) work now {}",
            from_chan.state,
            string_if_bool(is_from_chan_processable(from_chan), ",proc"),
            string_if_bool(has_from_chan_output(&from_chan.output), ",output"),
            string_if_bool(read_chan_input, ",read"),
            from_chan.waiting_for.unwrap_or(Rid(0)),
            from_way.state,
            string_if_bool(is_from_way_processable(&from_way.input, glob), ",proc"),
            string_if_bool(has_from_way_output(&from_way.output), ",output"),
            string_if_bool(read_way_input, ",read"),
            from_way.output.expected_recvd_msgs,
            fmt_bool(work_to_do_now)
        );

         
         
         
         

        let mut pfds = Vec::new();  
        pfds.push(PollFd::new(wake_r.as_fd(), PollFlags::POLLIN));

        let chan_id: Option<usize> = if read_chan_input || has_chan_output {
            let mut flags = PollFlags::empty();
            flags.set(PollFlags::POLLIN, read_chan_input);
            flags.set(PollFlags::POLLOUT, has_chan_output);
            pfds.push(PollFd::new(chanfd.as_fd(), flags));
            Some(pfds.len() - 1)
        } else {
            None
        };
        let prog_id: Option<usize> = if read_way_input || has_way_output {
            let mut flags = PollFlags::empty();
            flags.set(PollFlags::POLLIN, read_way_input);
            flags.set(PollFlags::POLLOUT, has_way_output);
            pfds.push(PollFd::new(progfd.as_fd(), flags));
            Some(pfds.len() - 1)
        } else {
            None
        };

        let (vulk_id, borrowed_fd): (Option<usize>, Option<BorrowedFd>) =
            if let DmabufDevice::Vulkan((_, ref vulk)) = glob.dmabuf_device {
                let g = tasksys.tasks.lock().unwrap();
                let mut first_pt = u64::MAX;
                first_pt = std::cmp::min(
                    first_pt,
                    g.apply_operations
                        .iter()
                        .map(|x| x.0)
                        .min()
                        .unwrap_or(u64::MAX),
                );
                first_pt = std::cmp::min(
                    first_pt,
                    g.dmabuf_fill_tasks
                        .iter()
                        .map(|x| x.wait_until)
                        .min()
                        .unwrap_or(u64::MAX),
                );
                first_pt = std::cmp::min(
                    first_pt,
                    g.dmabuf_diff_tasks
                        .iter()
                        .map(|x| x.wait_until)
                        .min()
                        .unwrap_or(u64::MAX),
                );

                if first_pt < u64::MAX {
                     
                    let mut flags = PollFlags::empty();
                    flags.set(PollFlags::POLLIN, true);
                    let obfd = vulk.get_event_fd(first_pt).unwrap();
                    if let Some(bfd) = obfd {
                        pfds.push(PollFd::new(bfd, flags));
                        (Some(pfds.len() - 1), Some(bfd))
                    } else {
                         
                        if let Some(ref w) = vulkan_wait_state {
                             
                            w.inner.lock().unwrap().target_sequence_point = Some(first_pt);
                            w.notify.notify_all();
                        } else {
                            let (wake_r, wake_w) =
                                crate::util::create_pipe()
                                    .map_err(|x| tag!("Failed to create pipe: {}", x))?;

                            let wait_state = Arc::new(VulkanWaitThread {
                                inner: Mutex::new(VulkanWaitThreadInner {
                                    pipe_w: wake_w,
                                    pending_read: false,
                                    target_sequence_point: Some(first_pt),
                                    stop: false,
                                    error: None,
                                }),
                                notify: Condvar::new(),
                            });
                            let shutdown = VulkanWaitThreadShutdown {
                                state: wait_state.clone(),
                            };
                            _vulkan_wait_thread_shutdown = Some(shutdown);

                            let vkclone = vulk.clone();
                            let waitclone = wait_state.clone();
                            let thread = std::thread::Builder::new()
                                .name(format!(
                                    "{}-vulkan-wait",
                                    if glob.on_display_side { "c" } else { "s" },
                                ))
                                .spawn_scoped(scope, move || vulkan_wait_thread(waitclone, vkclone))
                                .map_err(|x| tag!("Failed to spawn thread: {:?}", x))?;

                             
                            drop(thread);

                            vulkan_wait_state = Some(wait_state);
                            vulkan_wait_pipe_r = Some(wake_r);
                        }

                        let bfd = vulkan_wait_pipe_r.as_ref().unwrap().as_fd();
                        pfds.push(PollFd::new(bfd, flags));
                        (Some(pfds.len() - 1), Some(bfd))
                    }
                } else {
                    (None, None)
                }
            } else {
                (None, None)
            };

        let nbase_fds = pfds.len();

         
         
        let sfd_refs: Vec<std::cell::Ref<'_, ShadowFd>> =
            glob.pipes.iter().map(|v| v.borrow()).collect();
        for x in &sfd_refs {
            if let ShadowFdVariant::Pipe(data) = &x.data {
                if data.program_closed {
                    continue;
                }
                match &data.buf {
                    ShadowFdPipeBuffer::ReadFromWayland((buf, len)) => {
                        if *len < buf.len() {
                            pfds.push(PollFd::new(data.fd.as_fd(), PollFlags::POLLIN));
                        }
                    }
                    ShadowFdPipeBuffer::ReadFromChannel(v) => {
                        if !v.is_empty() {
                            pfds.push(PollFd::new(data.fd.as_fd(), PollFlags::POLLOUT));
                        }
                    }
                };
            }
        }

        let ntimelinebase_fds = pfds.len();
        let mut timelines = Vec::new();
        for (_rid, v) in glob.map.iter() {
            let Some(w) = v.upgrade() else {
                continue;
            };
            let b = w.borrow();
            let ShadowFdVariant::Timeline(ref data) = b.data else {
                continue;
            };
            if !glob.on_display_side {
                 
                continue;
            }
            let Some(min_pt) = data.releases.iter().map(|x| x.0).min() else {
                continue;
            };
            drop(b);
            timelines.push((w, min_pt));
        }
        let timeline_refs: Vec<(std::cell::Ref<'_, ShadowFd>, u64)> =
            timelines.iter().map(|v| (v.0.borrow(), v.1)).collect();
        for (b, pt) in timeline_refs.iter() {
            let ShadowFdVariant::Timeline(ref data) = b.data else {
                continue;
            };
            let evfd = data.timeline.link_event_fd(*pt)?;
            pfds.push(PollFd::new(evfd, PollFlags::POLLIN));
        }

         
        let ndmabufbase_fds = pfds.len();
        let mut dmabufs = Vec::new();
        {
            let g = tasksys.tasks.lock().unwrap();

            let mut dmabuf_rids: Vec<Rid> = Vec::new();
            dmabuf_rids.extend(g.waiting_for_implicit_acquire.keys().copied());
            for rid in dmabuf_rids {
                let Some(wsfd) = glob.map.get(&rid) else {
                    error!(
                        "ShadowFd with RID={} for dmabuf destroyed before task completion",
                        rid
                    );
                    continue;
                };
                let Some(sfd) = wsfd.upgrade() else {
                    error!(
                        "ShadowFd with RID={} for dmabuf destroyed before task completion",
                        rid
                    );
                    continue;
                };
                dmabufs.push(sfd);
            }
        }
        let dmabuf_refs: Vec<std::cell::Ref<'_, ShadowFd>> =
            dmabufs.iter().map(|v| v.borrow()).collect();
        for d in dmabuf_refs.iter() {
            let ShadowFdVariant::Dmabuf(ref data) = d.data else {
                unreachable!();
            };
            let DmabufImpl::Vulkan(ref v) = data.buf else {
                 
                unreachable!();
            };
            let dfd = v.main_fd.as_fd();
            pfds.push(PollFd::new(dfd, PollFlags::POLLIN));
        }

        let zero_timeout = time::TimeSpec::new(0, 0);
        let res = crate::util::sys_poll(
            &mut pfds,
            if work_to_do_now {
                Some(zero_timeout)
            } else {
                None
            },
            Some(*pollmask),
        );

        if let Err(errno) = res {
            assert!(errno == Errno::EINTR || errno == Errno::EAGAIN);
        }

         
         
        let pfd_returns: Vec<PollFlags> = pfds.iter().map(|x| x.revents().unwrap()).collect();

        drop(pfds);
        drop(sfd_refs);
        drop(timeline_refs);
        drop(dmabuf_refs);

        if sigint_received.load(Ordering::Acquire) {
            error!("SIGINT");
            break;
        }

        let mut self_wakeup = false;
        if pfd_returns[0].contains(PollFlags::POLLIN) {
            debug!("Self-pipe wakeup");
            let mut tmp: [u8; 64] = [0; 64];
            let res = unistd::read(&wake_r, &mut tmp[..]);
            match res {
                Ok(_) => {
                     
                    self_wakeup = true;
                }
                Err(nix::errno::Errno::EINTR) | Err(nix::errno::Errno::EAGAIN) => {
                     
                }
                Err(code) => {
                    return Err(tag!("Failed to read from self-pipe: {:?}", code));
                }
            };
        }

        let (mut prog_read_eof, mut prog_write_eof) = (false, false);
        if let Some(id) = prog_id {
            let evts = pfd_returns[id];

            if evts.contains(PollFlags::POLLIN) {
                 
                prog_read_eof |= read_from_socket(progfd, &mut from_way.input)?;
                if prog_read_eof {
                    debug!("EOF reading from program");
                }
            }
            if evts.contains(PollFlags::POLLOUT) {
                 
                prog_write_eof |= write_to_socket(progfd, &mut from_chan.output)?;
                if prog_write_eof {
                    debug!("EOF writing to program");
                }
            }
            if evts.contains(PollFlags::POLLHUP) {
                 
                debug!("POLLHUP from wayland side");
                prog_write_eof = true;
            }
        }

        let (mut chan_read_eof, mut chan_write_eof) = (false, false);
        if let Some(id) = chan_id {
            let evts = pfd_returns[id];

            if evts.contains(PollFlags::POLLIN) {
                 
                chan_read_eof |= read_from_channel(chanfd, from_chan)?;
                if chan_read_eof {
                    debug!(
                        "EOF reading from channel: has close message? {}",
                        fmt_bool(glob.has_received_close)
                    );
                }
            }
            if evts.contains(PollFlags::POLLOUT) {
                 
                chan_write_eof |= write_to_channel(chanfd, &mut from_way.output)?;
                if chan_write_eof {
                    debug!(
                        "EOF writing to channel: has close message? {}",
                        fmt_bool(glob.has_received_close)
                    );
                }
            }
            if evts.contains(PollFlags::POLLHUP) {
                 
                debug!(
                    "POLLHUP from channel side: has close message? {}",
                    fmt_bool(glob.has_received_close)
                );
                chan_write_eof = true;
            }
        }

         
        if prog_read_eof || prog_write_eof {
             
            from_chan.state = DirectionState::Off;
        }
        if chan_read_eof || chan_write_eof {
             
            from_way.state = DirectionState::Off;
        }
        if prog_read_eof {
             
            from_way.state = match from_way.state {
                DirectionState::On | DirectionState::Drain => DirectionState::Drain,
                DirectionState::Off => DirectionState::Off,
            };
             
        }
        if chan_read_eof {
             
            from_chan.state = match from_chan.state {
                DirectionState::On | DirectionState::Drain => DirectionState::Drain,
                DirectionState::Off => DirectionState::Off,
            };
             
        }

         
        let mut timeline_update = false;
        if let Some(id) = vulk_id {
            let evts = pfd_returns[id];

            if evts.contains(PollFlags::POLLIN) {
                let mut data = [0u8; 8];
                let r = nix::unistd::read(borrowed_fd.unwrap(), &mut data);
                match r {
                    Ok(s) => {
                         
                        assert!(s == 8);
                         
                        timeline_update = true;

                        if let Some(ref w) = vulkan_wait_state {
                            let mut m = w.inner.lock().unwrap();
                            m.pending_read = false;
                            let mut tmp = None;
                            std::mem::swap(&mut tmp, &mut m.error);
                            if let Some(e) = tmp {
                                 
                                error!("Vulkan timeline waiting thread failed: {}", e);
                                return Err(e);
                            }
                        }
                    }
                    Err(nix::errno::Errno::EINTR) | Err(nix::errno::Errno::EAGAIN) => {
                         
                    }
                    Err(code) => {
                        return Err(tag!("Failed to read from eventfd: {:?}", code));
                    }
                }
            }
        }

         
        let mut base_id = nbase_fds;
        retain_err(&mut glob.pipes, |sfd| -> Result<bool, String> {
            let mut r = sfd.borrow_mut();
            let rid = r.remote_id;

            let ShadowFdVariant::Pipe(ref mut data) = &mut r.data else {
                panic!("Pipe list contained a non-pipe");
            };
            if data.program_closed {
                return Ok(true);
            }
            let has_pfd = match &data.buf {
                ShadowFdPipeBuffer::ReadFromWayland((buf, len)) => *len < buf.len(),
                ShadowFdPipeBuffer::ReadFromChannel(v) => !v.is_empty(),
            };
            if !has_pfd {
                return Ok(true);
            }

             
            let evts = pfd_returns[base_id];
            base_id += 1;

            let mut keep = true;
            match data.buf {
                ShadowFdPipeBuffer::ReadFromWayland((ref mut buf, ref mut used_len)) => {
                    if evts.contains(PollFlags::POLLIN) {
                         
                        let res = unistd::read(&data.fd, &mut buf[*used_len..]);
                        match res {
                            Ok(len) => {
                                if len == 0 {
                                    debug!("Pipe closed when reading from it");
                                    data.program_closed = true;
                                } else {
                                    debug!("Have read {} bytes from pipe at RID {}", len, rid);
                                    *used_len += len;
                                }
                            }
                            Err(nix::errno::Errno::EINTR) | Err(nix::errno::Errno::EAGAIN) => {
                                 
                            }
                            Err(code) => {
                                return Err(tag!("Failed to read from pipe: {:?}", code));
                            }
                        };
                    } else if evts.contains(PollFlags::POLLHUP) || evts.contains(PollFlags::POLLERR)
                    {
                         
                        debug!(
                            "Pipe at RID={} received{}{}",
                            rid,
                            string_if_bool(evts.contains(PollFlags::POLLHUP), " POLLHUP"),
                            string_if_bool(evts.contains(PollFlags::POLLERR), " POLLERR"),
                        );
                        data.program_closed = true;
                    }
                }
                ShadowFdPipeBuffer::ReadFromChannel(ref mut buf) => {
                    if evts.contains(PollFlags::POLLOUT) {
                        let (slice1, slice2) = buf.as_slices();
                        let io_slices = &[IoSlice::new(slice1), IoSlice::new(slice2)];
                         
                        let res = uio::writev(&data.fd, io_slices);
                        match res {
                            Ok(len) => {
                                buf.drain(0..len).count();
                                if buf.is_empty() && data.channel_closed {
                                     
                                    debug!("Deleting pipe at RID={} after all data written", rid);
                                    keep = false;
                                }
                            }
                            Err(nix::errno::Errno::EPIPE) | Err(nix::errno::Errno::ECONNRESET) => {
                                debug!("Pipe closed when writing to it");
                                data.program_closed = true;
                            }
                            Err(nix::errno::Errno::EINTR) | Err(nix::errno::Errno::EAGAIN) => {
                                 
                            }
                            Err(code) => {
                                return Err(tag!("Failed to write to pipe: {:?}", code));
                            }
                        }
                    } else if evts.contains(PollFlags::POLLHUP) || evts.contains(PollFlags::POLLERR)
                    {
                         
                        debug!(
                            "Pipe at RID={} received{}{}",
                            rid,
                            string_if_bool(evts.contains(PollFlags::POLLHUP), " POLLHUP"),
                            string_if_bool(evts.contains(PollFlags::POLLERR), " POLLERR"),
                        );
                        data.program_closed = true;
                    }
                }
            }
            Ok(keep)
        })?;

         
        let mut base_timeline_id = ntimelinebase_fds;
        for (w, min_pt) in timelines.iter() {
            let evts = pfd_returns[base_timeline_id];
            base_timeline_id += 1;

            let mut b = w.borrow_mut();
            let rid = b.remote_id;
            let ShadowFdVariant::Timeline(ref mut data) = b.data else {
                continue;
            };

            if evts.contains(PollFlags::POLLIN) {
                let mut ret = [0u8; 8];

                let r = nix::unistd::read(data.timeline.get_event_fd(), &mut ret);
                match r {
                    Ok(s) => {
                        assert!(s == 8);
                         

                        let current = data.timeline.get_current_pt()?;
                        if current < *min_pt {
                             
                            continue;
                        }

                        prune_releases(&mut data.releases, current, rid);
                        if let Some(new_min) = data.releases.iter().map(|x| x.0).min() {
                            assert!(new_min > *min_pt);
                        }

                        let pt_bytes = current.to_le_bytes();
                        let msg = cat4x4(
                            build_wmsg_header(WmsgType::SignalTimeline, 16).to_le_bytes(),
                            rid.0.to_le_bytes(),
                            pt_bytes[..4].try_into().unwrap(),
                            pt_bytes[4..].try_into().unwrap(),
                        );
                        from_way.output.other_messages.push(Vec::from(msg));
                    }
                    Err(nix::errno::Errno::EINTR) | Err(nix::errno::Errno::EAGAIN) => {
                         
                    }
                    Err(code) => {
                        return Err(tag!("Failed to read from eventfd: {:?}", code));
                    }
                }
            }
        }

         
        let mut base_dmabuf_id = ndmabufbase_fds;
        for d in dmabufs.iter() {
            let evts = pfd_returns[base_dmabuf_id];
            base_dmabuf_id += 1;

            let rid = d.borrow().remote_id;

            if evts.contains(PollFlags::POLLIN) {
                let mut g = tasksys.tasks.lock().unwrap();
                 
                if let Some(mut tasks) = g.waiting_for_implicit_acquire.remove(&rid) {
                    debug!(
                        "Dmabuf with RID={} may safely be read from, queueing {} tasks",
                        rid,
                        tasks.len()
                    );
                    g.construct.extend(tasks.drain(..));
                }
                drop(g);
                tasksys.task_notify.notify_one();
            }
        }

         
        if timeline_update {
            process_vulkan_updates(glob, tasksys, from_chan)?;
        }

         
        if self_wakeup {
            while let Ok(msg) = from_way.output.recv_msgs.try_recv() {
                match msg {
                    Ok(TaskOutput::Msg(vec)) => {
                        debug!(
                            "Received off thread work result: new output message of length: {}",
                            vec.len()
                        );
                        from_way.output.expected_recvd_msgs =
                            from_way.output.expected_recvd_msgs.checked_sub(1).unwrap();

                        if (from_way.state == DirectionState::On
                            || from_way.state == DirectionState::Drain)
                            && !vec.is_empty()
                        {
                            from_way.output.other_messages.push(vec);
                        }
                    }
                    Ok(TaskOutput::ApplyDone(rid)) => {
                        debug!(
                            "Received off thread work result: completed apply operation for RID={}",
                            rid
                        );
                        let Some(wsfd) = glob.map.get(&rid) else {
                            return Err(tag!("Completed apply for RID that no longer exists"));
                        };
                        let r = wsfd.upgrade().unwrap();
                        let mut sfd = r.borrow_mut();
                        if let ShadowFdVariant::File(ref mut d) = sfd.data {
                            d.pending_apply_tasks = d.pending_apply_tasks.checked_sub(1).unwrap();
                        } else if let ShadowFdVariant::Dmabuf(ref mut d) = sfd.data {
                            d.pending_apply_tasks = d.pending_apply_tasks.checked_sub(1).unwrap();
                        } else {
                            unreachable!();
                        }
                    }
                    Ok(TaskOutput::MirrorApply) | Ok(TaskOutput::DmabufApplyOp(_)) => {
                        unreachable!()
                    }
                    Err(e) => {
                        error!("worker failed: {}", e);
                        return Err(e);
                    }
                }

                if let Some(rid) = from_chan.waiting_for {
                    let sfd = glob.map.get(&rid).unwrap().upgrade().unwrap();
                    let b = sfd.borrow();
                    if let ShadowFdVariant::File(ref data) = &b.data {
                        if data.pending_apply_tasks == 0 {
                            from_chan.waiting_for = None;
                        }
                    } else if let ShadowFdVariant::Dmabuf(ref data) = &b.data {
                        if data.pending_apply_tasks == 0 {
                            from_chan.waiting_for = None;
                        }
                    } else {
                        unreachable!();
                    }
                }
            }
        }

         
        if from_way.state == DirectionState::On || from_way.state == DirectionState::Drain {
            if is_from_way_processable(&from_way.input, glob)
                && !has_from_way_output(&from_way.output)
                && !has_pending_compute_tasks(&from_way.output)
            {
                 
                process_wayland_1(from_way, glob, tasksys)?;
            }

            if !has_pending_compute_tasks(&from_way.output) {
                 
                process_wayland_2(from_way);
            }
        }

        if is_from_chan_processable(from_chan)
            && (from_chan.state == DirectionState::On || from_chan.state == DirectionState::Drain)
            && from_chan.waiting_for.is_none()
            && !has_from_chan_output(&from_chan.output)
        {
            process_channel(from_chan, glob, tasksys)?;
        }

         
        if from_way.state == DirectionState::On
            && from_chan.state == DirectionState::On
            && glob.wire_version <= 16
        {
             
            let val = from_chan.message_counter as u32;
            if val != from_way.output.last_ack {
                let dst = if from_way.output.ack_nwritten > 0 {
                    &mut from_way.output.ack_msg_nxt
                } else {
                    &mut from_way.output.ack_msg_cur
                };
                dst[0..4]
                    .copy_from_slice(&build_wmsg_header(WmsgType::AckNblocks, 8).to_le_bytes());
                dst[4..8].copy_from_slice(&val.to_le_bytes());
                debug!("Queued ack: counter {}", val);
                from_way.output.needs_new_ack = true;
                from_way.output.last_ack = val;
            }
        }

         
        glob.map.retain(|_rid, sfd| sfd.upgrade().is_some());

         
        if from_chan.state == DirectionState::Drain  
            && !is_from_chan_processable(from_chan)
            && !has_from_chan_output(&from_chan.output)
        {
            from_chan.state = DirectionState::Off;
        }
        if from_way.state == DirectionState::Drain
            && !is_from_way_processable(&from_way.input, glob)
            && !has_from_way_output(&from_way.output)
        {
            from_way.state = DirectionState::Off;
        }
    }

    Ok(())
}

 
fn write_bytes(sockfd: &OwnedFd, data: &[u8]) -> Result<(bool, usize), String> {
    let slice = &[IoSlice::new(data)];
    match uio::writev(sockfd, slice) {
        Ok(len) => Ok((false, len)),
        Err(nix::errno::Errno::EPIPE) | Err(nix::errno::Errno::ECONNRESET) => {
            debug!("Channel closed while writing error");
            Ok((true, 0))
        }
        Err(nix::errno::Errno::EINTR) | Err(nix::errno::Errno::EAGAIN) => {
             
            Ok((false, 0))
        }
        Err(x) => Err(tag!("Error writing to socket: {:?}", x)),
    }
}

 
fn loop_end_to_channel(
    error: Option<&str>,
    from_way: &mut FromWayland,
    chanfd: &OwnedFd,
    pollmask: &signal::SigSet,
    sigint_received: &AtomicBool,
    send_close_msg: bool,
) -> Result<(), String> {
    let mut endmsg = Vec::new();
    if let Some(msg) = error {
        let err = format!("waypipe-client internal error: {}", msg);
        let errmsg_len = 4 + length_evt_wl_display_error(err.len());
        endmsg.extend_from_slice(&build_wmsg_header(WmsgType::Protocol, errmsg_len).to_le_bytes());
        assert!(errmsg_len % 4 == 0);
        endmsg.resize(errmsg_len, 0);
        let mut tmp = &mut endmsg[4..];
        write_evt_wl_display_error(&mut tmp, ObjId(1), ObjId(1), 0, err.as_bytes());
    }
    if send_close_msg {
         
        endmsg.extend_from_slice(&build_wmsg_header(WmsgType::Close, 8).to_le_bytes());
        endmsg.extend_from_slice(&0_u32.to_le_bytes());
    }

    let mut nwritten_err = 0;
     
    process_wayland_2(from_way);

    while has_from_way_output(&from_way.output) || nwritten_err < endmsg.len() {
        let mut pfds = [PollFd::new(chanfd.as_fd(), PollFlags::POLLOUT)];

        let res = crate::util::sys_poll(&mut pfds, None, Some(*pollmask));

        if let Err(errno) = res {
            assert!(errno == Errno::EINTR || errno == Errno::EAGAIN);
        }

        if sigint_received.load(Ordering::Acquire) {
            error!("SIGINT");
            break;
        }

        let evts = pfds[0].revents().unwrap();
        if evts.contains(PollFlags::POLLOUT) {
            let eof = if has_from_way_output(&from_way.output) {
                write_to_channel(chanfd, &mut from_way.output)?
            } else {
                let (eof, nwritten) = write_bytes(chanfd, &endmsg[nwritten_err..])?;
                debug!(
                    "Wrote bytes {}..{} of length {} final message; eof: {}",
                    nwritten_err,
                    nwritten_err + nwritten,
                    endmsg.len(),
                    eof
                );
                nwritten_err += nwritten;
                eof
            };
            if eof {
                debug!("Channel closed while writing final message");
                break;
            }
        }
        if evts.contains(PollFlags::POLLHUP) {
             
            debug!("Channel closed");
            break;
        }
    }

    Ok(())
}
 
fn loop_error_to_wayland(
    error: &str,
    from_chan: &mut FromChannel,
    progfd: &OwnedFd,
    pollmask: &signal::SigSet,
    sigint_received: &AtomicBool,
) -> Result<(), String> {
    let err = format!("waypipe-server internal error: {}", error);
    let mut errmsg = Vec::new();
    let errmsg_len = length_evt_wl_display_error(err.len());
    errmsg.resize(errmsg_len, 0);
    let mut tmp = &mut errmsg[..];
    write_evt_wl_display_error(&mut tmp, ObjId(1), ObjId(1), 0, err.as_bytes());

    let mut nwritten_err = 0;

    while has_from_chan_output(&from_chan.output) || nwritten_err < errmsg_len {
        let mut pfds = [PollFd::new(progfd.as_fd(), PollFlags::POLLOUT)];

        let res = crate::util::sys_poll(&mut pfds, None, Some(*pollmask));

        if let Err(errno) = res {
            assert!(errno == Errno::EINTR || errno == Errno::EAGAIN);
        }

        if sigint_received.load(Ordering::Acquire) {
            error!("SIGINT");
            break;
        }

        let evts = pfds[0].revents().unwrap();
        if evts.contains(PollFlags::POLLOUT) {
            let eof = if has_from_chan_output(&from_chan.output) {
                write_to_socket(progfd, &mut from_chan.output)?
            } else {
                let (eof, nwritten) = write_bytes(progfd, &errmsg[nwritten_err..])?;
                debug!(
                    "Wrote bytes {}..{} of length {} error message; eof: {}",
                    nwritten_err,
                    nwritten_err + nwritten,
                    errmsg.len(),
                    eof
                );
                nwritten_err += nwritten;
                eof
            };
            if eof {
                debug!("Wayland connection closed while writing error");
                break;
            }
        }
        if evts.contains(PollFlags::POLLHUP) {
             
            debug!("Wayland connection closed");
            break;
        }
    }

    Ok(())
}

 
struct ThreadShutdown<'a> {
    tasksys: &'a TaskSystem,
}
impl Drop for ThreadShutdown<'_> {
    fn drop(&mut self) {
         
        match self.tasksys.tasks.lock() {
            Ok(mut guard) => {
                guard.stop = true;
            }
            Err(_) => {
                error!("Mutex poisoned, workers expected to shut down");
            }
        }
        self.tasksys.task_notify.notify_all();
    }
}

 
#[derive(Debug, Clone)]
pub struct Options {
     
    pub debug: bool,
     
    pub compression: Compression,
     
    pub video: VideoSetting,
     
    pub threads: u32,
     
    pub title_prefix: String,
     
    pub no_gpu: bool,
     
    pub drm_node: Option<PathBuf>,
     
    pub debug_store_video: Option<PathBuf>,
     
    pub test_skip_vulkan: bool,
     
    pub test_no_timeline_export: bool,
     
    pub test_no_binary_semaphore_import: bool,
}

 
pub fn main_interface_loop(
    chanfd: OwnedFd,
    progfd: OwnedFd,
    opts: &Options,
    init_wire_version: u32,
    on_display_side: bool,
    pollmask: signal::SigSet,
    sigint_received: &AtomicBool,
) -> Result<(), String> {
    debug!("Entered main loop");

    let (sender, receiver): (Sender<TaskResult>, Receiver<TaskResult>) = std::sync::mpsc::channel();

    let mut from_chan = FromChannel {
        state: DirectionState::On,
        input: ReadBuffer::new(),
        next_msg: None,
        rid_queue: VecDeque::new(),
        output: TransferWayland {
            data: &mut [0; 4096],
            start: 0,
            len: 0,
            fds: VecDeque::new(),
        },
        message_counter: 0,
        waiting_for: None,
    };
    let mut from_way = FromWayland {
        state: DirectionState::On,
        input: WaylandInputRing {
            data: &mut [0; 4096],
            len: 0,
            fds: VecDeque::new(),
        },

        output: TransferQueue {
            protocol_data: &mut [0; 4096],
            protocol_len: 0,
            protocol_header_added: false,
            protocol_rids: Vec::new(),

            ack_msg_cur: [0; 8],
            ack_msg_nxt: [0; 8],
            ack_nwritten: 0,
            needs_new_ack: false,
            last_ack: 0,

            other_messages: Vec::new(),
            expected_recvd_msgs: 0,
            recv_msgs: receiver,

            nbytes_written: 0,
        },
    };
    if init_wire_version > MIN_PROTOCOL_VERSION {
        let version_resp = cat2x4(
            build_wmsg_header(WmsgType::Version, 8).to_le_bytes(),
            init_wire_version.to_le_bytes(),
        );
        from_way.output.other_messages.push(Vec::from(version_resp));
    }

    let mut glob = Globals {
        map: BTreeMap::new(),
        fresh: BTreeMap::new(),
        pipes: Vec::new(),
        on_display_side,
        dmabuf_device: if opts.no_gpu {
            DmabufDevice::Unavailable
        } else {
            DmabufDevice::Unknown
        },
        max_local_id: if on_display_side { -1 } else { 1 },
        objects: setup_object_map(),
        max_buffer_uid: 1,  
        presentation_clock: None,
        advertised_modifiers: BTreeMap::new(),
        screencopy_restrictions: BTreeMap::new(),
        opts: (*opts).clone(),  
        wire_version: init_wire_version,
        has_first_message: false,
        has_received_close: false,
    };
    let (wake_r, wake_w) = crate::util::create_pipe()
        .map_err(|x| tag!("Failed to create pipe: {}", x))?;

    let tasksys = TaskSystem {
        task_notify: Condvar::new(),
        tasks: Mutex::new(TaskSet {
            construct: VecDeque::new(),
            waiting_for_implicit_acquire: BTreeMap::new(),
            last_seqno: 0,
            decompress: VecDeque::new(),
            apply: BTreeMap::new(),
            in_progress_decomp: BTreeSet::new(),
            in_progress_apply: BTreeSet::new(),
            apply_operations: Vec::new(),
            dmabuf_fill_tasks: Vec::new(),
            dmabuf_diff_tasks: Vec::new(),
            stop: false,
        }),
        wake_fd: wake_w,
    };

    let nthreads = if opts.threads == 0 {
        std::cmp::max(1, std::thread::available_parallelism().unwrap().get() / 2)
    } else {
        opts.threads as usize
    };

    let tasksys_ref: &TaskSystem = &tasksys;
    let ret = std::thread::scope(|scope| {
        let shutdown = ThreadShutdown { tasksys: &tasksys };

        let mut threads: Vec<std::thread::ScopedJoinHandle<()>> = Vec::new();
        for i in 0..nthreads {
            let senderclone = sender.clone();

            let t = std::thread::Builder::new()
                .name(format!(
                    "{}-worker{}",
                    if on_display_side { "c" } else { "s" },
                    i
                ))
                .spawn_scoped(scope, move || work_thread(tasksys_ref, senderclone))
                .map_err(|x| tag!("Failed to spawn thread: {:?}", x))?;

            threads.push(t);
        }

        let ret = loop_inner(
            scope,
            &mut glob,
            &mut from_chan,
            &mut from_way,
            &tasksys,
            wake_r,
            &chanfd,
            &progfd,
            &pollmask,
            sigint_received,
        );

         
        drop(shutdown);

        let end_res = if let Err(err) = ret {
            error!("Sending error: {}", err);
            if on_display_side {
                loop_end_to_channel(
                    Some(&err),
                    &mut from_way,
                    &chanfd,
                    &pollmask,
                    sigint_received,
                    !glob.has_received_close,
                )
            } else {
                let x = loop_error_to_wayland(
                    &err,
                    &mut from_chan,
                    &progfd,
                    &pollmask,
                    sigint_received,
                );
                let y = loop_end_to_channel(
                    None,
                    &mut from_way,
                    &chanfd,
                    &pollmask,
                    sigint_received,
                    !glob.has_received_close,
                );
                x.and(y)
            }
        } else {
            loop_end_to_channel(
                None,
                &mut from_way,
                &chanfd,
                &pollmask,
                sigint_received,
                !glob.has_received_close,
            )
        };
        if let Err(errerr) = end_res {
            error!("Error while trying to send final messages: {}", errerr);
        }

         
        Ok(())
    });

    debug!("Done.");
    ret
}
