 
 
use crate::tag;
use nix::errno::Errno;
use nix::libc;
use std::os::fd::{AsRawFd, OwnedFd};
use std::sync::atomic::{AtomicU32, AtomicU8, Ordering};

 
pub struct ExternalMapping {
     
    addr: *mut libc::c_void,
    size: usize,
}

 
 
 
 
 
unsafe impl Send for ExternalMapping {}
unsafe impl Sync for ExternalMapping {}

impl Drop for ExternalMapping {
    fn drop(&mut self) {
        if self.size > 0 {
            let ret = unsafe {
                 
                libc::munmap(self.addr, self.size)
            };
            assert!(ret != libc::EINVAL);
        }
    }
}
impl ExternalMapping {
    pub fn new(fd: &OwnedFd, size: usize, readonce: bool) -> Result<ExternalMapping, String> {
        if size == 0 {
            return Ok(ExternalMapping {
                addr: std::ptr::null_mut(),
                size: 0,
            });
        }
        let the_fd: libc::c_int = fd.as_raw_fd();

        if size > isize::MAX as usize {
            return Err(tag!("Failed to mmap {} bytes, region too large", size));
        }

        let (prot_type, map_type) = if readonce {
             
            (libc::PROT_READ, libc::MAP_PRIVATE)
        } else {
            (libc::PROT_READ | libc::PROT_WRITE, libc::MAP_SHARED)
        };
         

         
         

        let addr: *mut libc::c_void = unsafe {
             
            libc::mmap(std::ptr::null_mut(), size, prot_type, map_type, the_fd, 0)
        };
        if addr == libc::MAP_FAILED {
            Err(tag!("Failed to mmap {}", Errno::last_raw()))
        } else {
            assert!(!addr.is_null(), "Weird system allocating null page");
             
            assert!(addr as usize % 64 == 0);
            Ok(ExternalMapping { addr, size })
        }
    }
    pub fn get_u32(&self) -> &[AtomicU32] {
         
        let nblocks = self.size / 4;
        if nblocks == 0 {
            &[]
        } else {
            unsafe {
                 
                let ptr = self.addr as *const AtomicU32;
                std::slice::from_raw_parts(ptr, nblocks)
            }
        }
    }
    pub fn get_u8(&self) -> &[AtomicU8] {
         
        if self.size == 0 {
            &[]
        } else {
            unsafe {
                 
                let ptr = self.addr as *const AtomicU8;
                std::slice::from_raw_parts(ptr, self.size)
            }
        }
    }
}

pub fn construct_diff(
    diff: &mut [u8],
    fd: &ExternalMapping,
    intervals: &[(u32, u32)],
    reference: &mut [u8],  
    reference_base: u32,
) -> usize {
    let target = &fd.get_u32();

    let mut output_len = 0;
    for intv in intervals {
        assert!(intv.0 % 64 == 0 && intv.1 % 64 == 0);
        assert!(reference_base <= intv.0 && intv.0 < intv.1);
        output_len += construct_diff_segment_two_iter::<ShmIterator, ShmWriteback>(
            &mut diff[output_len..],
            ShmIterator::new(
                &target[(intv.0 / 4) as usize..(intv.1 / 4) as usize],
                &mut reference
                    [(intv.0 - reference_base) as usize..(intv.1 - reference_base) as usize],
            ),
            intv.0,
            16,
        ) as usize;
    }

    output_len
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2,lzcnt,bmi1")]
unsafe fn construct_diff_segment_two_avx2(
    mut diff: &mut [u8],
    target: &[u8],
    reference: &mut [u8],
    reference_base: u32,
    skip_gap_len: usize,
) -> u32 {
    use std::arch::x86_64::*;

     
    let mut i = 0;
    let nslabs = target.len() / 64;
    assert!(reference.len() == target.len());
    assert!(target.len() % 64 == 0);
    assert!(target.as_ptr() as usize % 64 == 0);
    assert!(reference.as_ptr() as usize % 64 == 0);
    assert!(diff.as_ptr() as usize % 4 == 0);
    assert!(diff.len() >= target.len() + 8);
    assert!(diff.len() < (u32::MAX as usize));
    let refbase_block: u32 = reference_base / 4;
     
    assert!(skip_gap_len >= 16);
    let skip_slab_len = skip_gap_len / 16;
    assert!(skip_slab_len >= 1);

    let ones = _mm256_set1_epi64x(u64::MAX as i64);
    let mut dc = 0;
    while i < nslabs {
         
        let (ctrl_blocks, x) = diff.split_at_mut(8);
        diff = x;
        dc += 2;  
        let idc = dc;  

        let mut trailing_unchanged = 0;
         
        let mut start = 0;
        while i < nslabs {
             
             
             
            let t1 = _mm256_load_si256(target.as_ptr().add(64 * i) as *const _);
            let t2 = _mm256_load_si256(target.as_ptr().add(64 * i + 32) as *const _);

            let r1 = _mm256_load_si256(reference.as_ptr().add(64 * i) as *const _);
            let r2 = _mm256_load_si256(reference.as_ptr().add(64 * i + 32) as *const _);

            let d1 = _mm256_cmpeq_epi32(t1, r1);
            let d2 = _mm256_cmpeq_epi32(t2, r2);

             
             
             

             

            let merged = _mm256_blend_epi16::<0b01010101>(d1, d2);
            let identical: bool = _mm256_testc_si256(merged, ones) != 0;
            if !identical {
                let merged_mask = _mm256_movemask_epi8(merged) as u32;
                let part1 = 0b00110011001100110011001100110011;
                let part2 = !part1;
                let new_mask =
                    (((merged_mask & part1) as u64) << 32) | (merged_mask & part2) as u64;
                let ncom = (_tzcnt_u64(!new_mask) >> 2) as usize;
                trailing_unchanged = (_lzcnt_u64(!new_mask) >> 2) as usize;
                 
                _mm256_store_si256(reference.as_mut_ptr().add(64 * i) as *mut _, t1);
                _mm256_store_si256(reference.as_mut_ptr().add(64 * i + 32) as *mut _, t2);

                 

                let block_shift = ncom & 7;
                let esmask: u64 = 0xffffffffu64 << (block_shift * 4);

                let halfsize = _mm_set_epi64x(0i64, esmask as i64);
                let estoremask = _mm256_cvtepi8_epi64(halfsize);
                _mm256_maskstore_epi32(
                     
                    diff.as_mut_ptr().sub(block_shift * 4) as *mut _,
                    estoremask,
                    if ncom < 8 { t1 } else { t2 },
                );
                if ncom < 8 {
                    _mm256_storeu_si256(diff.as_mut_ptr().add(4 * (8 - block_shift)) as *mut _, t2);
                }
                dc += 16 - ncom;

                start = 16 * i as u32 + ncom as u32 + refbase_block;

                i += 1;

                break;
            }

            i += 1;
        }

         
         
        while i < nslabs {
            let t1 = _mm256_load_si256(target.as_ptr().add(64 * i) as *const _);
            let t2 = _mm256_load_si256(target.as_ptr().add(64 * i + 32) as *const _);

            let r1 = _mm256_load_si256(reference.as_ptr().add(64 * i) as *const _);
            let r2 = _mm256_load_si256(reference.as_ptr().add(64 * i + 32) as *const _);

            let d1 = _mm256_cmpeq_epi32(t1, r1);
            let d2 = _mm256_cmpeq_epi32(t2, r2);

            let m1 = _mm256_movemask_epi8(d1) as u32;
            let m2 = _mm256_movemask_epi8(d2) as u32;

            let mask = ((m2 as u64) << 32) | (m1 as u64);
            let clear = (!mask == 0) as usize;
            let trail_count = (_lzcnt_u64(!mask) >> 2) as usize;
            trailing_unchanged = trailing_unchanged * clear + trail_count;
             

             

            _mm256_storeu_si256(diff.as_mut_ptr().add(4 * (dc - idc)) as *mut _, t1);
            _mm256_storeu_si256(diff.as_mut_ptr().add(4 * (dc - idc) + 32) as *mut _, t2);
            dc += 16;

             
             
            if trailing_unchanged > skip_gap_len {
                i += 1;
                break;
            }

            _mm256_store_si256(reference.as_ptr().add(64 * i) as *mut _, t1);
            _mm256_store_si256(reference.as_ptr().add(64 * i + 32) as *mut _, t2);

            i += 1;
        }

        if i >= nslabs && dc == idc {
             
            dc -= 2;
            break;
        }

         
         
        dc -= trailing_unchanged;
        let end = 16 * i as u32 - trailing_unchanged as u32 + refbase_block;
        ctrl_blocks[..4].copy_from_slice(&start.to_le_bytes());
        ctrl_blocks[4..].copy_from_slice(&end.to_le_bytes());
        diff = &mut diff[(4 * (end - start)) as usize..];
    }

    (dc * 4) as u32
}

pub trait DiffIterator {
    type Next: DiffWriteback;
    fn next(self) -> Option<(Self::Next, [u8; 64], [u8; 64])>;
}
pub trait DiffWriteback {
    type Next: DiffIterator;
    fn next(self, store: bool) -> Self::Next;
}

struct LocalIterator<'a> {
    target: &'a [u8],
    reference: &'a mut [u8],
    pos: usize,
}
struct LocalWriteback<'a> {
    target: &'a [u8],
    reference: &'a mut [u8],
    pos: usize,
    values: [u8; 64],
}
impl LocalIterator<'_> {
    fn new<'b>(target: &'b [u8], reference: &'b mut [u8]) -> LocalIterator<'b> {
        assert!(target.len() == reference.len());
        assert!(target.len() % 64 == 0);
        assert!(target.as_ptr() as usize % 64 == 0);
        assert!(reference.as_ptr() as usize % 64 == 0);
        LocalIterator {
            target,
            reference,
            pos: 0,
        }
    }
}
impl<'a> DiffIterator for LocalIterator<'a> {
    type Next = LocalWriteback<'a>;

    fn next(self) -> Option<(Self::Next, [u8; 64], [u8; 64])> {
        if self.pos >= self.target.len() {
            return None;
        }
        let values: [u8; 64] = self.target[self.pos..self.pos + 64].try_into().unwrap();
        let refvals: [u8; 64] = self.reference[self.pos..self.pos + 64].try_into().unwrap();

        Some((
            LocalWriteback {
                target: self.target,
                reference: self.reference,
                pos: self.pos,
                values,
            },
            values,
            refvals,
        ))
    }
}
impl<'a> DiffWriteback for LocalWriteback<'a> {
    type Next = LocalIterator<'a>;

    fn next(self, store: bool) -> Self::Next {
        if store {
            self.reference[self.pos..self.pos + 64].copy_from_slice(&self.values);
        }

        LocalIterator {
            target: self.target,
            reference: self.reference,
            pos: self.pos + 64,
        }
    }
}

struct ShmIterator<'a> {
    target: &'a [AtomicU32],
    reference: &'a mut [u8],
    pos: usize,
}
struct ShmWriteback<'a> {
    target: &'a [AtomicU32],
    reference: &'a mut [u8],
    pos: usize,
    values: [u8; 64],
}
impl ShmIterator<'_> {
    fn new<'b>(target: &'b [AtomicU32], reference: &'b mut [u8]) -> ShmIterator<'b> {
        assert!(target.len() * 4 == reference.len());
        assert!(target.len() % 16 == 0);
        assert!(target.as_ptr() as usize % 64 == 0);
        assert!(reference.as_ptr() as usize % 64 == 0);
        ShmIterator {
            target,
            reference,
            pos: 0,
        }
    }
}
impl<'a> DiffIterator for ShmIterator<'a> {
    type Next = ShmWriteback<'a>;

    fn next(self) -> Option<(Self::Next, [u8; 64], [u8; 64])> {
        if self.pos >= self.target.len() {
            return None;
        }
        let mut values = [0_u8; 64];
        for i in 0..16 {
            values[4 * i..4 * (i + 1)].copy_from_slice(
                &self.target[self.pos + i]
                    .load(Ordering::Relaxed)
                    .to_le_bytes(),
            );
        }
        let refvals: [u8; 64] = self.reference[(4 * self.pos)..(4 * self.pos + 64)]
            .try_into()
            .unwrap();

        Some((
            ShmWriteback {
                target: self.target,
                reference: self.reference,
                pos: self.pos,
                values,
            },
            values,
            refvals,
        ))
    }
}
impl<'a> DiffWriteback for ShmWriteback<'a> {
    type Next = ShmIterator<'a>;

    fn next(self, store: bool) -> Self::Next {
        if store {
            self.reference[(4 * self.pos)..(4 * self.pos + 64)].copy_from_slice(&self.values);
        }
        ShmIterator {
            target: self.target,
            reference: self.reference,
            pos: self.pos + 16,
        }
    }
}

 
fn construct_diff_segment_two_iter<A, B>(
    mut diff: &mut [u8],
    mut iter: A,
    reference_base: u32,
    skip_gap_len: usize,
) -> u32
where
    A: DiffIterator<Next = B>,
    B: DiffWriteback<Next = A>,
{
    assert!(reference_base % 4 == 0);
    assert!(diff.as_ptr() as usize % 4 == 0);
    let refbase_block: u32 = reference_base / 4;
    assert!(skip_gap_len >= 16);

    let mut dc: usize = 0;
    let mut line_no = 0;
    'outer: loop {
        let (ctrl_blocks, x) = diff.split_at_mut(8);
        diff = x;
        dc += 2;  
        let idc = dc;  

        let mut trailing_unchanged: usize;
        let start: u32;

         
        loop {
            let Some((res, values, refvals)) = iter.next() else {
                 
                dc -= 2;
                break 'outer;
            };

             
            if values != refvals {
                 
                let leading_unchanged = if values[..32] == refvals[..32] { 8 } else { 0 };
                trailing_unchanged = 0;

                diff[..(4 * (16 - leading_unchanged))]
                    .copy_from_slice(&values[4 * leading_unchanged..]);
                dc += 16 - leading_unchanged;
                start = 16 * line_no as u32 + leading_unchanged as u32 + refbase_block;

                iter = res.next(true);
                line_no += 1;
                break;
            } else {
                iter = res.next(false);
                line_no += 1;
            }
        }

         
        loop {
            let Some((res, values, refvals)) = iter.next() else {
                 
                dc -= trailing_unchanged;
                let end = (16 * line_no - trailing_unchanged) as u32 + refbase_block;
                ctrl_blocks[..4].copy_from_slice(&start.to_le_bytes());
                ctrl_blocks[4..].copy_from_slice(&end.to_le_bytes());
                break 'outer;
            };

             
            iter = res.next(true);

            let clear = unsafe {
                 
                 
                let a: [u64; 8] = std::mem::transmute(values);
                let b: [u64; 8] = std::mem::transmute(refvals);
                (a[0] == b[0])
                    && (a[1] == b[1])
                    && (a[2] == b[2])
                    && (a[3] == b[3])
                    && (a[4] == b[4])
                    && (a[5] == b[5])
                    && (a[6] == b[6])
                    && (a[7] == b[7])
            };
             
            let trail_count = (clear as usize) * 16;
            trailing_unchanged = trailing_unchanged * (clear as usize) + trail_count;

            diff[4 * (dc - idc)..(4 * (dc - idc)) + 64].copy_from_slice(&values);
            dc += 16;

            if trailing_unchanged > skip_gap_len {
                 
                dc -= trailing_unchanged;
                let end = (16 + 16 * line_no - trailing_unchanged) as u32 + refbase_block;
                ctrl_blocks[..4].copy_from_slice(&start.to_le_bytes());
                ctrl_blocks[4..].copy_from_slice(&end.to_le_bytes());
                diff = &mut diff[(4 * (end - start)) as usize..];

                line_no += 1;
                break;
            }
            line_no += 1;
        }
    }

    (dc * 4).try_into().unwrap()
}

pub fn construct_diff_segment_two(
    diff: &mut [u8],
    target: &[u8],
    reference: &mut [u8],
    reference_base: u32,
    skip_gap_len: usize,
) -> u32 {
    #[cfg(target_arch = "x86_64")]
    if is_x86_feature_detected!("avx2")
        && is_x86_feature_detected!("lzcnt")
        && is_x86_feature_detected!("bmi1")
    {
        return unsafe {
             
            construct_diff_segment_two_avx2(diff, target, reference, reference_base, skip_gap_len)
        };
    }

    assert!(diff.len() >= target.len() + 8);
    construct_diff_segment_two_iter::<LocalIterator, LocalWriteback>(
        diff,
        LocalIterator::new(target, reference),
        reference_base,
        skip_gap_len,
    )
}

pub fn copy_tail_if_diff(
    diff_tail: &mut [u8],
    fd: &ExternalMapping,
    tail_len: usize,
    reference: &mut [u8],
) -> bool {
    assert!(reference.len() == tail_len);
    let byte_level = &fd.get_u8();
    let start = byte_level.len() - tail_len;

     
    let mut any_change = false;
    for i in 0..tail_len {
        diff_tail[i] = byte_level[start + i].load(Ordering::Relaxed);
        if diff_tail[i] != reference[i] {
            any_change = true;
        }
        reference[i] = diff_tail[i];
    }
    any_change
}

pub fn copy_from_mapping(dest: &mut [u8], fd: &ExternalMapping, start: usize) {
    let byte_level = &fd.get_u8();
    for i in 0..dest.len() {
        dest[i] = byte_level[i + start].load(Ordering::Relaxed);
    }
}

pub fn copy_onto_mapping(src: &[u8], fd: &ExternalMapping, start: usize) {
    let byte_level = &fd.get_u8();
    for i in 0..src.len() {
        byte_level[i + start].store(src[i], Ordering::Relaxed);
    }
}

pub fn apply_diff_one(
    diff: &[u8],
    ntrailing: usize,
     
    mir_start: usize,
    mirror: &mut [u8],
) -> Result<(), String> {
    assert!((diff.len() - ntrailing) % 4 == 0);
    let mlen = mirror.len();
    let nblocks = (diff.len() - ntrailing) / 4;

     
    let buf_end = mir_start + mlen;
    let mut pos: usize = 0;
    while pos < 4 * nblocks {
         
        let start = u32::from_le_bytes(diff[pos..pos + 4].try_into().unwrap()) as usize;
        let end = u32::from_le_bytes(diff[pos + 4..pos + 8].try_into().unwrap()) as usize;
        if end <= start || end > buf_end || pos + 4 * (end - start) > 4 * nblocks {
            return Err(tag!(
                "copy interval invalid: pos {} segment [{},{}) mirror range [{},{}) remaining {}",
                pos,
                start,
                end,
                mir_start / 4,
                buf_end / 4,
                nblocks - pos / 4
            ));
        }
        pos += 8;
        mirror[(start * 4 - mir_start)..(end * 4 - mir_start)]
            .copy_from_slice(&diff[pos..pos + 4 * (end - start)]);
        pos += (end - start) * 4;
    }

    if ntrailing > 0 {
        let offset = mlen - ntrailing;
        mirror[offset..].copy_from_slice(&diff[nblocks * 4..nblocks * 4 + ntrailing]);
    }

    Ok(())
}

pub fn apply_diff(
    diff: &[u8],
    ntrailing: usize,
    fd: &ExternalMapping,
     
    mir_start: usize,
    mirror: &mut [u8],
) -> Result<(), &'static str> {
    let target = &fd.get_u32();
    assert!((diff.len() - ntrailing) % 4 == 0);
    let nblocks = (diff.len() - ntrailing) / 4;

    let buf_end = target.len();
    let mut pos: usize = 0;
    while pos < 4 * nblocks {
        let start = u32::from_le_bytes(diff[pos..pos + 4].try_into().unwrap()) as usize;
        let end = u32::from_le_bytes(diff[pos + 4..pos + 8].try_into().unwrap()) as usize;
        if end <= start || end > buf_end || pos + 8 + 4 * (end - start) > 4 * nblocks {
            return Err("Copy interval invalid");
        }
        pos += 8;
        mirror[(start * 4 - mir_start)..(end * 4 - mir_start)]
            .copy_from_slice(&diff[pos..pos + 4 * (end - start)]);
        for i in 0..(end - start) {
            target[i + start].store(
                u32::from_le_bytes(diff[(pos + 4 * i)..(pos + 4 * (i + 1))].try_into().unwrap()),
                Ordering::Relaxed,
            );
        }
        pos += (end - start) * 4;
    }

    if ntrailing > 0 {
        let byte_level = &fd.get_u8();
        let offset = byte_level.len() - ntrailing;
        for i in 0..ntrailing {
            byte_level[offset + i].store(diff[nblocks * 4 + i], Ordering::Relaxed);
            mirror[offset + i - mir_start] = diff[nblocks * 4 + i];
        }
    }

    Ok(())
}

 
pub fn compute_diff_span(
    diff: &[u8],
    ntrailing: usize,
    buf_len: usize,
) -> Result<(usize, usize), &'static str> {
    let mut start = buf_len;
    let mut end = 0;

    assert!((diff.len() - ntrailing) % 4 == 0);
    let nblocks = (diff.len() - ntrailing) / 4;
    if nblocks == 0 {
        if ntrailing == 0 {
            return Err("computed diff span on empty diff");
        } else {
            return Ok((buf_len - ntrailing, buf_len));
        }
    }

    let mut pos = 0;
    while pos < 4 * nblocks {
         
        let span_start = u32::from_le_bytes(diff[pos..pos + 4].try_into().unwrap()) as usize;
        let span_end = u32::from_le_bytes(diff[pos + 4..pos + 8].try_into().unwrap()) as usize;
        pos += 8;
        pos += (span_end - span_start) * 4;
        start = std::cmp::min(start, span_start);
        end = std::cmp::max(end, span_end);
    }

    assert!(start < end);

    if ntrailing > 0 {
        Ok((start * 4, buf_len))
    } else {
        Ok((start * 4, end * 4))
    }
}

#[test]
fn test_buffer_replication() {
    use crate::util::AlignedArray;

    let local_fd = crate::util::create_anon_file().unwrap();

    let size = 4096;

    nix::unistd::ftruncate(&local_fd, size as libc::off_t).unwrap();

    let mapping: ExternalMapping = ExternalMapping::new(&local_fd, size, false).unwrap();

    let mut reference_arr = AlignedArray::new(size);
    let reference = reference_arr.get_mut();
     
    reference[123..789].fill(1u8);
    reference[1023..1889].fill(1u8);
    reference[1901..2000].fill(1u8);
    reference[size - 1] = 1;

    let mut diff = vec![0; size + 16];
    let intvs = &[(0, size as u32)];
    let diff_len = construct_diff(&mut diff, &mapping, intvs, &mut reference[..], 0);
    println!("diff len (from fd): {}", diff_len);
    apply_diff(&diff[..diff_len], 0, &mapping, 0, reference).unwrap();

    assert!(reference.iter().all(|x| *x == 0));
}

#[test]
fn test_memory_replication() {
    use crate::util::AlignedArray;

    fn test_pattern(name: &str, diff_start_pos: usize, fill: &dyn Fn(&mut [u8])) {
        let size = 4096;
        let mut mem_arr = AlignedArray::new(size);
        let mut reference_arr = AlignedArray::new(size);
        let mut copy_arr = AlignedArray::new(size);
        let mem = mem_arr.get_mut();
        let reference = reference_arr.get_mut();
        let copy = copy_arr.get_mut();
         
        for (i, x) in mem.chunks_exact_mut(4).enumerate() {
            x.copy_from_slice(&((i + 0xf) as u32).to_le_bytes());
        }
        reference.copy_from_slice(mem);
        copy.copy_from_slice(mem);

        fill(mem);

        let start = std::time::Instant::now();

        let mut diff = vec![0; size + 16];
        let diff_len = construct_diff_segment_two(
            &mut diff,
            &mem[diff_start_pos..],
            &mut reference[diff_start_pos..],
            diff_start_pos as u32,
            16,
        );
        let end = std::time::Instant::now();

        println!(
            "pattern {}, diff len (from memory): {}, elapsed {:.6} msecs",
            name,
            diff_len,
            end.duration_since(start).as_secs_f64() * 1e3
        );

        apply_diff_one(
            &diff[..diff_len as usize],
            0,
            diff_start_pos,
            &mut copy[diff_start_pos..],
        )
        .unwrap();

         
        assert!(copy[diff_start_pos..] == mem[diff_start_pos..]);
         
        assert!(reference[diff_start_pos..] == mem[diff_start_pos..]);
    }

    test_pattern("no change", 1024, &|_| ());
    test_pattern("all change", 64, &|x| x.fill(1));
    test_pattern("irregular", 64, &|x| {
        x[123..789].fill(1);
        x[1023..1889].fill(1);
        x[1901..2000].fill(1);
        x[x.len() - 1] = 1;
    });
    test_pattern("mark some even lines", 64, &|x| {
        for i in 5..23 {
            x[128 * i] = 1u8;
        }
    });
    test_pattern("mark every fourth line", 64, &|x| {
        for i in 0..x.len() / 256 {
            x[256 * i] = 1u8;
        }
    });
    test_pattern("small gaps", 64, &|x| {
        x[123..246].fill(1);
        x[369..1200].fill(1);
        x[1421..2000].fill(1);
    });

    for osc in [1, 7, 63, 64, 65, 127, 128, 129, 255, 256, 257] {
        let s = format!("alternating {}, with gap", osc);
        test_pattern(&s, 64, &|x| {
            for i in 0..x.len() / (2 * osc) {
                let end = std::cmp::min(x.len(), (2 * i + 1) * osc);
                x[(2 * i * osc)..end].fill(1);
            }
            x[1500..2500].fill(1);
        });
    }
    test_pattern("short start", 0, &|x| {
        x[0..4].fill(1);
    });
}
