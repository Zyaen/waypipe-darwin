 
 
use crate::tag;
use crate::util::*;
use log::debug;
use nix::errno::Errno;
use nix::libc;
use std::ffi::c_void;
use std::os::fd::AsRawFd;
use std::{collections::VecDeque, os::fd::OwnedFd, sync::Arc};

 
struct ReadBufferChunk {
    data: *mut u8,
    size: usize,
}

 
 
unsafe impl Sync for ReadBufferChunk {}
 
 
unsafe impl Send for ReadBufferChunk {}

 
pub struct ReadBuffer {
     
    old: Option<Arc<ReadBufferChunk>>,
     
    current: Arc<ReadBufferChunk>,
     
     
    last_msg_start: usize,  
    end: usize,
    msg_queue: VecDeque<ReadBufferView>,
}

impl ReadBufferChunk {
    fn new(len: usize) -> ReadBufferChunk {
         
         
        let layout = std::alloc::Layout::from_size_align(len, 4).unwrap();
        assert!(len > 0);
        unsafe {
             
            let data = std::alloc::alloc(layout);
            assert!(!data.is_null());
            ReadBufferChunk { data, size: len }
        }
    }
}

impl Drop for ReadBufferChunk {
    fn drop(&mut self) {
        let layout = std::alloc::Layout::from_size_align(self.size, 4).unwrap();
        unsafe {
             
            std::alloc::dealloc(self.data, layout);
        }
    }
}

 
pub struct ReadBufferView {
     
    _base: Arc<ReadBufferChunk>,  
    data: *mut u8,
    data_len: usize,
}

 
 
unsafe impl Send for ReadBufferView {}
 
unsafe impl Sync for ReadBufferView {}

 
const MAX_NORMAL_MSG_SIZE: usize = 1 << 18;

impl ReadBuffer {
     
    pub fn new() -> Self {
        let chunksize = 4 * MAX_NORMAL_MSG_SIZE;

        ReadBuffer {
            old: None,
            current: Arc::new(ReadBufferChunk::new(chunksize)),
            last_msg_start: 0,
            end: 0,
            msg_queue: VecDeque::new(),
        }
    }

     
    unsafe fn get_message_padded_len(base: *const u8, start: usize) -> Result<usize, String> {
         
        let header_start = base.add(start);
         
         
         
        let header_slice = std::ptr::slice_from_raw_parts(header_start, 4);
        let header = u32::from_le_bytes((&*header_slice).try_into().unwrap());
        let (msg_len, _typ) = parse_wmsg_header(header)
            .ok_or_else(|| tag!("Failed to parse wmsg header: {}", header))?;

        if msg_len < 4 {
            return Err(tag!("Message lengths must be at least 4, not {}", msg_len));
        }
        Ok(align4(msg_len))
    }

     
    unsafe fn read_inner(iovs: &[libc::iovec], src_fd: &OwnedFd) -> Result<(bool, usize), String> {
        assert!(iovs.iter().map(|x| x.iov_len).sum::<usize>() > 0);

         

         
         
        #[cfg(not(miri))]
        let ret = libc::readv(src_fd.as_raw_fd(), iovs.as_ptr(), iovs.len() as _);
        #[cfg(miri)]
        let ret = test_readv(src_fd.as_raw_fd(), iovs.as_ptr(), iovs.len() as _);

        if ret < 0 {
            return match Errno::last() {
                Errno::ECONNRESET => Ok((true, 0)),
                Errno::EINTR | Errno::EAGAIN => Ok((false, 0)),
                x => Err(tag!("Error reading from channel socket: {:?}", x)),
            };
        }
        debug!("Read {} bytes from channel", ret);
        Ok((ret == 0, ret.try_into().unwrap()))
    }

     
    fn move_tail_to_new(&mut self, new_size: usize) {
        let base = self.current.data;
        let nxt = Arc::new(ReadBufferChunk::new(new_size));

         
        assert!(new_size >= self.end - self.last_msg_start);
        unsafe {
            let c_dst = nxt.data;
             
            let c_src = base.add(self.last_msg_start);
             
             
             
             
             
            std::ptr::copy_nonoverlapping(c_src, c_dst, self.end - self.last_msg_start);
        }

        self.current = nxt;
        self.end -= self.last_msg_start;
        self.last_msg_start = 0;
    }

    fn extract_messages(&mut self) -> Result<(), String> {
        let cur_ptr = self.current.data;
        while self.end - self.last_msg_start >= 4 {
            let msg_len = unsafe {
                 
                 
                Self::get_message_padded_len(cur_ptr, self.last_msg_start)?
            };
            if self.end - self.last_msg_start >= msg_len {
                 
                let ptr = unsafe {
                     
                     
                    cur_ptr.add(self.last_msg_start)
                };

                self.msg_queue.push_back(ReadBufferView {
                    _base: self.current.clone(),
                    data: ptr,
                    data_len: msg_len,
                });
                self.last_msg_start += msg_len;
            } else {
                 
                break;
            }
        }
        Ok(())
    }

    fn read_with_old(&mut self, src_fd: &OwnedFd) -> Result<bool, String> {
        let old = self.old.as_ref().unwrap();
        assert!(self.end - self.last_msg_start >= 4);

        let msg_len = unsafe {
             
             
             
            Self::get_message_padded_len(old.data, self.last_msg_start)?
        };
        let msg_end = self.last_msg_start + msg_len;

        let (eof, mut nread) = unsafe {
             
            let iovs = [
                libc::iovec {
                    iov_base: old.data.add(self.end) as *mut c_void,
                    iov_len: msg_end - self.end,
                },
                libc::iovec {
                    iov_base: self.current.data as *mut c_void,
                    iov_len: self.current.size - MAX_NORMAL_MSG_SIZE,
                },
            ];
             
             
             
             
             
             
             
            Self::read_inner(&iovs, src_fd)?
        };

        if nread < msg_end - self.end {
             
            self.end += nread;
            return Ok(eof);
        }
        nread -= msg_end - self.end;

        let ptr = unsafe {
             
            old.data.add(self.last_msg_start)
        };

        let mut tmp = None;
        std::mem::swap(&mut self.old, &mut tmp);
        self.msg_queue.push_back(ReadBufferView {
            _base: tmp.unwrap(),
            data: ptr,
            data_len: msg_len,
        });

        self.last_msg_start = 0;
        self.end = nread;

        self.extract_messages()?;

        Ok(eof)
    }

     
    pub fn read_more(&mut self, src_fd: &OwnedFd) -> Result<bool, String> {
        if self.old.is_some() {
            return self.read_with_old(src_fd);
        }

        if self.end - self.last_msg_start >= 4 {
            let chunk: &ReadBufferChunk = &self.current;
            let cap = chunk.size;

             
            let msg_len = unsafe {
                 
                 
                 
                Self::get_message_padded_len(chunk.data, self.last_msg_start)?
            };
            let msg_end = self.last_msg_start + msg_len;

            if msg_end > cap {
                 
                debug!(
                    "oversized message, length {}, end {} is > capacity {}",
                    msg_len, msg_end, cap
                );
                let new_size = std::cmp::max(2 * msg_len, 4 * MAX_NORMAL_MSG_SIZE);
                assert!(new_size > msg_len + MAX_NORMAL_MSG_SIZE);
                self.move_tail_to_new(new_size);
            } else if cap - msg_end <= MAX_NORMAL_MSG_SIZE {
                 
                let new_size = 4 * MAX_NORMAL_MSG_SIZE;
                let mut nxt = Arc::new(ReadBufferChunk::new(new_size));

                std::mem::swap(&mut self.current, &mut nxt);
                self.old = Some(nxt);
                return self.read_with_old(src_fd);
            }
        } else {
            let chunk: &ReadBufferChunk = &self.current;
            let cap = chunk.size;

             
            let msg_end = self.last_msg_start;
            assert!(msg_end <= cap);
            if cap - msg_end <= MAX_NORMAL_MSG_SIZE {
                 
                 
                debug!(
                    "partial header move, {} {}",
                    msg_end,
                    cap - MAX_NORMAL_MSG_SIZE,
                );
                let new_size = 4 * MAX_NORMAL_MSG_SIZE;
                self.move_tail_to_new(new_size);
            }
        }

         
        let chunk: &ReadBufferChunk = &self.current;
        assert!(chunk.size - self.end >= MAX_NORMAL_MSG_SIZE);

        let (eof, nread) = unsafe {
             
             
            let iovs = [libc::iovec {
                iov_base: chunk.data.add(self.end) as *mut c_void,
                iov_len: chunk.size - MAX_NORMAL_MSG_SIZE - self.end,
            }];
             
             
             
             
             
            Self::read_inner(&iovs, src_fd)?
        };
        self.end += nread;
        self.extract_messages()?;
        Ok(eof)
    }

     
    pub fn pop_next_msg(&mut self) -> Option<ReadBufferView> {
        self.msg_queue.pop_front()
    }
}

impl ReadBufferView {
     
    pub fn get_mut(&mut self) -> &mut [u8] {
        unsafe {
             
             
             
             
             
            let dst = std::ptr::slice_from_raw_parts_mut(self.data, self.data_len);
            &mut *dst
        }
    }

     
    pub fn get(&self) -> &[u8] {
        unsafe {
             
             
             
             
             
            let dst = std::ptr::slice_from_raw_parts_mut(self.data, self.data_len);
            &mut *dst
        }
    }

     
    pub fn advance(&mut self, skip: usize) {
        assert!(skip % 4 == 0);  
        assert!(skip <= self.data_len, "{} <?= {}", skip, self.data_len);

        unsafe {
             
             
             
            self.data = self.data.add(skip);
            self.data_len -= skip;
        }
    }
}

#[cfg(miri)]
use std::ffi::c_int;
 
#[cfg(miri)]
unsafe fn test_readv(fd: c_int, iovs: *const libc::iovec, len: c_int) -> isize {
    let mut nread: isize = 0;
    let mut first = true;
    for i in 0..(len as isize) {
        let iov = *iovs.offset(i);
        if iov.iov_len == 0 {
            continue;
        }
        let r = libc::read(fd, iov.iov_base, iov.iov_len);
        if r == -1 {
            if !first {
                return nread;
            } else {
                return -1;
            }
        }
        nread = nread.checked_add(r).unwrap();
        first = false;
    }
    nread
}

#[test]
fn test_read_buffer() {
    use nix::fcntl;
    use nix::poll;
    use nix::unistd;
    use std::os::fd::AsFd;
    use std::time::Instant;

    let mut rb = ReadBuffer::new();
    let (pipe_r, pipe_w) =
        unistd::pipe2(fcntl::OFlag::O_CLOEXEC | fcntl::OFlag::O_NONBLOCK).unwrap();

    #[cfg(not(miri))]
    fn read_all(rb: &mut ReadBuffer, fd: &OwnedFd) {
        loop {
            let mut p = [poll::PollFd::new(fd.as_fd(), poll::PollFlags::POLLIN)];
            let r = poll::poll(&mut p, poll::PollTimeout::ZERO);
            match r {
                Err(Errno::EINTR) => {
                    continue;
                }
                Err(Errno::EAGAIN) => {
                    break;
                }
                Err(x) => panic!("{:?}", x),
                Ok(_) => {
                    let rev = p[0].revents().unwrap();
                    if rev.contains(poll::PollFlags::POLLIN) {
                        rb.read_more(fd).unwrap();
                    } else if rev.intersects(poll::PollFlags::POLLHUP | poll::PollFlags::POLLERR) {
                        panic!();
                    } else {
                        return;
                    }
                }
            }
        }
    }
    #[cfg(miri)]
    fn read_all(rb: &mut ReadBuffer, fd: &OwnedFd) {
         
         
        for i in 0..20 {
            rb.read_more(fd).unwrap();
        }
    }

    let start = Instant::now();
    println!(
        "Many small messages, immediately dequeued: {}",
        Instant::now().duration_since(start).as_secs_f32()
    );
    {
        for i in 0..(MAX_NORMAL_MSG_SIZE / 2) {
            let mut small_msg = [0u8; 16];
            small_msg[..4]
                .copy_from_slice(&build_wmsg_header(WmsgType::AckNblocks, 16).to_le_bytes());
            small_msg[4..8].copy_from_slice(&(i as u32).to_le_bytes());

            unistd::write(&pipe_w, &small_msg).unwrap();

            read_all(&mut rb, &pipe_r);

            let nxt = rb.pop_next_msg().unwrap();
            assert!(nxt.get() == small_msg);
        }
    }

    println!(
        "Long input with many small chunks: {}",
        Instant::now().duration_since(start).as_secs_f32()
    );
    {
        let mut long_fragmented_input = Vec::<u8>::new();
        for i in 0..(2 * MAX_NORMAL_MSG_SIZE) {
            let mut small_msg = [0u8; 8];
             
            small_msg[..4]
                .copy_from_slice(&build_wmsg_header(WmsgType::AckNblocks, 7).to_le_bytes());
            small_msg[4..].copy_from_slice(&(i as u32).to_le_bytes());
            long_fragmented_input.extend_from_slice(&small_msg);
        }
        let mut x = 0;
        while x < long_fragmented_input.len() {
            let y = unistd::write(&pipe_w, &long_fragmented_input[x..]).unwrap();
            x += y;
            assert!(y > 0);
            rb.read_more(&pipe_r).unwrap();
        }
         
        read_all(&mut rb, &pipe_r);
        for i in 0..(2 * MAX_NORMAL_MSG_SIZE) {
            let nxt = rb.pop_next_msg().unwrap();
            let val = u32::from_le_bytes(nxt.get()[4..].try_into().unwrap());
            assert!(val == i as u32);
        }
    }

    println!(
        "Very long input, needs oversize buffer: {}",
        Instant::now().duration_since(start).as_secs_f32()
    );
    {
        let mut ultra_long_input = Vec::<u8>::new();
        let len = 10 * MAX_NORMAL_MSG_SIZE;
        ultra_long_input.resize(len, 0);
        ultra_long_input[..4]
            .copy_from_slice(&build_wmsg_header(WmsgType::Protocol, len).to_le_bytes());

        let mut x = 0;
        while x < ultra_long_input.len() {
            let y = unistd::write(&pipe_w, &ultra_long_input[x..]).unwrap();
            x += y;
            assert!(y > 0);

            rb.read_more(&pipe_r).unwrap();
        }
        read_all(&mut rb, &pipe_r);
        assert!(rb.pop_next_msg().unwrap().get_mut().len() == len);
    }

     
    println!(
        "Many long chunks: {}",
        Instant::now().duration_since(start).as_secs_f32()
    );
    {
        let mut long_block_input = Vec::<u8>::new();
        let mut long_msg = vec![0; align4(MAX_NORMAL_MSG_SIZE)];
        long_msg[..4].copy_from_slice(
            &build_wmsg_header(WmsgType::AckNblocks, MAX_NORMAL_MSG_SIZE).to_le_bytes(),
        );
        for _ in 0..20 {
            long_block_input.extend_from_slice(&long_msg);
        }
        let mut x = 0;
        while x < long_block_input.len() {
            let y = unistd::write(&pipe_w, &long_block_input[x..]).unwrap();
            x += y;
            assert!(y > 0);

            rb.read_more(&pipe_r).unwrap();
        }
        read_all(&mut rb, &pipe_r);
        let mut concat = Vec::<u8>::new();
        while concat.len() < long_block_input.len() {
            concat.extend_from_slice(rb.pop_next_msg().unwrap().get());
        }
        assert!(concat == long_block_input);
    }

    println!(
        "Mixture of lengths, initially sent byte-by-byte: {}",
        Instant::now().duration_since(start).as_secs_f32()
    );
    {
        let mut long_mixed_input = Vec::<u8>::new();
        let mut i = 0;
        let zero_slice = &[0; 1004];
        while long_mixed_input.len() < 8 * MAX_NORMAL_MSG_SIZE {
            let length = 4 + i % 1000;
            i += 1;

            long_mixed_input
                .extend_from_slice(&build_wmsg_header(WmsgType::AckNblocks, length).to_le_bytes());
            long_mixed_input.extend_from_slice(&zero_slice[..align4(length - 4)]);
        }
        let mut x = 0;
        while x < long_mixed_input.len() {
            let step = if x < 10000 { 1 } else { 100 };
            let y = unistd::write(
                &pipe_w,
                &long_mixed_input[x..std::cmp::min(x + step, long_mixed_input.len())],
            )
            .unwrap();
            x += y;
            assert!(y > 0);

            rb.read_more(&pipe_r).unwrap();
        }
        read_all(&mut rb, &pipe_r);
        let mut concat = Vec::<u8>::new();
        while concat.len() < long_mixed_input.len() {
            let mut nxt = rb.pop_next_msg().unwrap();
            concat.extend_from_slice(&nxt.get()[..4]);
            nxt.advance(4);
            concat.extend_from_slice(nxt.get());
        }
        assert!(concat == long_mixed_input);
    }

    println!(
        "Done: {}",
        Instant::now().duration_since(start).as_secs_f32()
    );
}
