 
 
use crate::tag;
use crate::util::*;
use nix::errno::Errno;
use nix::libc;
use std::collections::BTreeSet;
use std::ops::Range;
use std::sync::Mutex;

enum MirrorBacking {
    Alloc(AlignedArray),
    Mmap(*mut u8),
}

 
unsafe impl Send for MirrorBacking {}

struct MirrorState {
    data: MirrorBacking,
    region_size: usize,
    ranges: BTreeSet<(usize, usize)>,
}

 
pub struct Mirror {
    data: Mutex<MirrorState>,
}

pub struct MirrorRange<'a> {
    mirror: &'a Mirror,
    span: (usize, usize),
    pub data: &'a mut [u8],
}

fn nonempty_range_overlap(a: &(usize, usize), b: &(usize, usize)) -> bool {
    b.0 < a.1 && a.0 < b.1
}

impl Drop for MirrorRange<'_> {
    fn drop(&mut self) {
         
        let mut x = self.mirror.data.lock().unwrap();
        x.ranges.remove(&self.span);
    }
}

impl Drop for MirrorState {
    fn drop(&mut self) {
        if let MirrorBacking::Mmap(v) = self.data {
            if !v.is_null() {
                unsafe {
                     
                    let ret = libc::munmap(v as *mut libc::c_void, self.region_size);
                     
                    assert!(ret == 0);
                }
            }
        }
    }
}

 
unsafe fn do_mmap(size: usize) -> Result<*mut libc::c_void, String> {
     
    let addr: *mut libc::c_void = unsafe {
         
        libc::mmap(
            std::ptr::null_mut(),
            size,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
            -1,
            0,
        )
    };
    if addr == libc::MAP_FAILED {
        let errno = Errno::last_raw();
        return Err(tag!("Failed to mmap size {}: {}", size, errno));
    }
     
    assert!(!addr.is_null());
     
    assert!(
        (addr as usize) % 64 == 0,
        "Insufficient mmap address alignment: {:?}",
        addr
    );
    Ok(addr)
}

 
#[cfg(target_os = "linux")]
unsafe fn do_mremap(
    src: *mut libc::c_void,
    old_size: usize,
    new_size: usize,
) -> Result<*mut libc::c_void, String> {
    let new_addr: *mut libc::c_void = unsafe {
         
        libc::mremap(src, old_size, new_size, libc::MREMAP_MAYMOVE)
    };
    if new_addr == libc::MAP_FAILED {
        let errno = Errno::last_raw();
        return Err(tag!(
            "Failed to remap from size {} to size {}: {}",
            old_size,
            new_size,
            errno,
        ));
    }
    assert!(!new_addr.is_null());
    assert!(
        (new_addr as usize) % 64 == 0,
        "Insufficient mmap address alignment: {:?}",
        new_addr
    );
    Ok(new_addr)
}
#[cfg(not(target_os = "linux"))]
unsafe fn do_mremap(
    src: *mut libc::c_void,
    old_size: usize,
    new_size: usize,
) -> Result<*mut libc::c_void, String> {
    let new_addr: *mut libc::c_void = unsafe {
         
        libc::mmap(
            std::ptr::null_mut(),
            new_size,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
            -1,
            0,
        )
    };
    if new_addr == libc::MAP_FAILED {
        let errno = Errno::last_raw();
        return Err(tag!(
            "Failed to remap from size {} to size {}: {}",
            old_size,
            new_size,
            errno,
        ));
    }
    assert!(!new_addr.is_null());
    assert!(
        (new_addr as usize) % 64 == 0,
        "Insufficient mmap address alignment: {:?}",
        new_addr
    );
    unsafe {
         
        std::ptr::copy_nonoverlapping(src, new_addr, old_size);
         
        let ret = libc::munmap(src, old_size);
         
        assert!(ret == 0);
    }

    Ok(new_addr)
}

impl Mirror {
    pub fn new(size: usize, mmapped: bool) -> Result<Mirror, String> {
        if size > isize::MAX as usize {
            return Err(tag!("Creating mirror too large: {} > {}", size, isize::MAX));
        }
        let s = if mmapped {
             
             
             
             

            let addr: *mut libc::c_void = if size > 0 {
                unsafe {
                     
                    do_mmap(size)?
                }
            } else {
                std::ptr::null_mut()
            };

            MirrorState {
                data: MirrorBacking::Mmap(addr as *mut u8),
                region_size: size,
                ranges: BTreeSet::new(),
            }
        } else {
            MirrorState {
                data: MirrorBacking::Alloc(AlignedArray::new(size)),
                region_size: size,
                ranges: BTreeSet::new(),
            }
        };
        Ok(Mirror {
            data: Mutex::new(s),
        })
    }
     
    pub fn get_mut_range<'a>(&'a self, span: Range<usize>) -> Option<MirrorRange<'a>> {
        if span.end <= span.start {
            return None;
        }
        let x = (span.start, span.end);

        let mut guard = self.data.lock().unwrap();
         
         
        for sp in &guard.ranges {
            if nonempty_range_overlap(sp, &x) {
                 
                return None;
            }
        }
        guard.ranges.insert(x);

        if x.1 > guard.region_size {
            return None;
        }
        let len = x.1 - x.0;
        let start: isize = x.0.try_into().unwrap();

         
        match guard.data {
            MirrorBacking::Mmap(ref mut p) => {
                unsafe {
                     
                    let s: &mut [u8] = std::slice::from_raw_parts_mut(p.offset(start), len);
                    Some(MirrorRange {
                        mirror: self,
                        span: x,
                        data: s,
                    })
                }
            }
            MirrorBacking::Alloc(ref mut v) => {
                unsafe {
                     
                    let (p, size) = v.get_parts();
                    assert!(start >= 0 && (start as usize).saturating_add(len) <= size);
                    let s: &mut [u8] = std::slice::from_raw_parts_mut(p.offset(start), len);
                    Some(MirrorRange {
                        mirror: self,
                        span: x,
                        data: s,
                    })
                }
            }
        }
    }
     
    pub fn extend(&mut self, new_size: usize) -> Result<(), String> {
        if new_size > isize::MAX as usize {
            return Err(tag!(
                "Extending mirror too large: {} >= {}",
                new_size,
                isize::MAX
            ));
        }

        let mut guard = self.data.lock().unwrap();
        let old_size = guard.region_size;
         
        assert!(guard.ranges.is_empty());
        assert!(
            old_size <= new_size,
            "region_size = {} <= new_size = {}",
            old_size,
            new_size
        );
        if new_size == old_size {
            return Ok(());  
        }
        assert!(new_size > old_size);

        match guard.data {
            MirrorBacking::Mmap(ref mut p) => {
                let new_addr = unsafe {
                    if old_size == 0 {
                         
                        do_mmap(new_size)?
                    } else {
                         
                        do_mremap(*p as *mut libc::c_void, old_size, new_size)?
                    }
                };
                *p = new_addr as *mut u8;
            }

            MirrorBacking::Alloc(ref mut v) => {
                let mut new = AlignedArray::new(new_size);
                new.get_mut()[..v.get().len()].copy_from_slice(v.get());
                *v = new;
            }
        }
        guard.region_size = new_size;
        Ok(())
    }
    pub fn len(&self) -> usize {
        self.data.lock().unwrap().region_size
    }
}

#[cfg(test)]
use std::sync::Arc;
#[test]
fn test_mirror_type() {
    for use_mmap in &[false, true] {
        let m: Arc<Mirror> = Arc::new(Mirror::new(1024, *use_mmap).unwrap());
        let m1 = m.clone();
        let m2 = m.clone();
        let j1 = std::thread::spawn(move || {
            let x = m1.get_mut_range(0..20).unwrap();
            x.data[0] = 1;
        });
        let j2 = std::thread::spawn(move || {
            let x = m2.get_mut_range(20..100).unwrap();
            x.data[0] = 1;
        });
        j1.join().unwrap();
        j2.join().unwrap();

        let mut y = Arc::into_inner(m).unwrap();
        y.extend(2048).unwrap();
        let a = y.get_mut_range(0..10).unwrap();
        let b = y.get_mut_range(10..1500).unwrap();
        let c = y.get_mut_range(15..200);
        let d = y.get_mut_range(1600..5000);
        assert!(c.is_none());
        assert!(a.data[0] == 1);
        assert!(b.data[10] == 1);
        assert!(d.is_none());

         
    }
}
