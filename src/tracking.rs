 
 
use crate::damage::*;
#[cfg(feature = "dmabuf")]
use crate::dmabuf::*;
#[cfg(feature = "gbmfallback")]
use crate::gbm::*;
use crate::kernel::*;
use crate::mainloop::*;
use crate::platform::*;
#[cfg(any(not(feature = "video"), not(feature = "gbmfallback")))]
use crate::stub::*;
use crate::tag;
use crate::util::*;
use crate::wayland::*;
use crate::wayland_gen::*;

use core::str;
use log::{debug, error};
use nix::libc;
use nix::sys::time;
use nix::unistd;
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt::{Display, Formatter};
use std::os::fd::OwnedFd;
use std::rc::Rc;

 
pub struct WpObject {
     
    obj_type: WaylandInterface,
     
    extra: WpExtra,
}

 
#[derive(Clone, Copy, Debug)]
struct WlRect {
    x: i32,
    y: i32,
    width: i32,
    height: i32,
}

 
#[derive(Eq, PartialEq, Clone)]
struct BufferAttachment {
     
    scale: u32,
     
    transform: WlOutputTransform,
     
    viewport_src: Option<(i32, i32, i32, i32)>,
     
    viewport_dst: Option<(i32, i32)>,
     
    buffer_uid: u64,
     
    buffer_size: (i32, i32),
}

 
#[derive(Clone)]
struct DamageBatch {
     
    attachment: BufferAttachment,
     
    damage: Vec<WlRect>,
     
    damage_buffer: Vec<WlRect>,
}

 
struct ObjWlSurface {
    attached_buffer_id: Option<ObjId>,
     
    damage_history: [DamageBatch; 7],
     
    acquire_pt: Option<(u64, Rc<RefCell<ShadowFd>>)>,
    release_pt: Option<(u64, Rc<RefCell<ShadowFd>>)>,

     
    viewport_id: Option<ObjId>,
}

 
struct ObjWpViewport {
     
    wl_surface: Option<ObjId>,
}

 
struct ObjWlShmPool {
    buffer: Rc<RefCell<ShadowFd>>,
}

 
#[derive(Clone, Copy)]
struct ObjWlBufferShm {
    width: i32,
    height: i32,
    format: u32,
    offset: i32,
    stride: i32,
}

 
struct ObjWlBuffer {
    sfd: Rc<RefCell<ShadowFd>>,
     
    shm_info: Option<ObjWlBufferShm>,

    unique_id: u64,
}

 
struct DmabufTranche {
    flags: u32,
     
    values: Vec<(u32, u64)>,
     
    indices: Vec<u8>,
    device: u64,
}

 
struct ObjZwpLinuxDmabuf {
     
    formats_seen: BTreeSet<u32>,
}

 
struct ObjZwpLinuxDmabufFeedback {
     
    input_format_table: Option<Vec<(u32, u64)>>,
     
    output_format_table: Option<Vec<u8>>,

    main_device: Option<u64>,
    tranches: Vec<DmabufTranche>,
    current: DmabufTranche,
     
    processed: bool,
     
    queued_format_table: Option<(Rc<RefCell<ShadowFd>>, u32)>,
}

 
struct ObjZwpLinuxDmabufParams {
     
    dmabuf: Option<Rc<RefCell<ShadowFd>>>,
     
     
    planes: Vec<AddDmabufPlane>,
     
     
     
}

 
struct ObjWpDrmSyncobjSurface {
     
    surface: ObjId,
}

 
struct ObjWpDrmSyncobjTimeline {
    timeline: Rc<RefCell<ShadowFd>>,
}

 
struct ObjZwlrScreencopyFrame {
     
    buffer: Option<(Rc<RefCell<ShadowFd>>, Option<ObjWlBufferShm>)>,
}

 
struct ObjExtImageCopyCaptureSession {
    dmabuf_device: Option<u64>,
    dmabuf_formats: Vec<(u32, Vec<u64>)>,
     
    last_format_mod_list: Vec<(u32, u64)>,
    frame_list: Vec<ObjId>,  
}

 
struct ObjExtImageCopyCaptureFrame {
     
    buffer: Option<(Rc<RefCell<ShadowFd>>, Option<ObjWlBufferShm>)>,

     
    capture_session: Option<ObjId>,

     
    supported_modifiers: Vec<(u32, u64)>,
}

 
struct ObjZwlrGammaControl {
    gamma_size: Option<u32>,
}

 
struct ObjWlRegistry {
     
    syncobj_manager_replay: Vec<(u32, u32)>,
}

 
enum WpExtra {
    WlSurface(Box<ObjWlSurface>),
    WlBuffer(Box<ObjWlBuffer>),
    WlRegistry(Box<ObjWlRegistry>),
    WlShmPool(Box<ObjWlShmPool>),
    WpViewport(Box<ObjWpViewport>),
    ZwpDmabuf(Box<ObjZwpLinuxDmabuf>),
    ZwpDmabufFeedback(Box<ObjZwpLinuxDmabufFeedback>),
    ZwpDmabufParams(Box<ObjZwpLinuxDmabufParams>),
    ZwlrScreencopyFrame(Box<ObjZwlrScreencopyFrame>),
    ExtImageCopyCaptureSession(Box<ObjExtImageCopyCaptureSession>),
    ExtImageCopyCaptureFrame(Box<ObjExtImageCopyCaptureFrame>),
    ZwlrGammaControl(Box<ObjZwlrGammaControl>),
    WpDrmSyncobjSurface(Box<ObjWpDrmSyncobjSurface>),
    WpDrmSyncobjTimeline(Box<ObjWpDrmSyncobjTimeline>),
    None,
}

 
pub enum TranslationInfo<'a> {
     
    FromChannel(
        (
            &'a mut VecDeque<Rc<RefCell<ShadowFd>>>,
            &'a mut VecDeque<Rc<RefCell<ShadowFd>>>,
        ),
    ),
     
    FromWayland(
        (
            &'a mut VecDeque<OwnedFd>,
            &'a mut Vec<Rc<RefCell<ShadowFd>>>,
        ),
    ),
}

 
fn clip_wlrect_to_buffer(a: &WlRect, w: i32, h: i32) -> Option<Rect> {
    let x1 = a.x;
    let x2 = a.x.saturating_add(a.width);
    let y1 = a.y;
    let y2 = a.y.saturating_add(a.height);
    let nx1 = x1.clamp(0, w);
    let nx2 = x2.clamp(0, w);
    let ny1 = y1.clamp(0, h);
    let ny2 = y2.clamp(0, h);
    if nx2 > nx1 && ny2 > ny1 {
        Some(Rect {
            x1: nx1 as u32,
            x2: nx2 as u32,
            y1: ny1 as u32,
            y2: ny2 as u32,
        })
    } else {
        None
    }
}

 
fn apply_viewport_transform(
    a: &WlRect,
    buffer_size: (i32, i32),
    view_src: Option<(i32, i32, i32, i32)>,
    view_dst: Option<(i32, i32)>,
) -> Option<Rect> {
    assert!(a.width >= 0 && a.height >= 0);

    let dst: (i32, i32) = if let Some(x) = view_dst {
        (x.0, x.1)
    } else if let Some(x) = view_src {
         
        (x.2 / 256, x.3 / 256)
    } else {
        (buffer_size.0, buffer_size.1)
    };

     
    let x1 = a.x.clamp(0, dst.0);
    let x2 = a.x.saturating_add(a.width).clamp(0, dst.0);
    let y1 = a.y.clamp(0, dst.1);
    let y2 = a.y.saturating_add(a.height).clamp(0, dst.1);
    if x2 <= x1 || y2 <= y1 {
         
        return None;
    }

     
    if let Some(v) = view_src {
        assert!(v.0 >= 0 && v.1 >= 0 && v.2 > 0 && v.3 > 0);

        fn source_floor(v: i32, dst: i32, src_sz_fixed: i32, src_offset_fixed: i32) -> u32 {
             
            (((v as u64) * (src_sz_fixed as u64) / (dst as u64) + (src_offset_fixed as u64)) / 256)
                as u32
        }
        fn source_ceil(v: i32, dst: i32, src_sz_fixed: i32, src_offset_fixed: i32) -> u32 {
            ((((v as u64) * (src_sz_fixed as u64)).div_ceil(dst as u64)
                + (src_offset_fixed as u64))
                .div_ceil(256)) as u32
        }

         
        let sx1 = source_floor(x1, dst.0, v.2, v.0).min(buffer_size.0 as u32);
        let sx2 = source_ceil(x2, dst.0, v.2, v.0).min(buffer_size.0 as u32);
        let sy1 = source_floor(y1, dst.1, v.3, v.1).min(buffer_size.1 as u32);
        let sy2 = source_ceil(y2, dst.1, v.3, v.1).min(buffer_size.1 as u32);
        if sx1 >= sx2 || sy1 >= sy2 {
            return None;
        }
        Some(Rect {
            x1: sx1,
            x2: sx2,
            y1: sy1,
            y2: sy2,
        })
    } else {
         
        let sx1 = (((x1 as u64) * (buffer_size.0 as u64)) / (dst.0 as u64)) as u32;
        let sx2 = (((x2 as u64) * (buffer_size.0 as u64)).div_ceil(dst.0 as u64)) as u32;
        let sy1 = (((y1 as u64) * (buffer_size.1 as u64)) / (dst.1 as u64)) as u32;
        let sy2 = (((y2 as u64) * (buffer_size.1 as u64)).div_ceil(dst.1 as u64)) as u32;
         
        Some(Rect {
            x1: sx1,
            x2: sx2,
            y1: sy1,
            y2: sy2,
        })
    }
}

 
fn apply_damage_rect_transform(
    a: &WlRect,
    scale: u32,
    transform: WlOutputTransform,
    view_src: Option<(i32, i32, i32, i32)>,
    view_dst: Option<(i32, i32)>,
    width: i32,
    height: i32,
) -> Option<Rect> {
     
    let seq = [0b000, 0b011, 0b110, 0b101, 0b010, 0b001, 0b100, 0b111];
    let code = seq[transform as u32 as usize];
    let swap_xy = code & 0x1 != 0;
    let flip_x = code & 0x2 != 0;
    let flip_y = code & 0x4 != 0;

    let pre_vp_size = if swap_xy {
        (height / scale as i32, width / scale as i32)
    } else {
        (width / scale as i32, height / scale as i32)
    };

    let b = apply_viewport_transform(a, pre_vp_size, view_src, view_dst)?;

     
    let mut xl = b.x1.checked_mul(scale).unwrap();
    let mut yl = b.y1.checked_mul(scale).unwrap();
    let mut xh = b.x2.checked_mul(scale).unwrap();
    let mut yh = b.y2.checked_mul(scale).unwrap();

    let end_w = if swap_xy { height } else { width } as u32;
    let end_h = if swap_xy { width } else { height } as u32;

    if flip_x {
        (xh, xl) = (end_w - xl, end_w - xh);
    }
    if flip_y {
        (yh, yl) = (end_h - yl, end_h - yh);
    }
    if swap_xy {
        (xl, xh, yl, yh) = (yl, yh, xl, xh);
    }
    Some(Rect {
        x1: xl,
        x2: xh,
        y1: yl,
        y2: yh,
    })
}

 
fn inverse_viewport_transform(
    a: &Rect,
    buffer_size: (i32, i32),
    view_src: Option<(i32, i32, i32, i32)>,
    view_dst: Option<(i32, i32)>,
) -> Option<WlRect> {
    assert!(buffer_size.0 > 0 && buffer_size.1 > 0);
    assert!(
        a.x1 < a.x2 && a.y1 < a.y2 && a.x2 <= buffer_size.0 as u32 && a.y2 <= buffer_size.1 as u32
    );

     
    let (mut x1, mut x2, mut y1, mut y2) = (
        (a.x1 as u64) * 256,
        (a.x2 as u64) * 256,
        (a.y1 as u64) * 256,
        (a.y2 as u64) * 256,
    );
    if let Some((sx, sy, sw, sh)) = view_src {
        assert!(sx >= 0 && sy >= 0 && sw > 0 && sh > 0);
        let e = (sx as u64 + sw as u64, sy as u64 + sh as u64);
        x1 = x1.clamp(sx as u64, e.0) - (sx as u64);
        x2 = x2.clamp(sx as u64, e.0) - (sx as u64);
        y1 = y1.clamp(sy as u64, e.1) - (sy as u64);
        y2 = y2.clamp(sy as u64, e.1) - (sy as u64);

         
        if x2 <= x1 || y2 <= y1 {
            return None;
        }
    };
     
    let src: (u64, u64) = if let Some(x) = view_src {
        (x.2 as u64, x.3 as u64)
    } else {
        (buffer_size.0 as u64 * 256, buffer_size.1 as u64 * 256)
    };

     
    let dst: (u32, u32) = if let Some(x) = view_dst {
        (x.0 as u32, x.1 as u32)
    } else if let Some(x) = view_src {
         
        (x.2 as u32 / 256, x.3 as u32 / 256)
    } else {
        (buffer_size.0 as u32, buffer_size.1 as u32)
    };

     
    let xl = ((x1 as u128) * (dst.0 as u128)) / (src.0 as u128);
    let xh = ((x2 as u128) * (dst.0 as u128)).div_ceil(src.0 as u128);
    let yl = ((y1 as u128) * (dst.1 as u128)) / (src.1 as u128);
    let yh = ((y2 as u128) * (dst.1 as u128)).div_ceil(src.1 as u128);
    assert!(xh > xl && yh > yl);
     
    assert!(xh <= i32::MAX as _ && yh <= i32::MAX as _);
    Some(WlRect {
        x: xl as i32,
        y: yl as i32,
        width: (xh - xl) as i32,
        height: (yh - yl) as i32,
    })
}

 
fn inverse_damage_rect_transform(
    a: &WlRect,
    scale: u32,
    transform: WlOutputTransform,
    view_src: Option<(i32, i32, i32, i32)>,
    view_dst: Option<(i32, i32)>,
    width: i32,
    height: i32,
) -> Option<WlRect> {
    assert!(width > 0 && height > 0 && scale > 0);

     
    let seq = [0b000, 0b011, 0b110, 0b101, 0b010, 0b001, 0b100, 0b111];
    let code = seq[transform as u32 as usize];
    let swap_xy = code & 0x1 != 0;
    let flip_x = code & 0x2 != 0;
    let flip_y = code & 0x4 != 0;

    let mut xl = a.x.clamp(0, width) as u32;
    let mut xh = a.x.saturating_add(a.width).clamp(0, width) as u32;
    let mut yl = a.y.clamp(0, height) as u32;
    let mut yh = a.y.saturating_add(a.height).clamp(0, height) as u32;
    if xh <= xl || yh <= yl {
         
        return None;
    }
    let (end_w, end_h) = if swap_xy {
        (height as u32, width as u32)
    } else {
        (width as u32, height as u32)
    };

    if swap_xy {
        (xl, xh, yl, yh) = (yl, yh, xl, xh);
    }
    if flip_y {
        (yh, yl) = (end_h - yl, end_h - yh);
    }
    if flip_x {
        (xh, xl) = (end_w - xl, end_w - xh);
    }
    (xl, xh) = (xl / scale, xh.div_ceil(scale));
    (yl, yh) = (yl / scale, yh.div_ceil(scale));

    let post_vp_size = if swap_xy {
        (height / scale as i32, width / scale as i32)
    } else {
        (width / scale as i32, height / scale as i32)
    };

    let b = Rect {
        x1: xl,
        x2: xh,
        y1: yl,
        y2: yh,
    };
    inverse_viewport_transform(&b, post_vp_size, view_src, view_dst)
}

 
fn damage_for_entire_buffer(buffer: &ObjWlBufferShm) -> (usize, usize) {
    let start = (buffer.offset) as usize;
    let end = if let Some(layout) = get_shm_format_layout(buffer.format) {
        let mut end = start;
        assert!(buffer.stride >= 0 && buffer.width >= 0 && buffer.height >= 0);
         
        let ext_stride = (buffer.stride as u32) * layout.planes[0].hsub.get();

        for plane in layout.planes {
             
            let plane_stride = (ext_stride * plane.bpt.get())
                .div_ceil(layout.planes[0].bpt.get() * plane.hsub.get());
            let plane_height = (buffer.height as u32).div_ceil(plane.vsub.get());

            end = end.saturating_add(plane_height.saturating_mul(plane_stride) as usize);
        }
        end
    } else {
        start.saturating_add(buffer.stride.saturating_mul(buffer.height) as usize)
    };
    (64 * (start / 64), align(end, 64))
}

 
fn get_damage_rects(surface: &ObjWlSurface, attachment: &BufferAttachment) -> Vec<Rect> {
    let (width, height) = attachment.buffer_size;
    let mut rects = Vec::<Rect>::new();
    let full_damage = Rect {
        x1: 0,
        x2: width.try_into().unwrap(),
        y1: 0,
        y2: height.try_into().unwrap(),
    };

     
    let Some(first_idx_offset) = surface
        .damage_history
        .iter()
        .skip(1)
        .position(|x| x.attachment.buffer_uid == attachment.buffer_uid)
    else {
         
        rects.push(full_damage);
        return rects;
    };
    let first_idx = first_idx_offset + 1;
    if surface.damage_history[first_idx].attachment != *attachment {
         
        rects.push(full_damage);
        return rects;
    }

     
    for (i, batch) in surface.damage_history[..first_idx].iter().enumerate() {
        if i == 0 {
             
            for w in &batch.damage_buffer {
                if let Some(r) = clip_wlrect_to_buffer(w, width, height) {
                    rects.push(r);
                }
            }
        } else {
            for w in &batch.damage_buffer {
                 
                let Some(s) = inverse_damage_rect_transform(
                    w,
                    batch.attachment.scale,
                    batch.attachment.transform,
                    batch.attachment.viewport_src,
                    batch.attachment.viewport_dst,
                    batch.attachment.buffer_size.0,
                    batch.attachment.buffer_size.1,
                ) else {
                    continue;
                };
                if let Some(r) = apply_damage_rect_transform(
                    &s,
                    attachment.scale,
                    attachment.transform,
                    attachment.viewport_src,
                    attachment.viewport_dst,
                    width,
                    height,
                ) {
                    rects.push(r);
                }
            }
        }

        for w in &batch.damage {
             
            if let Some(r) = apply_damage_rect_transform(
                w,
                attachment.scale,
                attachment.transform,
                attachment.viewport_src,
                attachment.viewport_dst,
                width,
                height,
            ) {
                rects.push(r);
            }
        }
    }
    rects
}

 
fn get_damage_for_shm(
    buffer: &ObjWlBuffer,
    surface: &ObjWlSurface,
    attachment: &BufferAttachment,
) -> Vec<(usize, usize)> {
    let Some(shm_info) = &buffer.shm_info else {
        panic!();
    };

    let Some(layout) = get_shm_format_layout(shm_info.format) else {
        debug!("Format without known linear layout {}", shm_info.format);
        return vec![damage_for_entire_buffer(shm_info)];
    };
    let [p0] = layout.planes else {
        debug!(
            "Format {} has {} planes",
            shm_info.format,
            layout.planes.len()
        );
        return vec![damage_for_entire_buffer(shm_info)];
    };
    if p0.hsub.get() != 1 || p0.vsub.get() != 1 || p0.htex.get() != 1 || p0.vtex.get() != 1 {
        debug!(
            "Format {} has nontrivial texels or subsampling",
            shm_info.format
        );
        return vec![damage_for_entire_buffer(shm_info)];
    }
    let bpp = p0.bpt.get();

    let mut rects = get_damage_rects(surface, attachment);
    compute_damaged_segments(
        &mut rects[..],
        6,
        128,
        shm_info.offset.try_into().unwrap(),
        shm_info.stride.try_into().unwrap(),
        bpp as usize,
    )
}

 
fn get_damage_for_dmabuf(
    sfdd: &ShadowFdDmabuf,
    surface: &ObjWlSurface,
    attachment: &BufferAttachment,
) -> Vec<(usize, usize)> {
     
    let (nom_len, width) = match sfdd.buf {
        DmabufImpl::Vulkan(ref buf) => (buf.nominal_size(sfdd.view_row_stride), buf.width),
        DmabufImpl::Gbm(ref buf) => (buf.nominal_size(sfdd.view_row_stride), buf.width),
    };

    assert!(
        sfdd.drm_format != WlShmFormat::Xrgb8888 as u32
            && sfdd.drm_format != WlShmFormat::Argb8888 as u32
    );
    let wayl_format = drm_to_wayland(sfdd.drm_format);
    let Some(layout) = get_shm_format_layout(wayl_format) else {
        debug!("Format without known bpp {}", sfdd.drm_format);
        return vec![(0, align(nom_len, 64))];
    };
    let [p0] = layout.planes else {
        debug!(
            "Format {} has {} planes",
            sfdd.drm_format,
            layout.planes.len()
        );
        return vec![(0, align(nom_len, 64))];
    };
    if p0.hsub.get() != 1 || p0.vsub.get() != 1 || p0.htex.get() != 1 || p0.vtex.get() != 1 {
        debug!(
            "Format {} has nontrivial texels or subsampling",
            sfdd.drm_format
        );
        return vec![(0, align(nom_len, 64))];
    }
    let bpp = p0.bpt.get();

    let mut rects = get_damage_rects(surface, attachment);
     
     
    let stride = sfdd.view_row_stride.unwrap_or(width * bpp);
    compute_damaged_segments(&mut rects[..], 6, 128, 0, stride as usize, bpp as usize)
}

 
fn process_dmabuf_feedback(feedback: &mut ObjZwpLinuxDmabufFeedback) -> Result<Vec<u8>, String> {
    let mut index: BTreeMap<(u32, u64), u16> = BTreeMap::new();
    for t in feedback.tranches.iter() {
        for f in t.values.iter() {
            index.insert(*f, u16::MAX);
        }
    }

    if index.len() > u16::MAX as usize {
        return Err(tag!(
            "Format table is too large ({} > {})",
            index.len(),
            u16::MAX
        ));
    }

    let mut table = Vec::new();
    for (i, (f, v)) in index.iter_mut().enumerate() {
        table.extend_from_slice(&f.0.to_le_bytes());
        table.extend_from_slice(&0u32.to_le_bytes());
        table.extend_from_slice(&f.1.to_le_bytes());
        *v = i as u16;
    }

    for t in feedback.tranches.iter_mut() {
        let mut indices = Vec::new();
        for f in t.values.iter() {
            let idx = index.get(f).expect("Inserted key should still be present");
            indices.extend_from_slice(&idx.to_le_bytes());
        }
        t.indices = indices;
    }

    Ok(table)
}

 
fn rebuild_format_table(
    dmabuf_dev: &DmabufDevice,
    feedback: &mut ObjZwpLinuxDmabufFeedback,
) -> Result<(), String> {
     
    let mut remote_formats = BTreeSet::<u32>::new();
    for t in feedback.tranches.iter() {
        if t.device != feedback.main_device.unwrap() {
             
            continue;
        }
        for (fmt, _modifier) in t.values.iter() {
            remote_formats.insert(*fmt);
        }
    }

     
    let mut new_tranches = Vec::<DmabufTranche>::new();
    for t in feedback.tranches.iter() {
        if t.device != feedback.main_device.unwrap() {
            continue;
        }

        let mut n = DmabufTranche {
            device: feedback.main_device.unwrap(),
            flags: 0,
            values: Vec::new(),
            indices: Vec::new(),
        };

        for (fmt, _modifier) in t.values.iter() {
             
            if remote_formats.remove(fmt) {
                let mods = dmabuf_dev_modifier_list(dmabuf_dev, *fmt);
                for m in mods {
                    n.values.push((*fmt, *m));
                }
            }
        }
        if !n.values.is_empty() {
            new_tranches.push(n);
        }
    }
    if new_tranches.is_empty() {
        return Err(tag!(
            "Failed to build new format tranches: no formats with common support"
        ));
    }
    feedback.tranches = new_tranches;

    Ok(())
}

 
fn add_advertised_modifiers(map: &mut BTreeMap<u32, Vec<u64>>, format: u32, modifiers: &[u64]) {
    if modifiers.is_empty() {
        return;
    }
    let entries: &mut Vec<u64> = map.entry(format).or_default();
    for m in modifiers {
        if !entries.contains(m) {
            entries.push(*m);
        }
    }
}

 
fn insert_new_object(
    objects: &mut BTreeMap<ObjId, WpObject>,
    id: ObjId,
    obj: WpObject,
) -> Result<(), String> {
    let t = obj.obj_type;
    if let Some(old) = objects.insert(id, obj) {
        return Err(tag!(
            "Creating object of type {:?} with id {}, but object of type {:?} with same id already exists",
            t, id, old.obj_type,
        ));
    }
    Ok(())
}

 
fn register_generic_new_ids(
    msg: &[u8],
    meth: &WaylandMethod,
    glob: &mut Globals,
) -> Result<(), String> {
    let mut tail = &msg[8..];

    for op in meth.sig {
        match op {
            WaylandArgument::Uint | WaylandArgument::Int | WaylandArgument::Fixed => {
                let _ = parse_u32(&mut tail);
            }
            WaylandArgument::Fd => {
                 
            }
            WaylandArgument::Object(_) | WaylandArgument::GenericObject => {
                let _ = parse_obj(&mut tail)?;
            }
            WaylandArgument::NewId(new_intf) => {
                let id = parse_obj(&mut tail)?;

                insert_new_object(
                    &mut glob.objects,
                    id,
                    WpObject {
                        obj_type: *new_intf,
                        extra: WpExtra::None,
                    },
                )?;
            }
            WaylandArgument::GenericNewId => {
                 
                let string = parse_string(&mut tail)?
                    .ok_or_else(|| tag!("New id string should not be null"))?;
                let _version = parse_u32(&mut tail)?;
                let id = parse_obj(&mut tail)?;

                if glob.objects.contains_key(&id) {
                    return Err(tag!("Creating object with id not detected as deleted"));
                }

                if let Some(new_intf) = lookup_intf_by_name(string) {
                     
                    glob.objects.insert(
                        id,
                        WpObject {
                            obj_type: new_intf,
                            extra: WpExtra::None,
                        },
                    );
                }
            }
            WaylandArgument::String => {
                let x = parse_string(&mut tail)?;
                if x.is_none() {
                    return Err(tag!("Received null string where none allowed"));
                }
            }
            WaylandArgument::OptionalString => {
                let _ = parse_string(&mut tail)?;
            }
            WaylandArgument::Array => {
                let _ = parse_array(&mut tail)?;
            }
        }
    }
    Ok(())
}

 
fn copy_msg(msg: &[u8], dst: &mut &mut [u8]) {
    dst[..msg.len()].copy_from_slice(msg);
    *dst = &mut std::mem::take(dst)[msg.len()..];
}
 
fn copy_msg_tag_fd(msg: &[u8], dst: &mut &mut [u8], from_channel: bool) -> Result<(), String> {
    dst[..msg.len()].copy_from_slice(msg);
    let mut h2 = u32::from_le_bytes(dst[4..8].try_into().unwrap());

    if from_channel {
         
        if h2 & (1 << 11) == 0 {
            return Err(tag!("header part {:x} missing fd tag", h2));
        }
        h2 &= !0xff00;
    } else {
         
        h2 = (h2 & !0xff00) | (1 << 11);
    }
    dst[4..8].copy_from_slice(&u32::to_le_bytes(h2));

    *dst = &mut std::mem::take(dst)[msg.len()..];
    Ok(())
}

 
fn is_server_object(id: ObjId) -> bool {
    id.0 >= 0xff000000
}

 
fn default_proc_way_msg(
    msg: &[u8],
    dst: &mut &mut [u8],
    meth: &WaylandMethod,
    is_req: bool,
    object_id: ObjId,
    glob: &mut Globals,
) -> Result<ProcMsg, String> {
    if dst.len() < msg.len() {
         
         
        return Ok(ProcMsg::NeedsSpace((msg.len(), 0)));
    }

    register_generic_new_ids(msg, meth, glob)?;

    if meth.destructor {
        if !is_req {
             
            glob.objects.remove(&object_id).unwrap();
        } else if is_server_object(object_id) {
             
            glob.objects.remove(&object_id).unwrap();
        } else {
             
        }
    }
    copy_msg(msg, dst);
    Ok(ProcMsg::Done)
}

 
fn proc_unknown_way_msg(
    msg: &[u8],
    dst: &mut &mut [u8],
    transl: TranslationInfo,
) -> Result<ProcMsg, String> {
     
    if let TranslationInfo::FromChannel((x, y)) = transl {
        let mut header2 = u32::from_le_bytes(msg[4..8].try_into().unwrap());
        let ntagfds = ((header2 & ((1 << 16) - 1)) >> 11) as usize;
         
         
         
        if ntagfds > 0 {
            error!("Unidentified message has {} fds attached according to other Waypipe instance; blindly transferring them", ntagfds);
        }

        if dst.len() < msg.len() || y.len() + ntagfds > MAX_OUTGOING_FDS {
             
            return Ok(ProcMsg::NeedsSpace((msg.len(), ntagfds)));
        }

        if x.len() < ntagfds {
            return Err(tag!("Missing sfd"));
        }
        for sfd in x.iter().take(ntagfds) {
            let b = sfd.borrow();
            if let ShadowFdVariant::File(data) = &b.data {
                if data.pending_apply_tasks > 0 {
                    return Ok(ProcMsg::WaitFor(b.remote_id));
                }
            }
        }

        for _ in 0..ntagfds {
            let sfd = x.pop_front().unwrap();
            y.push_back(sfd);
        }

         
        dst[..msg.len()].copy_from_slice(msg);
        header2 &= !0xf800;
        dst[4..8].copy_from_slice(&u32::to_le_bytes(header2));
    } else {
        if dst.len() < msg.len() {
             
            return Ok(ProcMsg::NeedsSpace((msg.len(), 0)));
        }
         
        dst[..msg.len()].copy_from_slice(msg);
    }

    *dst = &mut std::mem::take(dst)[msg.len()..];
    Ok(ProcMsg::Done)
}

 
fn parse_format_table(data: &[u8]) -> Vec<(u32, u64)> {
    let mut t = Vec::new();
    for chunk in data.chunks_exact(16) {
        let format: u32 = u32::from_le_bytes(chunk[..4].try_into().unwrap());
        let modifier: u64 = u64::from_le_bytes(chunk[8..16].try_into().unwrap());
        t.push((format, modifier));
    }
    t
}

 
fn parse_dev_array(arr: &[u8]) -> Option<u64> {
     
    if arr.len() == 4 {
        Some(u32::from_le_bytes(arr.try_into().unwrap()) as u64)
    } else if arr.len() == 8 {
        Some(u64::from_le_bytes(arr.try_into().unwrap()))
    } else {
        None
    }
}

 
fn write_dev_array(dev: u64) -> [u8; SIZEOF_DEV_T] {
    let b = dev.to_le_bytes();
    let (dev, leftover) = b.split_at(SIZEOF_DEV_T);
    assert!(leftover.iter().all(|x| *x == 0));
    dev.try_into().unwrap()
}

 
fn file_has_pending_apply_tasks(sfd: &RefCell<ShadowFd>) -> Result<bool, String> {
    let b = sfd.borrow();
    let ShadowFdVariant::File(data) = &b.data else {
         
        return Err(tag!("ShadowFd is not of file type"));
    };
    Ok(data.pending_apply_tasks > 0)
}

 
fn timespec_midpoint(stamp_1: libc::timespec, stamp_3: libc::timespec) -> libc::timespec {
    let mut mid_nsec = if stamp_1.tv_nsec < stamp_3.tv_nsec {
        stamp_1.tv_nsec + (stamp_3.tv_nsec - stamp_1.tv_nsec) / 2
    } else {
        stamp_3.tv_nsec + (stamp_1.tv_nsec - stamp_3.tv_nsec) / 2
    };
    let mut mid_sec = if stamp_1.tv_sec < stamp_3.tv_sec {
        stamp_1.tv_sec + (stamp_3.tv_sec - stamp_1.tv_sec) / 2
    } else {
        stamp_3.tv_sec + (stamp_1.tv_sec - stamp_3.tv_sec) / 2
    };
    if stamp_3.tv_sec % 2 != stamp_1.tv_sec % 2 {
        mid_nsec += 500_000_000;
    }
    if mid_nsec > 1_000_000_000 {
        mid_sec += 1;
        mid_nsec -= 1_000_000_000;
    }

    libc::timespec {
        tv_sec: mid_sec,
        tv_nsec: mid_nsec,
    }
}

 
fn clock_sub(clock_a: u32, clock_b: u32) -> Result<(i64, u32), String> {
    let mut stamp_1 = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    let mut stamp_2 = stamp_1;
    let mut stamp_3 = stamp_1;
    let ca: libc::clockid_t = clock_a.try_into().unwrap();
    let cb: libc::clockid_t = clock_b.try_into().unwrap();
    unsafe {
         
         
        let ret1 = libc::clock_gettime(ca, &mut stamp_1);
        let ret2 = libc::clock_gettime(cb, &mut stamp_2);
        let ret3 = libc::clock_gettime(ca, &mut stamp_3);
        if ret1 != 0 || ret2 != 0 || ret3 != 0 {
            return Err(tag!(
                "clock_gettime failed for clock {} or {}",
                clock_a,
                clock_b
            ));
        }
    }

    let stamp_avg = timespec_midpoint(stamp_1, stamp_3);
     
    #[allow(clippy::unnecessary_cast)]
    let mut tv_sec = stamp_avg
        .tv_sec
        .checked_sub(stamp_2.tv_sec)
        .ok_or_else(|| tag!("overflow"))? as i64;
    let mut tv_nsec = stamp_avg
        .tv_nsec
        .checked_sub(stamp_2.tv_nsec)
        .ok_or_else(|| tag!("overflow"))? as i32;

    if tv_nsec < 0 {
        tv_sec -= 1;
        tv_nsec += 1_000_000_000;
    }
    assert!((0..1_000_000_000).contains(&tv_nsec));

    Ok((tv_sec, tv_nsec as u32))
}

 
fn time_add(mut a: (u64, u32), b: (i64, u32)) -> Option<(u64, u32)> {
    assert!(a.1 < 1_000_000_000 && b.1 < 1_000_000_000);
    let mut nsec = a.1.checked_add(b.1)?;
    if nsec > 1_000_000_000 {
        nsec -= 1_000_000_000;
        a.0 = a.0.checked_add(1)?;
    }

     
    Some((a.0.checked_add_signed(b.0)?, nsec))
}

 
fn translate_timestamp(
    tv_sec_hi: u32,
    tv_sec_lo: u32,
    tv_nsec: u32,
    clock_id: u32,
    to_channel: bool,
) -> Result<(u32, u32, u32), String> {
    let tv_sec = join_u64(tv_sec_hi, tv_sec_lo);
    let realtime = libc::CLOCK_REALTIME as u32;
    let (new_sec, new_nsec) = if to_channel {
         
        let (diff_sec, diff_nsec) = clock_sub(realtime, clock_id)?;
        time_add((tv_sec, tv_nsec), (diff_sec, diff_nsec)).ok_or_else(|| tag!("overflow"))?
    } else {
         
        let (diff_sec, diff_nsec) = clock_sub(clock_id, realtime)?;
        time_add((tv_sec, tv_nsec), (diff_sec, diff_nsec)).ok_or_else(|| tag!("overflow"))?
    };
    let (new_sec_hi, new_sec_lo) = split_u64(new_sec);
    Ok((new_sec_hi, new_sec_lo, new_nsec))
}

 
fn translate_or_wait_for_fixed_file(
    transl: TranslationInfo,
    glob: &mut Globals,
    file_sz: u32,
) -> Result<Option<ProcMsg>, String> {
    match transl {
        TranslationInfo::FromChannel((x, y)) => {
            let sfd = &x.front().ok_or_else(|| tag!("Missing fd"))?;
            let rid = sfd.borrow().remote_id;
            if file_has_pending_apply_tasks(sfd)? {
                return Ok(Some(ProcMsg::WaitFor(rid)));
            }
            y.push_back(x.pop_front().unwrap());
        }
        TranslationInfo::FromWayland((x, y)) => {
            let v = translate_shm_fd(
                x.pop_front().ok_or_else(|| tag!("Missing fd"))?,
                file_sz.try_into().unwrap(),
                &mut glob.map,
                &mut glob.max_local_id,
                true,
                true,
                false,
            )?;
            y.push(v);
        }
    };
    Ok(None)
}

 
struct MethodArguments<'a> {
    meth: &'a WaylandMethod,
    msg: &'a [u8],
}
 
fn fmt_method(arg: &MethodArguments, f: &mut Formatter<'_>) -> Result<bool, &'static str> {
    assert!(arg.msg.len() >= 8);
    let mut tail: &[u8] = &arg.msg[8..];

    let mut first = true;
    for op in arg.meth.sig {
        if !first {
            if write!(f, ", ").is_err() {
                return Ok(true);
            }
        } else {
            first = false;
        }
        match op {
            WaylandArgument::Uint => {
                let v = parse_u32(&mut tail)?;
                if write!(f, "{}:u", v).is_err() {
                    return Ok(true);
                }
            }
            WaylandArgument::Int => {
                let v = parse_i32(&mut tail)?;
                if write!(f, "{}:i", v).is_err() {
                    return Ok(true);
                }
            }
            WaylandArgument::Fixed => {
                let v = parse_i32(&mut tail)?;
                if write!(f, "{:.8}:f", (v as f64) * 0.00390625).is_err() {
                    return Ok(true);
                }
            }
            WaylandArgument::Fd => {
                 
                if write!(f, "fd").is_err() {
                    return Ok(true);
                }
            }
            WaylandArgument::Object(t) => {
                let id = parse_u32(&mut tail)?;
                if write!(f, "{}#{}:obj", INTERFACE_TABLE[*t as usize].name, id).is_err() {
                    return Ok(true);
                }
            }
            WaylandArgument::NewId(t) => {
                let id = parse_u32(&mut tail)?;
                if write!(f, "{}#{}:new_id", INTERFACE_TABLE[*t as usize].name, id).is_err() {
                    return Ok(true);
                }
            }
            WaylandArgument::GenericObject => {
                let id = parse_u32(&mut tail)?;
                if write!(f, "{}:gobj", id).is_err() {
                    return Ok(true);
                }
            }
            WaylandArgument::GenericNewId => {
                 
                let ostring = parse_string(&mut tail)?;
                let version = parse_u32(&mut tail)?;
                let id = parse_u32(&mut tail)?;
                if (if let Some(string) = ostring {
                    write!(
                        f,
                        "(\"{}\", {}:u, {}:new_id)",
                        EscapeAsciiPrintable(string),
                        version,
                        id
                    )
                } else {
                    write!(f, "(null_str, {}:u, {}:new_id)", version, id)
                })
                .is_err()
                {
                    return Ok(true);
                }
            }
            WaylandArgument::String | WaylandArgument::OptionalString => {
                let ostring = parse_string(&mut tail)?;
                if (if let Some(string) = ostring {
                    write!(f, "\"{}\"", EscapeAsciiPrintable(string))
                } else {
                    write!(f, "null_str")
                })
                .is_err()
                {
                    return Ok(true);
                }
            }
            WaylandArgument::Array => {
                let a = parse_array(&mut tail)?;
                if write!(f, "{:?}", a).is_err() {
                    return Ok(true);
                }
            }
        }
    }
    if !tail.is_empty() {
        return Err(PARSE_ERROR);
    }
    Ok(false)
}

impl Display for MethodArguments<'_> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match fmt_method(self, f) {
            Err(e) => write!(f, "...format error: {}", e),
            Ok(eof) => {
                if eof {
                    Err(std::fmt::Error)
                } else {
                    Ok(())
                }
            }
        }
    }
}

 
#[derive(Debug, Eq, PartialEq)]
pub enum ProcMsg {
     
    Done,
     
    NeedsSpace((usize, usize)),
     
    WaitFor(Rid),
}

 
fn space_le(x: (usize, usize), y: (usize, usize)) -> bool {
    x.0 <= y.0 && x.1 <= y.1
}

 
macro_rules! check_space {
    ($x:expr, $y:expr, $r:expr) => {
        let space: (usize, usize) = ($x, $y);
        if !space_le(space, $r) {
            return Ok(ProcMsg::NeedsSpace(space));
        }
    };
}

 
pub fn process_way_msg(
    msg: &[u8],
    dst: &mut &mut [u8],
    transl: TranslationInfo,
    glob: &mut Globals,
) -> Result<ProcMsg, String> {
    let object_id = ObjId(u32::from_le_bytes(msg[0..4].try_into().unwrap()));
    let header2 = u32::from_le_bytes(msg[4..8].try_into().unwrap());
    let length = (header2 >> 16) as usize;
    assert!(msg.len() == length);
     
    let opcode = (header2 & ((1 << 11) - 1)) as usize;

    let (from_channel, outgoing_fds): (bool, usize) = match &transl {
        TranslationInfo::FromChannel((_x, y)) => (true, y.len()),
        TranslationInfo::FromWayland((_x, y)) => (false, y.len()),
    };

    let is_req = glob.on_display_side == from_channel;
    let Some(ref mut obj) = glob.objects.get_mut(&object_id) else {
        debug!(
            "Processing {} on unknown object {}; opcode {} length {}",
            if is_req { "request" } else { "event" },
            object_id,
            opcode,
            length
        );
        return proc_unknown_way_msg(msg, dst, transl);
    };

    let opt_meth: Option<&WaylandMethod> = if is_req {
        INTERFACE_TABLE[obj.obj_type as usize].reqs.get(opcode)
    } else {
        INTERFACE_TABLE[obj.obj_type as usize].evts.get(opcode)
    };
    if opt_meth.is_none() {
        debug!(
            "Method out of range: {}#{}, opcode {}",
            INTERFACE_TABLE[obj.obj_type as usize].name, object_id, opcode
        );
        return proc_unknown_way_msg(msg, dst, transl);
    }

    let meth = opt_meth.unwrap();
     
    if log::log_enabled!(log::Level::Debug) {
        debug!(
            "Processing {}: {}#{}.{}({})",
            if is_req { "request" } else { "event" },
            INTERFACE_TABLE[obj.obj_type as usize].name,
            object_id,
            meth.name,
            MethodArguments { meth, msg }
        );
    }

    assert!(opcode <= u8::MAX as usize);
    let mod_opcode = if is_req {
        MethodId::Request(opcode as u8)
    } else {
        MethodId::Event(opcode as u8)
    };

    let remaining_space = (dst.len(), MAX_OUTGOING_FDS - outgoing_fds);

    match (obj.obj_type, mod_opcode) {
        (WaylandInterface::WlDisplay, OPCODE_WL_DISPLAY_DELETE_ID) => {
            check_space!(msg.len(), 0, remaining_space);

            let object_id = ObjId(parse_evt_wl_display_delete_id(msg)?);
            if object_id == ObjId(1) {
                return Err(tag!("Tried to delete wl_display object"));
            }

            if let Some(_removed) = glob.objects.remove(&object_id) {
                 
            } else {
                debug!("Deleted untracked object");
            }

            copy_msg(msg, dst);

            Ok(ProcMsg::Done)
        }
        (WaylandInterface::WlDisplay, OPCODE_WL_DISPLAY_GET_REGISTRY) => {
            check_space!(msg.len(), 0, remaining_space);

            let registry_id = parse_req_wl_display_get_registry(msg)?;
            insert_new_object(
                &mut glob.objects,
                registry_id,
                WpObject {
                    obj_type: WaylandInterface::WlRegistry,
                    extra: WpExtra::WlRegistry(Box::new(ObjWlRegistry {
                        syncobj_manager_replay: Vec::new(),
                    })),
                },
            )?;
            copy_msg(msg, dst);
            Ok(ProcMsg::Done)
        }
        (WaylandInterface::WlCallback, OPCODE_WL_CALLBACK_DONE) => {
            check_space!(msg.len(), 0, remaining_space);
            copy_msg(msg, dst);
             
            glob.objects.remove(&object_id);

             
            Ok(ProcMsg::Done)
        }
        (WaylandInterface::WlShm, OPCODE_WL_SHM_CREATE_POOL) => {
            check_space!(msg.len(), 1, remaining_space);

            let (pool_id, pool_size) = parse_req_wl_shm_create_pool(msg)?;
            let pos_size = pool_size
                .try_into()
                .map_err(|_| tag!("Need nonnegative shm pool size, given {}", pool_size))?;

            let buffer: Rc<RefCell<ShadowFd>> = match transl {
                TranslationInfo::FromChannel((x, y)) => {
                    let sfd = x.pop_front().ok_or_else(|| tag!("Missing fd"))?;
                    y.push_back(sfd.clone());
                    sfd
                }
                TranslationInfo::FromWayland((x, y)) => {
                     
                     

                    let v = translate_shm_fd(
                        x.pop_front().ok_or_else(|| tag!("Missing fd"))?,
                        pos_size,
                        &mut glob.map,
                        &mut glob.max_local_id,
                        false,
                        false,
                        false,
                    )?;
                    y.push(v.clone());
                    v
                }
            };

            insert_new_object(
                &mut glob.objects,
                pool_id,
                WpObject {
                    obj_type: WaylandInterface::WlShmPool,
                    extra: WpExtra::WlShmPool(Box::new(ObjWlShmPool { buffer })),
                },
            )?;

            copy_msg_tag_fd(msg, dst, from_channel)?;

            Ok(ProcMsg::Done)
        }
        (WaylandInterface::WlShmPool, OPCODE_WL_SHM_POOL_CREATE_BUFFER) => {
            check_space!(msg.len(), 0, remaining_space);
            copy_msg(msg, dst);

            let (buffer_id, offset, width, height, stride, format) =
                parse_req_wl_shm_pool_create_buffer(msg)?;

            let sfd = if let WpExtra::WlShmPool(ref x) = &obj.extra {
                x.buffer.clone()
            } else {
                return Err(tag!("wl_shm_pool object has invalid extra type"));
            };

            insert_new_object(
                &mut glob.objects,
                buffer_id,
                WpObject {
                    obj_type: WaylandInterface::WlBuffer,
                    extra: WpExtra::WlBuffer(Box::new(ObjWlBuffer {
                        sfd,
                        shm_info: Some(ObjWlBufferShm {
                            width,
                            height,
                            format,
                            offset,
                            stride,
                        }),
                        unique_id: glob.max_buffer_uid,
                    })),
                },
            )?;
            glob.max_buffer_uid += 1;

            Ok(ProcMsg::Done)
        }

        (WaylandInterface::WlShmPool, OPCODE_WL_SHM_POOL_RESIZE) => {
            check_space!(msg.len(), 0, remaining_space);

            let WpExtra::WlShmPool(ref x) = &obj.extra else {
                return Err(tag!("wl_shm_pool object has invalid extra type"));
            };

            if file_has_pending_apply_tasks(&x.buffer)? {
                let b = x.buffer.borrow();
                return Ok(ProcMsg::WaitFor(b.remote_id));
            }

            copy_msg(msg, dst);

            if glob.on_display_side {
                 
                return Ok(ProcMsg::Done);
            }

            let size = parse_req_wl_shm_pool_resize(msg)?;
            let new_size: usize = size
                .try_into()
                .map_err(|_| tag!("Invalid buffer size: {}", size))?;

            let x: &mut ShadowFd = &mut x.buffer.borrow_mut();
            if let ShadowFdVariant::File(ref mut y) = x.data {
                y.buffer_size = new_size;

                 
                 
                update_core_for_new_size(&y.fd, new_size, &mut y.core)?;
            }

            Ok(ProcMsg::Done)
        }
        (WaylandInterface::WlCompositor, OPCODE_WL_COMPOSITOR_CREATE_SURFACE) => {
            check_space!(msg.len(), 0, remaining_space);

            let surf_id = parse_req_wl_compositor_create_surface(msg)?;

            let d = DamageBatch {
                damage: Vec::new(),
                damage_buffer: Vec::new(),
                attachment: BufferAttachment {
                    scale: 1,
                    transform: WlOutputTransform::Normal,
                    viewport_src: None,
                    viewport_dst: None,
                    buffer_uid: 0,
                    buffer_size: (0, 0),
                },
            };

            insert_new_object(
                &mut glob.objects,
                surf_id,
                WpObject {
                    obj_type: WaylandInterface::WlSurface,
                    extra: WpExtra::WlSurface(Box::new(ObjWlSurface {
                        attached_buffer_id: None,
                        damage_history: [
                            d.clone(),
                            d.clone(),
                            d.clone(),
                            d.clone(),
                            d.clone(),
                            d.clone(),
                            d.clone(),
                        ],
                        acquire_pt: None,
                        release_pt: None,
                        viewport_id: None,
                    })),
                },
            )?;

            copy_msg(msg, dst);

            Ok(ProcMsg::Done)
        }
        (WaylandInterface::WlSurface, OPCODE_WL_SURFACE_DESTROY) => {
            check_space!(msg.len(), 0, remaining_space);
            copy_msg(msg, dst);

            let WpExtra::WlSurface(ref mut surf) = obj.extra else {
                return Err(tag!("Surface object has invalid extra type"));
            };
            let mut tmp = None;
            std::mem::swap(&mut tmp, &mut surf.viewport_id);
            if let Some(vp_id) = tmp {
                if let Some(ref mut object) = glob.objects.get_mut(&vp_id) {
                    let WpExtra::WpViewport(ref mut viewport) = object.extra else {
                        return Err(tag!("Viewport object has invalid extra type"));
                    };
                    viewport.wl_surface = None;
                }
            }
            Ok(ProcMsg::Done)
        }
        (WaylandInterface::WlSurface, OPCODE_WL_SURFACE_ATTACH) => {
            check_space!(msg.len(), 0, remaining_space);

            let (buf_id, _x, _y) = parse_req_wl_surface_attach(msg)?;

            if let Some(ref mut object) = glob.objects.get_mut(&object_id) {
                if let WpExtra::WlSurface(ref mut x) = &mut object.extra {
                    x.attached_buffer_id = if buf_id != ObjId(0) {
                        Some(buf_id)
                    } else {
                        None
                    };
                } else {
                    return Err(tag!("Surface object has invalid extra type"));
                }
            } else {
                return Err(tag!("Attaching to nonexistant object"));
            }

            copy_msg(msg, dst);

            Ok(ProcMsg::Done)
        }
        (WaylandInterface::WlSurface, OPCODE_WL_SURFACE_SET_BUFFER_SCALE) => {
            check_space!(msg.len(), 0, remaining_space);
            copy_msg(msg, dst);

            let s = parse_req_wl_surface_set_buffer_scale(msg)?;

            if s <= 0 {
                return Err(tag!("wl_surface.set_buffer_scale used nonpositive scale"));
            }

            let WpExtra::WlSurface(ref mut surf) = &mut obj.extra else {
                return Err(tag!("Surface object has invalid extra type"));
            };

            surf.damage_history[0].attachment.scale = s as u32;

            Ok(ProcMsg::Done)
        }
        (WaylandInterface::WlSurface, OPCODE_WL_SURFACE_SET_BUFFER_TRANSFORM) => {
            check_space!(msg.len(), 0, remaining_space);
            copy_msg(msg, dst);

            let t = parse_req_wl_surface_set_buffer_transform(msg)?;

            let WpExtra::WlSurface(ref mut surf) = &mut obj.extra else {
                return Err(tag!("Surface object has invalid extra type"));
            };
            if t < 0 {
                return Err(tag!("Buffer transform value should be nonnegative"));
            }
            surf.damage_history[0].attachment.transform = (t as u32)
                .try_into()
                .map_err(|()| "Not a valid transform type")?;

            Ok(ProcMsg::Done)
        }
        (WaylandInterface::WlSurface, OPCODE_WL_SURFACE_DAMAGE) => {
            check_space!(msg.len(), 0, remaining_space);
            copy_msg(msg, dst);

            if glob.on_display_side {
                 
                return Ok(ProcMsg::Done);
            }

            let WpExtra::WlSurface(ref mut surf) = &mut obj.extra else {
                return Err(tag!("Surface object has invalid extra type"));
            };

            let (x, y, width, height) = parse_req_wl_surface_damage(msg)?;
            if width <= 0 || height <= 0 {
                 
                error!(
                    "Received degenerate damage rectangle: x={} y={} w={} h={}",
                    x, y, width, height
                );
            } else {
                surf.damage_history[0].damage.push(WlRect {
                    x,
                    y,
                    width,
                    height,
                });
            }

            Ok(ProcMsg::Done)
        }
        (WaylandInterface::WlSurface, OPCODE_WL_SURFACE_DAMAGE_BUFFER) => {
            check_space!(msg.len(), 0, remaining_space);
            copy_msg(msg, dst);

            if glob.on_display_side {
                 
                return Ok(ProcMsg::Done);
            }

            let WpExtra::WlSurface(ref mut surf) = &mut obj.extra else {
                return Err(tag!("Surface object has invalid extra type"));
            };

            let (x, y, width, height) = parse_req_wl_surface_damage_buffer(msg)?;
            if width <= 0 || height <= 0 {
                 
                error!(
                    "Received degenerate damage rectangle: x={} y={} w={} h={}",
                    x, y, width, height
                );
            } else {
                surf.damage_history[0].damage_buffer.push(WlRect {
                    x,
                    y,
                    width,
                    height,
                });
            }

            Ok(ProcMsg::Done)
        }
        (WaylandInterface::WlSurface, OPCODE_WL_SURFACE_COMMIT) => {
            check_space!(msg.len(), 0, remaining_space);

            let () = parse_req_wl_surface_commit(msg)?;
            let WpExtra::WlSurface(ref x) = obj.extra else {
                return Err(tag!("Surface object has invalid extra type"));
            };
            let opt_buf_id: Option<ObjId> = x.attached_buffer_id;
            if x.acquire_pt.is_some() != x.release_pt.is_some() {
                return Err(tag!("Acquire/release points must both be set"));
            }
            let has_timelines = x.acquire_pt.is_some() || x.release_pt.is_some();

            if from_channel {
                if let Some(buf_id) = opt_buf_id {
                    if let Some(buf) = glob.objects.get(&buf_id) {
                        if let WpExtra::WlBuffer(ref buf_data) = buf.extra {
                            let b = buf_data.sfd.borrow();
                            let apply_count = if let ShadowFdVariant::File(data) = &b.data {
                                data.pending_apply_tasks
                            } else if let ShadowFdVariant::Dmabuf(data) = &b.data {
                                 
                                if has_timelines {
                                    0  
                                } else {
                                    data.pending_apply_tasks
                                }
                            } else {
                                return Err(tag!("Attached buffer is not of file or dmabuf type"));
                            };
                            if apply_count > 0 {
                                return Ok(ProcMsg::WaitFor(b.remote_id));
                            }
                        }
                    }
                }
            }

            copy_msg(msg, dst);

            if glob.on_display_side {
                 
                let obj = &mut glob.objects.get_mut(&object_id).unwrap();
                let WpExtra::WlSurface(ref mut x) = &mut obj.extra else {
                    return Err(tag!("Surface object has invalid extra type"));
                };

                if x.acquire_pt.is_some() != x.release_pt.is_some() {
                    return Err(tag!("Acquire/release points must both be set"));
                }
                 
                let mut acq_pt = None;
                let mut rel_pt = None;
                std::mem::swap(&mut x.acquire_pt, &mut acq_pt);
                std::mem::swap(&mut x.release_pt, &mut rel_pt);

                let opt_buf_id: Option<ObjId> = x.attached_buffer_id;
                if let Some(buf_id) = opt_buf_id {
                    if let Some(buf) = glob.objects.get(&buf_id) {
                        if let WpExtra::WlBuffer(ref buf_data) = buf.extra {
                            let mut sfd = buf_data.sfd.borrow_mut();
                            if let ShadowFdVariant::Dmabuf(ref mut y) = &mut sfd.data {
                                dmabuf_post_apply_task_operations(y)?;

                                if let Some((pt, timeline)) = acq_pt {
                                    y.acquires.push((pt, timeline));
                                }
                                if let Some((pt, timeline)) = rel_pt {
                                    let mut tsfd = timeline.borrow_mut();
                                    let ShadowFdVariant::Timeline(ref mut timeline_data) =
                                        tsfd.data
                                    else {
                                        panic!("Expected timeline sfd");
                                    };
                                    timeline_data.releases.push((pt, buf_data.sfd.clone()));
                                    let trid = tsfd.remote_id;
                                    drop(tsfd);

                                    y.releases.insert((trid, pt), timeline);
                                }
                                if y.pending_apply_tasks == 0 {
                                     
                                    debug!("Tasks already done, signalling acquires");
                                    signal_timeline_acquires(&mut y.acquires)?;
                                }
                            }
                        }
                    }
                }

                return Ok(ProcMsg::Done);
            }

            let obj = &mut glob.objects.get_mut(&object_id).unwrap();
            let WpExtra::WlSurface(ref mut x) = &mut obj.extra else {
                return Err(tag!("Surface object has invalid extra type"));
            };

            if x.acquire_pt.is_some() != x.release_pt.is_some() {
                return Err(tag!("Acquire/release points must both be set"));
            }
             
            let mut acq_pt = None;
            let mut rel_pt = None;
            std::mem::swap(&mut x.acquire_pt, &mut acq_pt);
            std::mem::swap(&mut x.release_pt, &mut rel_pt);

            let mut current_attachment = x.damage_history[0].attachment.clone();

             
             

            let mut found_buffer = false;
            if let Some(buf_id) = opt_buf_id {
                 
                if let Some(buf) = glob.objects.get(&buf_id) {
                    let obj = &glob.objects.get(&object_id).unwrap();
                    let WpExtra::WlSurface(ref x) = &obj.extra else {
                        unreachable!();
                    };
                    if let WpExtra::WlBuffer(ref buf_data) = buf.extra {
                        let mut sfd = buf_data.sfd.borrow_mut();
                        let buffer_size: (i32, i32) = if let ShadowFdVariant::File(_) = sfd.data {
                            let Some(shm_info) = buf_data.shm_info else {
                                return Err(tag!(
                                    "Expected shm info for wl_buffer with File-type ShadowFd"
                                ));
                            };
                            (shm_info.width, shm_info.height)
                        } else if let ShadowFdVariant::Dmabuf(ref y) = sfd.data {
                            match y.buf {
                                DmabufImpl::Vulkan(ref buf) => (
                                    buf.width.try_into().unwrap(),
                                    buf.height.try_into().unwrap(),
                                ),
                                DmabufImpl::Gbm(ref buf) => (
                                    buf.width.try_into().unwrap(),
                                    buf.height.try_into().unwrap(),
                                ),
                            }
                        } else {
                            return Err(tag!("Expected buffer shadowfd to be of file type"));
                        };

                        current_attachment.buffer_uid = buf_data.unique_id;
                        current_attachment.buffer_size = buffer_size;
                        found_buffer = true;

                        if let ShadowFdVariant::File(ref mut y) = &mut sfd.data {
                            match &y.damage {
                                Damage::Everything => {}
                                Damage::Intervals(old) => {
                                    let dmg = get_damage_for_shm(buf_data, x, &current_attachment);
                                    y.damage =
                                        Damage::Intervals(union_damage(&old[..], &dmg[..], 128));
                                }
                            }
                        } else if let ShadowFdVariant::Dmabuf(ref mut y) = &mut sfd.data {
                            match &y.damage {
                                Damage::Everything => {}
                                Damage::Intervals(old) => {
                                    let dmg = get_damage_for_dmabuf(y, x, &current_attachment);
                                    y.damage =
                                        Damage::Intervals(union_damage(&old[..], &dmg[..], 128));
                                }
                            }
                            if acq_pt.is_none() {
                                y.using_implicit_sync = true;
                            }

                             
                            if let Some((pt, timeline)) = acq_pt {
                                y.acquires.push((pt, timeline));
                            }
                            if let Some((pt, timeline)) = rel_pt {
                                let mut tsfd = timeline.borrow_mut();
                                let ShadowFdVariant::Timeline(ref mut timeline_data) = tsfd.data
                                else {
                                    panic!("Expected timeline sfd");
                                };
                                timeline_data.releases.push((pt, buf_data.sfd.clone()));
                                let trid = tsfd.remote_id;
                                drop(tsfd);

                                y.releases.insert((trid, pt), timeline);
                            }
                        } else {
                            unreachable!();
                        }
                    }
                } else {
                    debug!("Attached wl_buffer {} for wl_surface {} destroyed before commit: the result of this is not specified and compositors may do anything. Interpreting as null attachment.", buf_id, object_id);
                }
            }

             
            let obj = &mut glob.objects.get_mut(&object_id).unwrap();

            let WpExtra::WlSurface(ref mut x) = &mut obj.extra else {
                return Err(tag!("Surface object has invalid extra type"));
            };

            if found_buffer {
                 
                x.damage_history[0].attachment = current_attachment.clone();
                 
                let mut fresh = DamageBatch {
                    attachment: current_attachment.clone(),
                    damage: Vec::new(),
                    damage_buffer: Vec::new(),
                };
                std::mem::swap(&mut x.damage_history[6], &mut fresh);
                x.damage_history.swap(5, 6);
                x.damage_history.swap(4, 5);
                x.damage_history.swap(3, 4);
                x.damage_history.swap(2, 3);
                x.damage_history.swap(1, 2);
                x.damage_history.swap(0, 1);
            } else {
                 
                current_attachment.buffer_uid = 0;
                current_attachment.buffer_size = (0, 0);
                for i in 0..7 {
                    x.damage_history[i] = DamageBatch {
                        attachment: current_attachment.clone(),
                        damage: Vec::new(),
                        damage_buffer: Vec::new(),
                    };
                }
            }

            Ok(ProcMsg::Done)
        }
        (WaylandInterface::WpViewporter, OPCODE_WP_VIEWPORTER_GET_VIEWPORT) => {
            check_space!(msg.len(), 0, remaining_space);
            copy_msg(msg, dst);

            let (new_id, surface) = parse_req_wp_viewporter_get_viewport(msg)?;
            insert_new_object(
                &mut glob.objects,
                new_id,
                WpObject {
                    obj_type: WaylandInterface::WpViewport,
                    extra: WpExtra::WpViewport(Box::new(ObjWpViewport {
                        wl_surface: Some(surface),
                    })),
                },
            )?;

            Ok(ProcMsg::Done)
        }
        (WaylandInterface::WpViewport, OPCODE_WP_VIEWPORT_DESTROY) => {
            check_space!(msg.len(), 0, remaining_space);
            copy_msg(msg, dst);

            let WpExtra::WpViewport(ref mut viewport) = obj.extra else {
                return Err(tag!("Viewport object has invalid extra type"));
            };
            let mut tmp = None;
            std::mem::swap(&mut tmp, &mut viewport.wl_surface);
            if let Some(surf_id) = tmp {
                if let Some(ref mut object) = glob.objects.get_mut(&surf_id) {
                    let WpExtra::WlSurface(ref mut surface) = object.extra else {
                        return Err(tag!("Surface object has invalid extra type"));
                    };
                    surface.damage_history[0].attachment.viewport_src = None;
                    surface.damage_history[0].attachment.viewport_dst = None;
                    surface.viewport_id = None;
                }
            }
            Ok(ProcMsg::Done)
        }
        (WaylandInterface::WpViewport, OPCODE_WP_VIEWPORT_SET_DESTINATION) => {
            check_space!(msg.len(), 0, remaining_space);
            copy_msg(msg, dst);

            let (w, h) = parse_req_wp_viewport_set_destination(msg)?;
            let destination: Option<(i32, i32)> = if w == -1 && h == -1 {
                None
            } else if w <= 0 || h <= 0 {
                return Err(tag!("invalid wp_viewport destination ({},{})", w, h));
            } else {
                Some((w, h))
            };

            let WpExtra::WpViewport(ref mut viewport) = obj.extra else {
                return Err(tag!("Viewport object has invalid extra type"));
            };
            if let Some(surf_id) = viewport.wl_surface {
                if let Some(ref mut object) = glob.objects.get_mut(&surf_id) {
                    let WpExtra::WlSurface(ref mut surface) = object.extra else {
                        return Err(tag!("Surface object has invalid extra type"));
                    };

                    surface.damage_history[0].attachment.viewport_dst = destination;
                }
            }
            Ok(ProcMsg::Done)
        }
        (WaylandInterface::WpViewport, OPCODE_WP_VIEWPORT_SET_SOURCE) => {
            check_space!(msg.len(), 0, remaining_space);
            copy_msg(msg, dst);

            let (x, y, w, h) = parse_req_wp_viewport_set_source(msg)?;
            let source: Option<(i32, i32, i32, i32)> =
                if x == -256 && y == -256 && w == -256 && h == -256 {
                    None
                } else if x < 0 || y < 0 || w <= 0 || h <= 0 {
                    return Err(tag!("invalid wp_viewport source ({},{},{},{})", x, y, w, h));
                } else {
                    Some((x, y, w, h))
                };

            let WpExtra::WpViewport(ref mut viewport) = obj.extra else {
                return Err(tag!("Viewport object has invalid extra type"));
            };
            if let Some(surf_id) = viewport.wl_surface {
                if let Some(ref mut object) = glob.objects.get_mut(&surf_id) {
                    let WpExtra::WlSurface(ref mut surface) = object.extra else {
                        return Err(tag!("Surface object has invalid extra type"));
                    };

                    surface.damage_history[0].attachment.viewport_src = source;
                }
            }
            Ok(ProcMsg::Done)
        }
        (
            WaylandInterface::WpLinuxDrmSyncobjManagerV1,
            OPCODE_WP_LINUX_DRM_SYNCOBJ_MANAGER_V1_GET_SURFACE,
        ) => {
            check_space!(msg.len(), 0, remaining_space);
            copy_msg(msg, dst);

            let (new_id, surf_id) = parse_req_wp_linux_drm_syncobj_manager_v1_get_surface(msg)?;

             
             

            insert_new_object(
                &mut glob.objects,
                new_id,
                WpObject {
                    obj_type: WaylandInterface::WpLinuxDrmSyncobjSurfaceV1,
                    extra: WpExtra::WpDrmSyncobjSurface(Box::new(ObjWpDrmSyncobjSurface {
                         
                        surface: surf_id,
                    })),
                },
            )?;

            Ok(ProcMsg::Done)
        }
        (
            WaylandInterface::WpLinuxDrmSyncobjManagerV1,
            OPCODE_WP_LINUX_DRM_SYNCOBJ_MANAGER_V1_IMPORT_TIMELINE,
        ) => {
            check_space!(msg.len(), 1, remaining_space);

            let new_id = parse_req_wp_linux_drm_syncobj_manager_v1_import_timeline(msg)?;

            let sfd = match transl {
                TranslationInfo::FromChannel((x, y)) => {
                    let sfd = x.pop_front().ok_or_else(|| tag!("Missing sfd"))?;
                    let mut b = sfd.borrow_mut();
                    if let ShadowFdVariant::Timeline(t) = &mut b.data {
                        t.debug_wayland_id = new_id;
                    } else {
                        return Err(tag!("Expected timeline fd"));
                    }
                    drop(b);

                    y.push_back(sfd.clone());
                    sfd
                }
                TranslationInfo::FromWayland((x, y)) => {
                    let v = translate_timeline(
                        x.pop_front().ok_or_else(|| tag!("Missing fd"))?,
                        glob,
                        new_id,
                    )?;
                    y.push(v.clone());
                    v
                }
            };

            insert_new_object(
                &mut glob.objects,
                new_id,
                WpObject {
                    obj_type: WaylandInterface::WpLinuxDrmSyncobjTimelineV1,
                    extra: WpExtra::WpDrmSyncobjTimeline(Box::new(ObjWpDrmSyncobjTimeline {
                         
                        timeline: sfd,
                    })),
                },
            )?;

            copy_msg_tag_fd(msg, dst, from_channel)?;
            Ok(ProcMsg::Done)
        }
        (
            WaylandInterface::WpLinuxDrmSyncobjSurfaceV1,
            OPCODE_WP_LINUX_DRM_SYNCOBJ_SURFACE_V1_SET_ACQUIRE_POINT,
        )
        | (
            WaylandInterface::WpLinuxDrmSyncobjSurfaceV1,
            OPCODE_WP_LINUX_DRM_SYNCOBJ_SURFACE_V1_SET_RELEASE_POINT,
        ) => {
            check_space!(msg.len(), 0, remaining_space);

            let acquire = mod_opcode == OPCODE_WP_LINUX_DRM_SYNCOBJ_SURFACE_V1_SET_ACQUIRE_POINT;

            let (timeline_id, pt_hi, pt_lo) = if acquire {
                parse_req_wp_linux_drm_syncobj_surface_v1_set_acquire_point(msg)?
            } else {
                parse_req_wp_linux_drm_syncobj_surface_v1_set_release_point(msg)?
            };
            let pt = join_u64(pt_hi, pt_lo);

            let WpExtra::WpDrmSyncobjSurface(s) = &obj.extra else {
                panic!("Incorrect extra type");
            };
            let surf_id = s.surface;

            let timeline_obj = glob.objects.get(&timeline_id).ok_or("")?;
            let WpExtra::WpDrmSyncobjTimeline(t) = &timeline_obj.extra else {
                return Err(tag!("Incorrect extra type, expected timeline"));
            };
            let sfd = t.timeline.clone();

             
             

            let surface_obj = glob.objects.get_mut(&surf_id).ok_or("")?;
            let WpExtra::WlSurface(ref mut surf) = &mut surface_obj.extra else {
                return Err(tag!("Incorrect extra type, expected surface"));
            };

            if acquire {
                surf.acquire_pt = Some((pt, sfd));
            } else {
                surf.release_pt = Some((pt, sfd));
            }

            copy_msg(msg, dst);
            Ok(ProcMsg::Done)
        }
        (WaylandInterface::ZwpLinuxDmabufV1, OPCODE_ZWP_LINUX_DMABUF_V1_FORMAT) => {
            let format = parse_evt_zwp_linux_dmabuf_v1_format(msg)?;

            match glob.dmabuf_device {
                DmabufDevice::Unknown
                | DmabufDevice::Unavailable
                | DmabufDevice::VulkanSetup(_) => unreachable!(),
                DmabufDevice::Vulkan((_, ref vulk)) => {
                    let mod_linear = 0;
                    if !vulk.supports_format(format, mod_linear) {
                         
                        return Ok(ProcMsg::Done);
                    }
                }
                DmabufDevice::Gbm(ref gbm) => {
                    if gbm_supported_modifiers(gbm, format).is_empty() {
                         
                        return Ok(ProcMsg::Done);
                    }
                }
            }
            check_space!(msg.len(), 0, remaining_space);
            copy_msg(msg, dst);
            Ok(ProcMsg::Done)
        }
        (WaylandInterface::ZwpLinuxDmabufV1, OPCODE_ZWP_LINUX_DMABUF_V1_MODIFIER) => {
            let (format, mod_hi, mod_lo) = parse_evt_zwp_linux_dmabuf_v1_modifier(msg)?;
            let modifier = join_u64(mod_hi, mod_lo);

            if glob.on_display_side {
                 
                if !dmabuf_dev_supports_format(&glob.dmabuf_device, format, modifier) {
                    return Ok(ProcMsg::Done);
                }
                check_space!(msg.len(), 0, remaining_space);
                add_advertised_modifiers(&mut glob.advertised_modifiers, format, &[modifier]);
                copy_msg(msg, dst);
                Ok(ProcMsg::Done)
            } else {
                 
                let WpExtra::ZwpDmabuf(d) = &mut obj.extra else {
                    panic!();
                };
                if d.formats_seen.contains(&format) {
                    return Ok(ProcMsg::Done);
                }
                d.formats_seen.insert(format);

                let mods = dmabuf_dev_modifier_list(&glob.dmabuf_device, format);
                check_space!(
                    mods.len() * length_evt_zwp_linux_dmabuf_v1_modifier(),
                    0,
                    remaining_space
                );
                add_advertised_modifiers(&mut glob.advertised_modifiers, format, mods);

                for new_mod in mods {
                    let (nmod_hi, nmod_lo) = split_u64(*new_mod);
                    write_evt_zwp_linux_dmabuf_v1_modifier(
                        dst, object_id, format, nmod_hi, nmod_lo,
                    );
                }
                Ok(ProcMsg::Done)
            }
        }

        (WaylandInterface::ZwpLinuxDmabufV1, OPCODE_ZWP_LINUX_DMABUF_V1_CREATE_PARAMS) => {
            check_space!(msg.len(), 0, remaining_space);
            copy_msg(msg, dst);

            let params_id = parse_req_zwp_linux_dmabuf_v1_create_params(msg)?;

            insert_new_object(
                &mut glob.objects,
                params_id,
                WpObject {
                    obj_type: WaylandInterface::ZwpLinuxBufferParamsV1,
                    extra: WpExtra::ZwpDmabufParams(Box::new(ObjZwpLinuxDmabufParams {
                        planes: Vec::new(),
                        dmabuf: None,
                    })),
                },
            )?;

            Ok(ProcMsg::Done)
        }

        (WaylandInterface::ZwpLinuxBufferParamsV1, OPCODE_ZWP_LINUX_BUFFER_PARAMS_V1_ADD) => {
             

            let (plane_idx, offset, stride, modifier_hi, modifier_lo) =
                parse_req_zwp_linux_buffer_params_v1_add(msg)?;
            let modifier: u64 = join_u64(modifier_hi, modifier_lo);

            let WpExtra::ZwpDmabufParams(ref mut p) = &mut obj.extra else {
                return Err(tag!("Incorrect extra type"));  
            };

            match transl {
                TranslationInfo::FromChannel((_, _)) => {
                     
                }
                TranslationInfo::FromWayland((x, _)) => {
                    let fd = x.pop_front().ok_or_else(|| tag!("Missing fd"))?;

                    p.planes.push(AddDmabufPlane {
                        fd,
                        plane_idx,
                        offset,
                        stride,
                        modifier,
                    });
                }
            };

            Ok(ProcMsg::Done)
        }

        (
            WaylandInterface::ZwpLinuxBufferParamsV1,
            OPCODE_ZWP_LINUX_BUFFER_PARAMS_V1_CREATE_IMMED,
        ) => {
            let WpExtra::ZwpDmabufParams(ref mut params) = &mut obj.extra else {
                return Err(tag!("Incorrect extra type"));  
            };

            let (buffer_id, width, height, drm_format, flags) =
                parse_req_zwp_linux_buffer_params_v1_create_immed(msg)?;
            if width <= 0 || height <= 0 {
                return Err(tag!("DMABUF width or height should be positive"));
            }
            let (width, height) = (width as u32, height as u32);

            if flags != 0 {
                return Err(tag!("DMABUF flags not yet supported"));
            }

            let sfd: Rc<RefCell<ShadowFd>> = match transl {
                TranslationInfo::FromChannel((x, y)) => {
                    let sfd = &x.front().ok_or_else(|| tag!("Missing sfd"))?;
                    let mut b = sfd.borrow_mut();
                    let ShadowFdVariant::Dmabuf(ref mut data) = &mut b.data else {
                        return Err(tag!("Incorrect extra type"));
                    };

                    data.debug_wayland_id = buffer_id;

                    let estimated_msg_len = length_req_zwp_linux_buffer_params_v1_add()
                        * data.export_planes.len()
                        + length_req_zwp_linux_buffer_params_v1_create_immed();
                    check_space!(estimated_msg_len, data.export_planes.len(), remaining_space);

                    for plane in data.export_planes.iter() {
                        let (mod_hi, mod_lo) = split_u64(plane.modifier);
                        write_req_zwp_linux_buffer_params_v1_add(
                            dst,
                            object_id,
                            false,
                            plane.plane_idx,
                            plane.offset,
                            plane.stride,
                            mod_hi,
                            mod_lo,
                        );
                    }
                    copy_msg(msg, dst);

                    drop(b);

                    let sfd = x.pop_front().unwrap();
                    y.push_back(sfd.clone());
                    sfd
                }
                TranslationInfo::FromWayland((_, y)) => {
                     
                    let estimated_msg_len = length_req_zwp_linux_buffer_params_v1_add()
                     
                    + length_req_zwp_linux_buffer_params_v1_create_immed();
                    check_space!(estimated_msg_len, params.planes.len(), remaining_space);

                    let mut planes = Vec::new();
                    std::mem::swap(&mut params.planes, &mut planes);

                    let sfd = translate_dmabuf_fd(
                        width,
                        height,
                        drm_format,
                        planes,
                        &glob.opts,
                        &glob.dmabuf_device,
                        &mut glob.max_local_id,
                        &mut glob.map,
                        buffer_id,
                    )?;
                    y.push(sfd.clone());

                     
                    let mod_linear: u64 = 0;
                     
                    let (mod_hi, mod_lo) = split_u64(mod_linear);

                    let wayl_format = drm_to_wayland(drm_format);
                     
                    let bpp = get_shm_format_layout(wayl_format).unwrap().planes[0].bpt;

                    write_req_zwp_linux_buffer_params_v1_add(
                        dst,
                        object_id,
                        true,
                          0,
                          0,
                        bpp.get().checked_mul(width).unwrap(),  
                        mod_hi,
                        mod_lo,
                    );
                    write_req_zwp_linux_buffer_params_v1_create_immed(
                        dst,
                        object_id,
                        buffer_id,
                        width as i32,
                        height as i32,
                        drm_format,
                        flags,
                    );

                    sfd
                }
            };

            insert_new_object(
                &mut glob.objects,
                buffer_id,
                WpObject {
                    obj_type: WaylandInterface::WlBuffer,
                    extra: WpExtra::WlBuffer(Box::new(ObjWlBuffer {
                        sfd,
                        shm_info: None,
                        unique_id: glob.max_buffer_uid,
                    })),
                },
            )?;
            glob.max_buffer_uid += 1;

            Ok(ProcMsg::Done)
        }
        (WaylandInterface::ZwpLinuxBufferParamsV1, OPCODE_ZWP_LINUX_BUFFER_PARAMS_V1_CREATE) => {
            let WpExtra::ZwpDmabufParams(ref mut params) = &mut obj.extra else {
                return Err(tag!("Incorrect extra type"));  
            };
            if params.dmabuf.is_some() {
                return Err(tag!("Can only create dmabuf from params once"));
            }

            let (width, height, drm_format, flags) =
                parse_req_zwp_linux_buffer_params_v1_create(msg)?;
            if width <= 0 || height <= 0 {
                return Err(tag!("DMABUF width or height should be positive"));
            }
            let (width, height) = (width as u32, height as u32);

            let sfd: Rc<RefCell<ShadowFd>> = match transl {
                TranslationInfo::FromChannel((x, y)) => {
                    let sfd = &x.front().ok_or_else(|| tag!("Missing sfd"))?;
                    let mut b = sfd.borrow_mut();
                    let ShadowFdVariant::Dmabuf(ref mut data) = &mut b.data else {
                        return Err(tag!("Incorrect extra type"));
                    };
                    let estimated_msg_len = length_req_zwp_linux_buffer_params_v1_add()
                        * data.export_planes.len()
                        + length_req_zwp_linux_buffer_params_v1_create_immed();
                    check_space!(estimated_msg_len, data.export_planes.len(), remaining_space);

                    for plane in data.export_planes.iter() {
                        let (mod_hi, mod_lo) = split_u64(plane.modifier);
                        write_req_zwp_linux_buffer_params_v1_add(
                            dst,
                            object_id,
                            false,
                            plane.plane_idx,
                            plane.offset,
                            plane.stride,
                            mod_hi,
                            mod_lo,
                        );
                    }
                    copy_msg(msg, dst);

                    drop(b);

                    let sfd = x.pop_front().unwrap();
                    y.push_back(sfd.clone());
                    sfd
                }
                TranslationInfo::FromWayland((_, y)) => {
                     
                    let estimated_msg_len = length_req_zwp_linux_buffer_params_v1_add()
                     
                    + length_req_zwp_linux_buffer_params_v1_create_immed();
                    check_space!(estimated_msg_len, params.planes.len(), remaining_space);

                    let mut planes = Vec::new();
                    std::mem::swap(&mut params.planes, &mut planes);

                    let sfd = translate_dmabuf_fd(
                        width,
                        height,
                        drm_format,
                        planes,
                        &glob.opts,
                        &glob.dmabuf_device,
                        &mut glob.max_local_id,
                        &mut glob.map,
                        ObjId(0),
                    )?;
                    y.push(sfd.clone());

                     
                    let mod_linear: u64 = 0;
                     
                    let (mod_hi, mod_lo) = split_u64(mod_linear);

                    let wayl_format = drm_to_wayland(drm_format);
                     
                    let bpp = get_shm_format_layout(wayl_format).unwrap().planes[0].bpt;

                    write_req_zwp_linux_buffer_params_v1_add(
                        dst,
                        object_id,
                        true,
                          0,
                          0,
                        bpp.get().checked_mul(width).unwrap(),  
                        mod_hi,
                        mod_lo,
                    );
                    write_req_zwp_linux_buffer_params_v1_create(
                        dst,
                        object_id,
                        width as i32,
                        height as i32,
                        drm_format,
                        flags,
                    );

                    sfd
                }
            };
            params.dmabuf = Some(sfd);
            Ok(ProcMsg::Done)
        }
        (WaylandInterface::ZwpLinuxBufferParamsV1, OPCODE_ZWP_LINUX_BUFFER_PARAMS_V1_FAILED) => {
            check_space!(msg.len(), 0, remaining_space);
            copy_msg(msg, dst);

             
            let WpExtra::ZwpDmabufParams(ref mut params) = &mut obj.extra else {
                return Err(tag!("Incorrect extra type"));
            };
            params.dmabuf = None;
            Ok(ProcMsg::Done)
        }
        (WaylandInterface::ZwpLinuxBufferParamsV1, OPCODE_ZWP_LINUX_BUFFER_PARAMS_V1_CREATED) => {
            check_space!(msg.len(), 0, remaining_space);
            copy_msg(msg, dst);

            let obj_id = parse_evt_zwp_linux_buffer_params_v1_created(msg)?;
            let WpExtra::ZwpDmabufParams(ref mut params) = &mut obj.extra else {
                return Err(tag!("Incorrect extra type"));
            };

            let mut osfd = None;
            std::mem::swap(&mut params.dmabuf, &mut osfd);
            let Some(sfd) = osfd else {
                return Err(tag!(
                    "zwp_linux_buffer_params_v1::created event must follow ::create request and must occur at most once"
                ));
            };

            let mut b = sfd.borrow_mut();
            let ShadowFdVariant::Dmabuf(ref mut d) = b.data else {
                return Err(tag!("Incorrect shadowfd type"));
            };
            d.debug_wayland_id = obj_id;
            drop(b);

            insert_new_object(
                &mut glob.objects,
                obj_id,
                WpObject {
                    obj_type: WaylandInterface::WlBuffer,
                    extra: WpExtra::WlBuffer(Box::new(ObjWlBuffer {
                        sfd,
                        shm_info: None,
                        unique_id: glob.max_buffer_uid,
                    })),
                },
            )?;
            glob.max_buffer_uid += 1;

            Ok(ProcMsg::Done)
        }

        (WaylandInterface::ZwpLinuxDmabufV1, OPCODE_ZWP_LINUX_DMABUF_V1_GET_DEFAULT_FEEDBACK)
        | (WaylandInterface::ZwpLinuxDmabufV1, OPCODE_ZWP_LINUX_DMABUF_V1_GET_SURFACE_FEEDBACK) => {
            check_space!(msg.len(), 0, remaining_space);
            copy_msg(msg, dst);

            let feedback_id = if mod_opcode == OPCODE_ZWP_LINUX_DMABUF_V1_GET_DEFAULT_FEEDBACK {
                parse_req_zwp_linux_dmabuf_v1_get_default_feedback(msg)?
            } else {
                let (f, _surface) = parse_req_zwp_linux_dmabuf_v1_get_surface_feedback(msg)?;
                f
            };

            insert_new_object(
                &mut glob.objects,
                feedback_id,
                WpObject {
                    obj_type: WaylandInterface::ZwpLinuxDmabufFeedbackV1,
                    extra: WpExtra::ZwpDmabufFeedback(Box::new(ObjZwpLinuxDmabufFeedback {
                        input_format_table: None,
                        output_format_table: None,
                        main_device: None,
                        tranches: Vec::new(),
                        processed: false,
                        queued_format_table: None,
                        current: DmabufTranche {
                            flags: 0,
                            values: Vec::new(),
                            indices: Vec::new(),
                            device: 0,
                        },
                    })),
                },
            )?;

            Ok(ProcMsg::Done)
        }

        (
            WaylandInterface::ZwpLinuxDmabufFeedbackV1,
            OPCODE_ZWP_LINUX_DMABUF_FEEDBACK_V1_FORMAT_TABLE,
        ) => {
            let table_size = parse_evt_zwp_linux_dmabuf_feedback_v1_format_table(msg)?;
            let WpExtra::ZwpDmabufFeedback(ref mut f) = &mut obj.extra else {
                return Err(tag!("Expected object to have dmabuf_feedback data"));
            };

             
            let mut table_data = vec![0; table_size as usize];
            match transl {
                TranslationInfo::FromChannel((x, _)) => {
                    let sfd = x.front().ok_or_else(|| tag!("Missing sfd"))?;
                    if file_has_pending_apply_tasks(sfd)? {
                        let b = sfd.borrow();
                        return Ok(ProcMsg::WaitFor(b.remote_id));
                    }
                    let sfd = x.pop_front().unwrap();

                    let b = sfd.borrow_mut();
                    let ShadowFdVariant::File(ref f) = b.data else {
                        return Err(tag!("Received non-File ShadowFd for format table"));
                    };
                    if table_size as usize != f.buffer_size {
                        return Err(tag!(
                            "Wrong buffer size for format table: got {}, expected {}",
                            f.buffer_size,
                            table_size
                        ));
                    }

                    copy_from_mapping(&mut table_data, &f.core.as_ref().unwrap().mapping, 0);
                }
                TranslationInfo::FromWayland((x, _)) => {
                    let fd = x.pop_front().ok_or_else(|| tag!("Missing fd"))?;

                    let mapping = ExternalMapping::new(&fd, table_size as usize, true)?;
                    copy_from_mapping(&mut table_data, &mapping, 0);
                }
            };
            f.input_format_table = Some(parse_format_table(&table_data));
            Ok(ProcMsg::Done)
        }

        (
            WaylandInterface::ZwpLinuxDmabufFeedbackV1,
            OPCODE_ZWP_LINUX_DMABUF_FEEDBACK_V1_MAIN_DEVICE,
        ) => {
            let WpExtra::ZwpDmabufFeedback(ref mut feedback) = &mut obj.extra else {
                return Err(tag!("Unexpected object extra type"));
            };

            let dev = parse_evt_zwp_linux_dmabuf_feedback_v1_main_device(msg)?;
            let main_device = parse_dev_array(dev)
                .ok_or_else(|| tag!("Unexpected size for dev_t: {}", dev.len()))?;
            feedback.main_device = Some(main_device);

            if glob.on_display_side && matches!(glob.dmabuf_device, DmabufDevice::VulkanSetup(_)) {
                complete_dmabuf_setup(&glob.opts, Some(main_device), &mut glob.dmabuf_device)?;
            }

            Ok(ProcMsg::Done)
        }
        (
            WaylandInterface::ZwpLinuxDmabufFeedbackV1,
            OPCODE_ZWP_LINUX_DMABUF_FEEDBACK_V1_TRANCHE_FLAGS,
        ) => {
            let WpExtra::ZwpDmabufFeedback(ref mut feedback) = &mut obj.extra else {
                return Err(tag!("Unexpected object extra type"));
            };

            let flags = parse_evt_zwp_linux_dmabuf_feedback_v1_tranche_flags(msg)?;
            feedback.current.flags = flags;

            Ok(ProcMsg::Done)
        }
        (
            WaylandInterface::ZwpLinuxDmabufFeedbackV1,
            OPCODE_ZWP_LINUX_DMABUF_FEEDBACK_V1_TRANCHE_TARGET_DEVICE,
        ) => {
            let WpExtra::ZwpDmabufFeedback(ref mut feedback) = &mut obj.extra else {
                return Err(tag!("Unexpected object extra type"));
            };

            let dev = parse_evt_zwp_linux_dmabuf_feedback_v1_tranche_target_device(msg)?;
            feedback.current.device = parse_dev_array(dev)
                .ok_or_else(|| tag!("Unexpected size for dev_t: {}", dev.len()))?;

            Ok(ProcMsg::Done)
        }
        (
            WaylandInterface::ZwpLinuxDmabufFeedbackV1,
            OPCODE_ZWP_LINUX_DMABUF_FEEDBACK_V1_TRANCHE_FORMATS,
        ) => {
            let WpExtra::ZwpDmabufFeedback(ref mut feedback) = &mut obj.extra else {
                return Err(tag!("Unexpected object extra type"));
            };

            let fmts = parse_evt_zwp_linux_dmabuf_feedback_v1_tranche_formats(msg)?;
            if fmts.len() % 2 != 0 {
                return Err(tag!("Format array not of even length"));
            }
            if fmts.is_empty() {
                return Ok(ProcMsg::Done);
            }
            let Some(ref table) = feedback.input_format_table else {
                return Err(tag!(
                    "No format table provided before tranche_formats was received"
                ));
            };

            for chunk in fmts.chunks_exact(2) {
                let idx = u16::from_le_bytes(chunk.try_into().unwrap());

                let Some(pair) = table.get(idx as usize) else {
                    return Err(tag!(
                        "Tranche format index {} out of range for format table of length {}",
                        idx,
                        table.len()
                    ));
                };
                 
                if glob.on_display_side
                    && !dmabuf_dev_supports_format(&glob.dmabuf_device, pair.0, pair.1)
                {
                    continue;
                }
                feedback.current.values.push(*pair);
            }

            Ok(ProcMsg::Done)
        }

        (
            WaylandInterface::ZwpLinuxDmabufFeedbackV1,
            OPCODE_ZWP_LINUX_DMABUF_FEEDBACK_V1_TRANCHE_DONE,
        ) => {
            let WpExtra::ZwpDmabufFeedback(ref mut feedback) = &mut obj.extra else {
                return Err(tag!("Unexpected object extra type"));
            };
            feedback.tranches.push(DmabufTranche {
                flags: 0,
                values: Vec::new(),
                indices: Vec::new(),
                device: 0,
            });
            std::mem::swap(feedback.tranches.last_mut().unwrap(), &mut feedback.current);
            Ok(ProcMsg::Done)
        }
        (WaylandInterface::ZwpLinuxDmabufFeedbackV1, OPCODE_ZWP_LINUX_DMABUF_FEEDBACK_V1_DONE) => {
            let WpExtra::ZwpDmabufFeedback(ref mut feedback) = &mut obj.extra else {
                return Err(tag!("Unexpected object extra type"));
            };

            let dev_len = write_dev_array(0).len();

            if !feedback.processed {
                 
                feedback.processed = true;

                if !glob.on_display_side {
                    rebuild_format_table(&glob.dmabuf_device, feedback)?;
                }
                let new_table = process_dmabuf_feedback(feedback)?;
                if Some(&new_table) != feedback.output_format_table.as_ref() {
                     
                    let local_fd = crate::util::create_anon_file()
                    .map_err(|x| tag!("Failed to create memfd: {:?}", x))?;
                    let sz: u32 = new_table.len().try_into().unwrap();
                    assert!(sz > 0);

                    unistd::ftruncate(&local_fd, sz as libc::off_t)
                        .map_err(|x| tag!("Failed to resize memfd: {:?}", x))?;
                    let mapping: ExternalMapping =
                        ExternalMapping::new(&local_fd, sz as usize, false).map_err(|x| {
                            tag!("Failed to mmap fd when building new format table: {}", x)
                        })?;
                    copy_onto_mapping(&new_table[..], &mapping, 0);

                    let sfd = translate_shm_fd(
                        local_fd,
                        sz as usize,
                        &mut glob.map,
                        &mut glob.max_local_id,
                        glob.on_display_side,
                        true,
                        !glob.on_display_side,
                    )?;

                    feedback.output_format_table = Some(new_table);
                    feedback.queued_format_table = Some((sfd, sz));
                }
            }

            let mut space_est = 0;
            if feedback.queued_format_table.is_some() {
                space_est += length_evt_zwp_linux_dmabuf_feedback_v1_format_table();
            }
            space_est += length_evt_zwp_linux_dmabuf_feedback_v1_main_device(dev_len);
            space_est += length_evt_zwp_linux_dmabuf_feedback_v1_done();

             
            for t in feedback.tranches.iter() {
                space_est += length_evt_zwp_linux_dmabuf_feedback_v1_tranche_done()
                    + length_evt_zwp_linux_dmabuf_feedback_v1_tranche_flags()
                    + length_evt_zwp_linux_dmabuf_feedback_v1_tranche_target_device(dev_len)
                    + length_evt_zwp_linux_dmabuf_feedback_v1_tranche_formats(t.indices.len());
            }

            check_space!(
                space_est,
                if feedback.queued_format_table.is_some() {
                    1
                } else {
                    0
                },
                remaining_space
            );

            let mut queued_table = None;
            std::mem::swap(&mut queued_table, &mut feedback.queued_format_table);
            if let Some((sfd, sz)) = queued_table {
                match transl {
                    TranslationInfo::FromChannel((_, y)) => {
                        write_evt_zwp_linux_dmabuf_feedback_v1_format_table(
                            dst, object_id, false, sz,
                        );
                        y.push_back(sfd);
                    }
                    TranslationInfo::FromWayland((_, y)) => {
                        write_evt_zwp_linux_dmabuf_feedback_v1_format_table(
                            dst, object_id, true, sz,
                        );
                        y.push(sfd);
                    }
                }
            }

             
            let dev_id = dmabuf_dev_get_id(&glob.dmabuf_device);
            write_evt_zwp_linux_dmabuf_feedback_v1_main_device(
                dst,
                object_id,
                write_dev_array(dev_id).as_slice(),
            );
            for t in feedback.tranches.iter() {
                for f in t.values.iter() {
                    add_advertised_modifiers(&mut glob.advertised_modifiers, f.0, &[f.1]);
                }
                write_evt_zwp_linux_dmabuf_feedback_v1_tranche_target_device(
                    dst,
                    object_id,
                    write_dev_array(dev_id).as_slice(),
                );
                write_evt_zwp_linux_dmabuf_feedback_v1_tranche_flags(dst, object_id, t.flags);
                write_evt_zwp_linux_dmabuf_feedback_v1_tranche_formats(
                    dst,
                    object_id,
                    &t.indices[..],
                );
                write_evt_zwp_linux_dmabuf_feedback_v1_tranche_done(dst, object_id);
            }
            write_evt_zwp_linux_dmabuf_feedback_v1_done(dst, object_id);

             
            feedback.processed = false;
            feedback.tranches = Vec::new();
             
            feedback.current = DmabufTranche {
                flags: 0,
                values: Vec::new(),
                indices: Vec::new(),
                device: 0,
            };

            Ok(ProcMsg::Done)
        }

        (WaylandInterface::WlKeyboard, OPCODE_WL_KEYBOARD_KEYMAP) => {
            check_space!(msg.len(), 1, remaining_space);

            let (_, keymap_size) = parse_evt_wl_keyboard_keymap(msg)?;
            if let Some(wait) = translate_or_wait_for_fixed_file(transl, glob, keymap_size)? {
                return Ok(wait);
            }
            copy_msg_tag_fd(msg, dst, from_channel)?;

            Ok(ProcMsg::Done)
        }
        (
            WaylandInterface::WpImageDescriptionInfoV1,
            OPCODE_WP_IMAGE_DESCRIPTION_INFO_V1_ICC_FILE,
        ) => {
            check_space!(msg.len(), 1, remaining_space);

            let file_size = parse_evt_wp_image_description_info_v1_icc_file(msg)?;
            if let Some(wait) = translate_or_wait_for_fixed_file(transl, glob, file_size)? {
                return Ok(wait);
            }
            copy_msg_tag_fd(msg, dst, from_channel)?;
            Ok(ProcMsg::Done)
        }
        (
            WaylandInterface::WpImageDescriptionCreatorIccV1,
            OPCODE_WP_IMAGE_DESCRIPTION_CREATOR_ICC_V1_SET_ICC_FILE,
        ) => {
            check_space!(msg.len(), 1, remaining_space);

             
             
             
             
             
            let (offset, length) = parse_req_wp_image_description_creator_icc_v1_set_icc_file(msg)?;
            if length == 0 {
                return Err(tag!("File length for wp_image_description_creator_icc_v1::set_icc_file should not be zero"));
            }
            let Some(file_sz) = offset.checked_add(length) else {
                return Err(tag!("File offset+length={}+{} overflow for wp_image_description_creator_icc_v1::set_icc_file", offset, length));
            };

            match transl {
                TranslationInfo::FromChannel((x, y)) => {
                    let sfd = &x.front().ok_or_else(|| tag!("Missing fd"))?;
                    let rid = sfd.borrow().remote_id;
                    if file_has_pending_apply_tasks(sfd)? {
                        return Ok(ProcMsg::WaitFor(rid));
                    }
                    y.push_back(x.pop_front().unwrap());
                }
                TranslationInfo::FromWayland((x, y)) => {
                    let v = translate_shm_fd(
                        x.pop_front().ok_or_else(|| tag!("Missing fd"))?,
                        file_sz.try_into().unwrap(),
                        &mut glob.map,
                        &mut glob.max_local_id,
                        true,
                        true,
                        false,
                    )?;
                    y.push(v);
                }
            };

            copy_msg_tag_fd(msg, dst, from_channel)?;
            Ok(ProcMsg::Done)
        }
        (
            WaylandInterface::ZwlrGammaControlManagerV1,
            OPCODE_ZWLR_GAMMA_CONTROL_MANAGER_V1_GET_GAMMA_CONTROL,
        ) => {
            check_space!(msg.len(), 0, remaining_space);

            let (gamma, _output) = parse_req_zwlr_gamma_control_manager_v1_get_gamma_control(msg)?;
            insert_new_object(
                &mut glob.objects,
                gamma,
                WpObject {
                    obj_type: WaylandInterface::ZwlrGammaControlV1,
                    extra: WpExtra::ZwlrGammaControl(Box::new(ObjZwlrGammaControl {
                        gamma_size: None,
                    })),
                },
            )?;

            copy_msg(msg, dst);
            Ok(ProcMsg::Done)
        }
        (WaylandInterface::ZwlrGammaControlV1, OPCODE_ZWLR_GAMMA_CONTROL_V1_GAMMA_SIZE) => {
            check_space!(msg.len(), 0, remaining_space);
            let WpExtra::ZwlrGammaControl(ref mut gamma) = obj.extra else {
                unreachable!();
            };
            let gamma_size = parse_evt_zwlr_gamma_control_v1_gamma_size(msg)?;
            if gamma_size > u32::MAX / 6 {
                return Err(tag!(
                    "Gamma size too large (ramps would use >u32::MAX bytes)"
                ));
            }
            gamma.gamma_size = Some(gamma_size);

            copy_msg(msg, dst);
            Ok(ProcMsg::Done)
        }
        (WaylandInterface::ZwlrGammaControlV1, OPCODE_ZWLR_GAMMA_CONTROL_V1_SET_GAMMA) => {
            check_space!(msg.len(), 1, remaining_space);
            let WpExtra::ZwlrGammaControl(ref gamma) = obj.extra else {
                unreachable!();
            };
            let Some(gamma_size) = gamma.gamma_size else {
                return Err(tag!(
                    "zwlr_gamma_control_v1::set_gamma called before gamma size provided"
                ));
            };

            if let Some(wait) =
                translate_or_wait_for_fixed_file(transl, glob, gamma_size.checked_mul(6).unwrap())?
            {
                return Ok(wait);
            }
            copy_msg_tag_fd(msg, dst, from_channel)?;

            Ok(ProcMsg::Done)
        }
        (
            WaylandInterface::ZwpPrimarySelectionSourceV1,
            OPCODE_ZWP_PRIMARY_SELECTION_SOURCE_V1_SEND,
        )
        | (
            WaylandInterface::ZwpPrimarySelectionOfferV1,
            OPCODE_ZWP_PRIMARY_SELECTION_OFFER_V1_RECEIVE,
        )
        | (WaylandInterface::GtkPrimarySelectionSource, OPCODE_GTK_PRIMARY_SELECTION_SOURCE_SEND)
        | (
            WaylandInterface::GtkPrimarySelectionOffer,
            OPCODE_GTK_PRIMARY_SELECTION_OFFER_RECEIVE,
        )
        | (WaylandInterface::ExtDataControlSourceV1, OPCODE_EXT_DATA_CONTROL_SOURCE_V1_SEND)
        | (WaylandInterface::ExtDataControlOfferV1, OPCODE_EXT_DATA_CONTROL_OFFER_V1_RECEIVE)
        | (WaylandInterface::ZwlrDataControlSourceV1, OPCODE_ZWLR_DATA_CONTROL_SOURCE_V1_SEND)
        | (WaylandInterface::ZwlrDataControlOfferV1, OPCODE_ZWLR_DATA_CONTROL_OFFER_V1_RECEIVE)
        | (WaylandInterface::WlDataSource, OPCODE_WL_DATA_SOURCE_SEND)
        | (WaylandInterface::WlDataOffer, OPCODE_WL_DATA_OFFER_RECEIVE) => {
            check_space!(msg.len(), 1, remaining_space);

             

            match transl {
                TranslationInfo::FromChannel((x, y)) => {
                    let sfd = x.pop_front().ok_or_else(|| tag!("Missing sfd"))?;
                    y.push_back(sfd);
                }
                TranslationInfo::FromWayland((x, y)) => {
                    let v = translate_pipe_fd(
                        x.pop_front().ok_or_else(|| tag!("Missing fd"))?,
                        glob,
                        true,  
                    )?;
                    y.push(v);
                }
            };

            copy_msg_tag_fd(msg, dst, from_channel)?;

            Ok(ProcMsg::Done)
        }
        (
            WaylandInterface::ZwlrScreencopyManagerV1,
            OPCODE_ZWLR_SCREENCOPY_MANAGER_V1_CAPTURE_OUTPUT_REGION,
        ) => {
            check_space!(msg.len(), 0, remaining_space);
            let (frame, _overlay_cursor, _output, _x, _y, _w, _h) =
                parse_req_zwlr_screencopy_manager_v1_capture_output_region(msg)?;
            insert_new_object(
                &mut glob.objects,
                frame,
                WpObject {
                    obj_type: WaylandInterface::ZwlrScreencopyFrameV1,
                    extra: WpExtra::ZwlrScreencopyFrame(Box::new(ObjZwlrScreencopyFrame {
                        buffer: None,
                    })),
                },
            )?;
            copy_msg(msg, dst);
            Ok(ProcMsg::Done)
        }
        (
            WaylandInterface::ZwlrScreencopyManagerV1,
            OPCODE_ZWLR_SCREENCOPY_MANAGER_V1_CAPTURE_OUTPUT,
        ) => {
            check_space!(msg.len(), 0, remaining_space);
            let (frame, _overlay_cursor, _output) =
                parse_req_zwlr_screencopy_manager_v1_capture_output(msg)?;
            insert_new_object(
                &mut glob.objects,
                frame,
                WpObject {
                    obj_type: WaylandInterface::ZwlrScreencopyFrameV1,
                    extra: WpExtra::ZwlrScreencopyFrame(Box::new(ObjZwlrScreencopyFrame {
                        buffer: None,
                    })),
                },
            )?;
            copy_msg(msg, dst);
            Ok(ProcMsg::Done)
        }
        (
            WaylandInterface::ExtImageCopyCaptureManagerV1,
            OPCODE_EXT_IMAGE_COPY_CAPTURE_MANAGER_V1_CREATE_SESSION,
        ) => {
            check_space!(msg.len(), 0, remaining_space);
            let (frame, _output, _options) =
                parse_req_ext_image_copy_capture_manager_v1_create_session(msg)?;
            insert_new_object(
                &mut glob.objects,
                frame,
                WpObject {
                    obj_type: WaylandInterface::ExtImageCopyCaptureSessionV1,
                    extra: WpExtra::ExtImageCopyCaptureSession(Box::new(
                        ObjExtImageCopyCaptureSession {
                            dmabuf_device: None,
                            dmabuf_formats: Vec::new(),
                            last_format_mod_list: Vec::new(),
                            frame_list: Vec::new(),
                        },
                    )),
                },
            )?;
            copy_msg(msg, dst);
            Ok(ProcMsg::Done)
        }
        (
            WaylandInterface::ExtImageCopyCaptureCursorSessionV1,
            OPCODE_EXT_IMAGE_COPY_CAPTURE_CURSOR_SESSION_V1_GET_CAPTURE_SESSION,
        ) => {
            check_space!(msg.len(), 0, remaining_space);
            let frame =
                parse_req_ext_image_copy_capture_cursor_session_v1_get_capture_session(msg)?;
            insert_new_object(
                &mut glob.objects,
                frame,
                WpObject {
                    obj_type: WaylandInterface::ExtImageCopyCaptureSessionV1,
                    extra: WpExtra::ExtImageCopyCaptureSession(Box::new(
                        ObjExtImageCopyCaptureSession {
                            dmabuf_device: None,
                            dmabuf_formats: Vec::new(),
                            last_format_mod_list: Vec::new(),
                            frame_list: Vec::new(),
                        },
                    )),
                },
            )?;
            copy_msg(msg, dst);
            Ok(ProcMsg::Done)
        }
        (
            WaylandInterface::ExtImageCopyCaptureSessionV1,
            OPCODE_EXT_IMAGE_COPY_CAPTURE_SESSION_V1_DMABUF_DEVICE,
        ) => {
            check_space!(msg.len(), 0, remaining_space);
            if glob.opts.no_gpu {
                return Ok(ProcMsg::Done);
            }
            let WpExtra::ExtImageCopyCaptureSession(ref mut session) = obj.extra else {
                unreachable!();
            };

            let dev = parse_evt_ext_image_copy_capture_session_v1_dmabuf_device(msg)?;
            let main_device = parse_dev_array(dev)
                .ok_or_else(|| tag!("Unexpected size for dev_t: {}", dev.len()))?;
            session.dmabuf_device = Some(main_device);

             
            Ok(ProcMsg::Done)
        }
        (
            WaylandInterface::ExtImageCopyCaptureSessionV1,
            OPCODE_EXT_IMAGE_COPY_CAPTURE_SESSION_V1_DMABUF_FORMAT,
        ) => {
            check_space!(msg.len(), 0, remaining_space);
            if glob.opts.no_gpu {
                return Ok(ProcMsg::Done);
            }
            let WpExtra::ExtImageCopyCaptureSession(ref mut session) = obj.extra else {
                unreachable!();
            };
            let (fmt, modifiers) = parse_evt_ext_image_copy_capture_session_v1_dmabuf_format(msg)?;
            let mut mod_list = Vec::new();
            for mb in modifiers.chunks_exact(std::mem::size_of::<u64>()) {
                let m = u64::from_le_bytes(mb.try_into().unwrap());
                mod_list.push(m);
            }
            if !mod_list.is_empty() {
                session.dmabuf_formats.push((fmt, mod_list));
            }
             
            Ok(ProcMsg::Done)
        }

        (
            WaylandInterface::ExtImageCopyCaptureSessionV1,
            OPCODE_EXT_IMAGE_COPY_CAPTURE_SESSION_V1_DONE,
        ) => {
             
            let WpExtra::ExtImageCopyCaptureSession(ref mut session) = obj.extra else {
                unreachable!();
            };

            let mut space_needed = length_evt_ext_image_copy_capture_session_v1_done();
            if let Some(main_device) = session.dmabuf_device {
                let dev: Option<u64> = if matches!(
                    glob.dmabuf_device,
                    DmabufDevice::Unknown | DmabufDevice::VulkanSetup(_)
                ) {
                     
                    if glob.on_display_side {
                        Some(main_device)
                    } else if let Some(node) = &glob.opts.drm_node {
                        Some(get_dev_for_drm_node_path(node)?)
                    } else {
                        None
                    }
                } else {
                    None
                };

                match glob.dmabuf_device {
                    DmabufDevice::Unknown => {
                        glob.dmabuf_device = try_setup_dmabuf_instance_full(&glob.opts, dev)?;
                    }
                    DmabufDevice::VulkanSetup(_) => {
                        complete_dmabuf_setup(&glob.opts, dev, &mut glob.dmabuf_device)?;
                    }
                    _ => (),
                }

                if matches!(glob.dmabuf_device, DmabufDevice::Unavailable) {
                    return Err(tag!(
                        "DMABUF device specified, but DMABUFs are not supported"
                    ));
                }
                let current_device_id = dmabuf_dev_get_id(&glob.dmabuf_device);
                if glob.on_display_side && main_device != current_device_id {
                     
                    return Err(tag!("image copy device did not match existing device; multiple devices are not yet supported"));
                }

                space_needed += length_evt_ext_image_copy_capture_session_v1_dmabuf_device(
                    write_dev_array(current_device_id).len(),
                );
                for (fmt, mod_list) in session.dmabuf_formats.iter() {
                    let new_list_len = if glob.on_display_side {
                        mod_list
                            .iter()
                            .filter(|m| dmabuf_dev_supports_format(&glob.dmabuf_device, *fmt, **m))
                            .count()
                    } else {
                        dmabuf_dev_modifier_list(&glob.dmabuf_device, *fmt).len()
                    };
                    if new_list_len == 0 {
                        continue;
                    }
                    space_needed += length_evt_ext_image_copy_capture_session_v1_dmabuf_format(
                        new_list_len * std::mem::size_of::<u64>(),
                    );
                }
            }

            check_space!(space_needed, 0, remaining_space);

            if session.dmabuf_device.is_some() {
                let current_device_id = dmabuf_dev_get_id(&glob.dmabuf_device);
                write_evt_ext_image_copy_capture_session_v1_dmabuf_device(
                    dst,
                    object_id,
                    write_dev_array(current_device_id).as_slice(),
                );

                for (fmt, mod_list) in session.dmabuf_formats.iter() {
                    let mut output = Vec::new();
                    if glob.on_display_side {
                         
                        for m in mod_list.iter() {
                            if dmabuf_dev_supports_format(&glob.dmabuf_device, *fmt, *m) {
                                output.extend_from_slice(&u64::to_le_bytes(*m));
                                add_advertised_modifiers(
                                    &mut glob.advertised_modifiers,
                                    *fmt,
                                    &[*m],
                                );
                            }
                        }
                    } else {
                         
                        let local_mods = dmabuf_dev_modifier_list(&glob.dmabuf_device, *fmt);
                        add_advertised_modifiers(&mut glob.advertised_modifiers, *fmt, local_mods);
                        for m in local_mods {
                            output.extend_from_slice(&u64::to_le_bytes(*m));
                        }
                    }
                    if output.is_empty() {
                        continue;
                    }

                    write_evt_ext_image_copy_capture_session_v1_dmabuf_format(
                        dst, object_id, *fmt, &output,
                    );
                }

                let mut format_mod_list = Vec::new();
                for (fmt, mods) in session.dmabuf_formats.iter() {
                    for m in mods.iter() {
                        format_mod_list.push((*fmt, *m))
                    }
                }
                format_mod_list.sort_unstable();
                session.last_format_mod_list = format_mod_list;

                if glob.on_display_side {
                    for (fmt, mods) in session.dmabuf_formats.iter() {
                        glob.screencopy_restrictions.insert(*fmt, mods.clone());
                    }
                }
            } else {
                session.last_format_mod_list = Vec::new();
            }
            write_evt_ext_image_copy_capture_session_v1_done(dst, object_id);

             
            session.dmabuf_device = None;
            session.dmabuf_formats = Vec::new();
            Ok(ProcMsg::Done)
        }
        (
            WaylandInterface::ExtImageCopyCaptureSessionV1,
            OPCODE_EXT_IMAGE_COPY_CAPTURE_SESSION_V1_CREATE_FRAME,
        ) => {
            check_space!(msg.len(), 0, remaining_space);

            let WpExtra::ExtImageCopyCaptureSession(ref session) = obj.extra else {
                unreachable!();
            };
            let supported_modifiers = session.last_format_mod_list.clone();

            let frame = parse_req_ext_image_copy_capture_session_v1_create_frame(msg)?;
            insert_new_object(
                &mut glob.objects,
                frame,
                WpObject {
                    obj_type: WaylandInterface::ExtImageCopyCaptureFrameV1,
                    extra: WpExtra::ExtImageCopyCaptureFrame(Box::new(
                        ObjExtImageCopyCaptureFrame {
                            buffer: None,
                            supported_modifiers,
                            capture_session: Some(object_id),
                        },
                    )),
                },
            )?;
            copy_msg(msg, dst);
            Ok(ProcMsg::Done)
        }
        (
            WaylandInterface::ExtImageCopyCaptureSessionV1,
            OPCODE_EXT_IMAGE_COPY_CAPTURE_SESSION_V1_DESTROY,
        ) => {
            check_space!(msg.len(), 0, remaining_space);
            copy_msg(msg, dst);

            let WpExtra::ExtImageCopyCaptureSession(ref mut session) = obj.extra else {
                unreachable!();
            };
            let mut frames = Vec::new();
            std::mem::swap(&mut session.frame_list, &mut frames);
            let last_format_mod_list = session.last_format_mod_list.clone();
            for frame_id in frames {
                let object = glob.objects.get_mut(&frame_id).unwrap();
                let WpExtra::ExtImageCopyCaptureFrame(ref mut frame) = object.extra else {
                    unreachable!();
                };
                frame.capture_session = None;
                frame.supported_modifiers = last_format_mod_list.clone();
            }
            Ok(ProcMsg::Done)
        }

        (WaylandInterface::ZwlrScreencopyFrameV1, OPCODE_ZWLR_SCREENCOPY_FRAME_V1_COPY) => {
            check_space!(msg.len(), 0, remaining_space);
            copy_msg(msg, dst);

            let buffer = parse_req_zwlr_screencopy_frame_v1_copy(msg)?;
            if buffer.0 == 0 {
                return Err(tag!(
                    "zwlr_screencopy_frame_v1::copy requires non-null object"
                ));
            }
            let buf_obj = glob.objects.get(&buffer).ok_or_else(|| {
                tag!(
                    "Failed to lookup buffer (id {}) for zwlr_screencopy_frame_v1::copy",
                    buffer
                )
            })?;
            let WpExtra::WlBuffer(ref d) = buf_obj.extra else {
                return Err(tag!("Expected wl_buffer object"));
            };
            let buf_info = (d.sfd.clone(), d.shm_info);

            let object = glob.objects.get_mut(&object_id).unwrap();
            let WpExtra::ZwlrScreencopyFrame(ref mut frame) = object.extra else {
                unreachable!();
            };
            frame.buffer = Some(buf_info);
            Ok(ProcMsg::Done)
        }
        (
            WaylandInterface::ZwlrScreencopyFrameV1,
            OPCODE_ZWLR_SCREENCOPY_FRAME_V1_COPY_WITH_DAMAGE,
        ) => {
            check_space!(msg.len(), 0, remaining_space);
            copy_msg(msg, dst);

            let buffer = parse_req_zwlr_screencopy_frame_v1_copy_with_damage(msg)?;
            if buffer.0 == 0 {
                return Err(tag!(
                    "zwlr_screencopy_frame_v1::copy requires non-null object"
                ));
            }
            let buf_obj = glob.objects.get(&buffer).ok_or_else(|| {
                tag!(
                    "Failed to lookup buffer (id {}) for zwlr_screencopy_frame_v1::copy",
                    buffer
                )
            })?;
            let WpExtra::WlBuffer(ref d) = buf_obj.extra else {
                return Err(tag!("Expected wl_buffer object"));
            };
            let buf_info = (d.sfd.clone(), d.shm_info);

            let object = glob.objects.get_mut(&object_id).unwrap();
            let WpExtra::ZwlrScreencopyFrame(ref mut frame) = object.extra else {
                unreachable!();
            };
            frame.buffer = Some(buf_info);
            Ok(ProcMsg::Done)
        }

        (
            WaylandInterface::ExtImageCopyCaptureFrameV1,
            OPCODE_EXT_IMAGE_COPY_CAPTURE_FRAME_V1_DESTROY,
        ) => {
            check_space!(msg.len(), 0, remaining_space);
            copy_msg(msg, dst);

            let WpExtra::ExtImageCopyCaptureFrame(ref mut frame) = obj.extra else {
                unreachable!();
            };
            let mut session: Option<ObjId> = None;
            std::mem::swap(&mut frame.capture_session, &mut session);

            if let Some(session_id) = session {
                let object = glob.objects.get_mut(&session_id).unwrap();
                let WpExtra::ExtImageCopyCaptureSession(ref mut session) = object.extra else {
                    unreachable!();
                };
                if let Some(i) = session.frame_list.iter().position(|x| *x == object_id) {
                    session.frame_list.remove(i);
                }
            }
            Ok(ProcMsg::Done)
        }
        (
            WaylandInterface::ExtImageCopyCaptureFrameV1,
            OPCODE_EXT_IMAGE_COPY_CAPTURE_FRAME_V1_ATTACH_BUFFER,
        ) => {
            check_space!(msg.len(), 0, remaining_space);
            copy_msg(msg, dst);

            let buffer = parse_req_ext_image_copy_capture_frame_v1_attach_buffer(msg)?;
            if buffer.0 == 0 {
                return Err(tag!(
                    "ext_image_copy_capture_frame_v1::attach_buffer requires non-null object"
                ));
            }
            let buf_obj = glob.objects.get(&buffer).ok_or_else(|| {
                tag!(
                    "Failed to lookup buffer (id {}) for ext_image_copy_capture_frame_v1::attach_buffer",
                     buffer
                )
            })?;
            let WpExtra::WlBuffer(ref d) = buf_obj.extra else {
                return Err(tag!("Expected wl_buffer object"));
            };
            let buf_info = (d.sfd.clone(), d.shm_info);

            let object = glob.objects.get_mut(&object_id).unwrap();
            let WpExtra::ExtImageCopyCaptureFrame(ref mut frame) = object.extra else {
                unreachable!();
            };
            frame.buffer = Some(buf_info);
            Ok(ProcMsg::Done)
        }

        (
            WaylandInterface::ExtImageCopyCaptureFrameV1,
            OPCODE_EXT_IMAGE_COPY_CAPTURE_FRAME_V1_CAPTURE,
        ) => {
            check_space!(msg.len(), 0, remaining_space);
            copy_msg(msg, dst);

            if glob.on_display_side {
                let WpExtra::ExtImageCopyCaptureFrame(ref frame) = obj.extra else {
                    unreachable!();
                };
                 
                let fmtmod = if let Some(ref buffer) = frame.buffer {
                    let b = buffer.0.borrow();
                    if let ShadowFdVariant::Dmabuf(ref d) = b.data {
                        Some((d.drm_format, d.drm_modifier))
                    } else {
                        None
                    }
                } else {
                    None
                };

                if let Some(pair) = fmtmod {
                    let err = if let Some(session_id) = frame.capture_session {
                        let object = glob.objects.get_mut(&session_id).unwrap();
                        let WpExtra::ExtImageCopyCaptureSession(ref session) = object.extra else {
                            unreachable!();
                        };
                        session.last_format_mod_list.binary_search(&pair).is_err()
                    } else {
                        frame.supported_modifiers.binary_search(&pair).is_err()
                    };
                    if err {
                        error!("A wl_buffer Waypipe created with (format, modifier) = (0x{:08x},0x{:016x}) \
                            is being submitted to ext_image_copy_capture_frame_v1#{}, whose parent ext_image_copy_capture_session_v1's \
                            most recent update did not include the (format, modifier) combination. This may be a known Waypipe issue.",
                            pair.0, pair.1, object_id);
                    }
                }
            }
            Ok(ProcMsg::Done)
        }
        (WaylandInterface::ZwlrScreencopyFrameV1, OPCODE_ZWLR_SCREENCOPY_FRAME_V1_READY) => {
            check_space!(msg.len(), 0, remaining_space);
            let WpExtra::ZwlrScreencopyFrame(ref mut frame) = obj.extra else {
                unreachable!();
            };

            let Some((ref sfd, ref shm_info)) = frame.buffer else {
                return Err(tag!(
                    "zwlr_screencopy_frame_v1::ready is missing buffer information"
                ));
            };

            if !glob.on_display_side {
                let b = sfd.borrow();
                let apply_count = if let ShadowFdVariant::File(data) = &b.data {
                    data.pending_apply_tasks
                } else if let ShadowFdVariant::Dmabuf(data) = &b.data {
                     
                    data.pending_apply_tasks
                } else {
                    return Err(tag!("Attached buffer is not of file or dmabuf type"));
                };
                if apply_count > 0 {
                    return Ok(ProcMsg::WaitFor(b.remote_id));
                }
            }

            let (tv_sec_hi, tv_sec_lo, tv_nsec) = parse_evt_zwlr_screencopy_frame_v1_ready(msg)?;
            let (new_sec_hi, new_sec_lo, new_nsec) = translate_timestamp(
                tv_sec_hi,
                tv_sec_lo,
                tv_nsec,
                libc::CLOCK_MONOTONIC as u32,
                glob.on_display_side,
            )?;
            write_evt_zwlr_screencopy_frame_v1_ready(
                dst, object_id, new_sec_hi, new_sec_lo, new_nsec,
            );

            if !glob.on_display_side {
                let mut sfd = sfd.borrow_mut();
                if let ShadowFdVariant::Dmabuf(ref mut y) = &mut sfd.data {
                    dmabuf_post_apply_task_operations(y)?;
                }
            }

            if glob.on_display_side {
                 

                let mut sfd = sfd.borrow_mut();
                if let ShadowFdVariant::File(ref mut y) = &mut sfd.data {
                    let damage_interval = damage_for_entire_buffer(shm_info.as_ref().unwrap());
                    match &y.damage {
                        Damage::Everything => {}
                        Damage::Intervals(old) => {
                            let dmg = &[damage_interval];
                            y.damage = Damage::Intervals(union_damage(&old[..], &dmg[..], 128));
                        }
                    }
                } else if let ShadowFdVariant::Dmabuf(ref mut y) = &mut sfd.data {
                    y.damage = Damage::Everything;

                    y.using_implicit_sync = true;
                } else {
                    return Err(tag!("Expected buffer shadowfd to be of file type"));
                }
            }
            frame.buffer = None;
            Ok(ProcMsg::Done)
        }
        (
            WaylandInterface::ExtImageCopyCaptureFrameV1,
            OPCODE_EXT_IMAGE_COPY_CAPTURE_FRAME_V1_PRESENTATION_TIME,
        ) => {
            check_space!(msg.len(), 0, remaining_space);

            let (tv_sec_hi, tv_sec_lo, tv_nsec) =
                parse_evt_ext_image_copy_capture_frame_v1_presentation_time(msg)?;
            let (new_sec_hi, new_sec_lo, new_nsec) = translate_timestamp(
                tv_sec_hi,
                tv_sec_lo,
                tv_nsec,
                libc::CLOCK_MONOTONIC as u32,
                glob.on_display_side,
            )?;
            write_evt_ext_image_copy_capture_frame_v1_presentation_time(
                dst, object_id, new_sec_hi, new_sec_lo, new_nsec,
            );
            Ok(ProcMsg::Done)
        }

        (
            WaylandInterface::ExtImageCopyCaptureFrameV1,
            OPCODE_EXT_IMAGE_COPY_CAPTURE_FRAME_V1_READY,
        ) => {
             
            check_space!(msg.len(), 0, remaining_space);
            let WpExtra::ExtImageCopyCaptureFrame(ref mut frame) = obj.extra else {
                unreachable!();
            };

            let Some((ref sfd, ref shm_info)) = frame.buffer else {
                return Err(tag!(
                    "zwlr_screencopy_frame_v1::ready is missing buffer information"
                ));
            };

            if !glob.on_display_side {
                let b = sfd.borrow();
                let apply_count = if let ShadowFdVariant::File(data) = &b.data {
                    data.pending_apply_tasks
                } else if let ShadowFdVariant::Dmabuf(data) = &b.data {
                     
                    data.pending_apply_tasks
                } else {
                    return Err(tag!("Attached buffer is not of file or dmabuf type"));
                };
                if apply_count > 0 {
                    return Ok(ProcMsg::WaitFor(b.remote_id));
                }
            }

            copy_msg(msg, dst);

            if !glob.on_display_side {
                let mut sfd = sfd.borrow_mut();
                if let ShadowFdVariant::Dmabuf(ref mut y) = &mut sfd.data {
                    dmabuf_post_apply_task_operations(y)?;
                }
            }

            if glob.on_display_side {
                 

                let mut sfd = sfd.borrow_mut();
                if let ShadowFdVariant::File(ref mut y) = &mut sfd.data {
                    let damage_interval = damage_for_entire_buffer(shm_info.as_ref().unwrap());
                    match &y.damage {
                        Damage::Everything => {}
                        Damage::Intervals(old) => {
                            let dmg = &[damage_interval];
                            y.damage = Damage::Intervals(union_damage(&old[..], &dmg[..], 128));
                        }
                    }
                } else if let ShadowFdVariant::Dmabuf(ref mut y) = &mut sfd.data {
                    y.using_implicit_sync = true;
                    y.damage = Damage::Everything;
                } else {
                    return Err(tag!("Expected buffer shadowfd to be of file type"));
                }
            }
            frame.buffer = None;
            Ok(ProcMsg::Done)
        }
        (WaylandInterface::ZwlrScreencopyFrameV1, OPCODE_ZWLR_SCREENCOPY_FRAME_V1_FAILED) => {
            check_space!(msg.len(), 0, remaining_space);
            copy_msg(msg, dst);
            let WpExtra::ZwlrScreencopyFrame(ref mut frame) = obj.extra else {
                unreachable!();
            };
            frame.buffer = None;
            Ok(ProcMsg::Done)
        }
        (
            WaylandInterface::ExtImageCopyCaptureFrameV1,
            OPCODE_EXT_IMAGE_COPY_CAPTURE_FRAME_V1_FAILED,
        ) => {
            check_space!(msg.len(), 0, remaining_space);
            copy_msg(msg, dst);
            let WpExtra::ExtImageCopyCaptureFrame(ref mut frame) = obj.extra else {
                unreachable!();
            };
            frame.buffer = None;
            Ok(ProcMsg::Done)
        }
        (WaylandInterface::XdgToplevel, OPCODE_XDG_TOPLEVEL_SET_TITLE) => {
            let title = parse_req_xdg_toplevel_set_title(msg)?;
            let prefix = glob.opts.title_prefix.as_bytes();

            let space_needed = length_req_xdg_toplevel_set_title(title.len() + prefix.len());
            check_space!(space_needed, 0, remaining_space);

             
             
            let mut concat: Vec<u8> = Vec::new();
            concat.extend_from_slice(prefix);
            concat.extend_from_slice(title);
            write_req_xdg_toplevel_set_title(dst, object_id, &concat);
            Ok(ProcMsg::Done)
        }
        (WaylandInterface::XdgToplevelIconV1, OPCODE_XDG_TOPLEVEL_ICON_V1_ADD_BUFFER) => {
            check_space!(msg.len(), 0, remaining_space);

            let (buffer_id, _scale) = parse_req_xdg_toplevel_icon_v1_add_buffer(msg)?;

            let Some(buffer) = glob.objects.get(&buffer_id) else {
                return Err(tag!(
                    "Provided buffer is null, was never created, or is not tracked"
                ));
            };
            let WpExtra::WlBuffer(ref extra) = buffer.extra else {
                return Err(tag!("Expected wl_buffer object"));
            };

             
            if glob.on_display_side {
                let b = extra.sfd.borrow();
                 
                let apply_count = if let ShadowFdVariant::File(data) = &b.data {
                    data.pending_apply_tasks
                } else {
                    return Err(tag!("Attached buffer shadowfd is not of file type"));
                };
                if apply_count > 0 {
                    return Ok(ProcMsg::WaitFor(b.remote_id));
                }
            }

            copy_msg(msg, dst);

            if !glob.on_display_side {
                 
                let mut sfd = extra.sfd.borrow_mut();
                if let ShadowFdVariant::File(ref mut y) = &mut sfd.data {
                    let damage_interval =
                        damage_for_entire_buffer(extra.shm_info.as_ref().unwrap());
                    match &y.damage {
                        Damage::Everything => {}
                        Damage::Intervals(old) => {
                            let dmg = &[damage_interval];
                            y.damage = Damage::Intervals(union_damage(&old[..], &dmg[..], 128));
                        }
                    }
                } else {
                    return Err(tag!("Expected buffer shadowfd to be of file type"));
                }
            }
            Ok(ProcMsg::Done)
        }
        (WaylandInterface::WpPresentationFeedback, OPCODE_WP_PRESENTATION_FEEDBACK_PRESENTED) => {
            check_space!(msg.len(), 0, remaining_space);
            let (tv_sec_hi, tv_sec_lo, tv_nsec, refresh, seq_hi, seq_lo, flags) =
                parse_evt_wp_presentation_feedback_presented(msg)?;
            let clock_id = glob.presentation_clock.unwrap_or_else(|| {
                error!("wp_presentation_feedback::presented timestamp was received before any wp_presentation::clock event,\
                        so Waypipe assumes CLOCK_MONOTONIC was used and may misconvert times if wrong.");
                libc::CLOCK_MONOTONIC as u32
            }            );
            let (new_sec_hi, new_sec_lo, new_nsec) = translate_timestamp(
                tv_sec_hi,
                tv_sec_lo,
                tv_nsec,
                clock_id,
                glob.on_display_side,
            )?;
            write_evt_wp_presentation_feedback_presented(
                dst, object_id, new_sec_hi, new_sec_lo, new_nsec, refresh, seq_hi, seq_lo, flags,
            );
            Ok(ProcMsg::Done)
        }
        (WaylandInterface::WpPresentation, OPCODE_WP_PRESENTATION_CLOCK_ID) => {
            check_space!(msg.len(), 0, remaining_space);
            let clock_id = parse_evt_wp_presentation_clock_id(msg)?;
            if let Some(old) = glob.presentation_clock {
                if clock_id != old {
                    return Err(tag!(
                        "The wp_presentation clock was already set to {} and cannot be changed to {}.",
                        old,
                        clock_id
                    ));
                }
            }
             
             
             
            glob.presentation_clock = Some(clock_id);
            copy_msg(msg, dst);
            Ok(ProcMsg::Done)
        }
        (WaylandInterface::WpCommitTimerV1, OPCODE_WP_COMMIT_TIMER_V1_SET_TIMESTAMP) => {
            check_space!(msg.len(), 0, remaining_space);
            let (tv_sec_hi, tv_sec_lo, tv_nsec) = parse_req_wp_commit_timer_v1_set_timestamp(msg)?;

            let clock_id = glob.presentation_clock.unwrap_or_else(|| {
                error!("wp_commit_timer_v1::set_timestamp was received before wp_presentation::clock,\
                        so Waypipe assumes CLOCK_MONOTONIC was used and may misconvert times if wrong.");
                libc::CLOCK_MONOTONIC as u32
            }            );
            let (new_sec_hi, new_sec_lo, new_nsec) = translate_timestamp(
                tv_sec_hi,
                tv_sec_lo,
                tv_nsec,
                clock_id,
                !glob.on_display_side,
            )?;
            write_req_wp_commit_timer_v1_set_timestamp(
                dst, object_id, new_sec_hi, new_sec_lo, new_nsec,
            );
            Ok(ProcMsg::Done)
        }
        (WaylandInterface::WlRegistry, OPCODE_WL_REGISTRY_GLOBAL) => {
             
            let (name, intf, mut version) = parse_evt_wl_registry_global(msg)?;

             
            let blacklist: &'static [&'static [u8]] = &[
                b"wl_drm",  
                b"wp_drm_lease_device_v1",
                b"zwlr_export_dmabuf_manager_v1",
                b"zwp_linux_explicit_synchronization_v1",  
                b"wp_security_context_manager_v1",         
            ];

             
            let intf_code = match intf {
                WL_SHM => Some(WaylandInterface::WlShm),
                ZWP_LINUX_DMABUF_V1 => Some(WaylandInterface::ZwpLinuxDmabufV1),
                WP_LINUX_DRM_SYNCOBJ_MANAGER_V1 => {
                    Some(WaylandInterface::WpLinuxDrmSyncobjManagerV1)
                }
                EXT_IMAGE_COPY_CAPTURE_MANAGER_V1 => {
                    Some(WaylandInterface::ExtImageCopyCaptureManagerV1)
                }
                ZWLR_SCREENCOPY_MANAGER_V1 => Some(WaylandInterface::ZwlrScreencopyManagerV1),
                ZWLR_EXPORT_DMABUF_MANAGER_V1 => Some(WaylandInterface::ZwlrExportDmabufManagerV1),
                _ => None,
            };
            if let Some(code) = intf_code {
                let max_v = INTERFACE_TABLE[code as usize].version;
                if version > max_v {
                    debug!(
                        "Downgrading {} version from {} to {}",
                        EscapeWlName(intf),
                        version,
                        max_v
                    );
                    version = max_v;
                }
            }
            if blacklist.contains(&intf) {
                 
                debug!("Dropping interface: {}", EscapeWlName(intf));
                return Ok(ProcMsg::Done);
            }

            if intf == ZWP_LINUX_DMABUF_V1 {
                 
                match glob.dmabuf_device {
                    DmabufDevice::Unavailable => (),  
                    DmabufDevice::Vulkan(_) | DmabufDevice::Gbm(_) => (),
                    DmabufDevice::VulkanSetup(_) => (),
                    DmabufDevice::Unknown => {
                        if !glob.on_display_side {
                            let dev = if let Some(node) = &glob.opts.drm_node {
                                 
                                Some(get_dev_for_drm_node_path(node)?)
                            } else {
                                 
                                None
                            };
                            glob.dmabuf_device = try_setup_dmabuf_instance_light(&glob.opts, dev)?;
                            assert!(!matches!(glob.dmabuf_device, DmabufDevice::Unknown));
                        }
                    }
                }
                if matches!(glob.dmabuf_device, DmabufDevice::Unavailable) {
                    debug!(
                        "No DMABUF handling device available: Dropping interface: {}",
                        EscapeWlName(intf)
                    );
                    return Ok(ProcMsg::Done);
                }

                 
            }
            if intf == WP_LINUX_DRM_SYNCOBJ_MANAGER_V1 {
                match &glob.dmabuf_device {
                    DmabufDevice::Unknown => {
                         
                        let WpExtra::WlRegistry(ref mut reg) = obj.extra else {
                            return Err(tag!("Unexpected extra type for wl_registry"));
                        };
                        reg.syncobj_manager_replay.push((name, version));
                    }
                    DmabufDevice::Gbm(_) | DmabufDevice::Unavailable => {
                         
                        debug!(
                            "No timeline semaphore handling device available: Dropping interface: {}",
                            EscapeWlName(intf)
                        );
                        return Ok(ProcMsg::Done);
                    }
                    DmabufDevice::VulkanSetup(vkinst) => {
                        let dev = if let Some(node) = &glob.opts.drm_node {
                            Some(get_dev_for_drm_node_path(node)?)
                        } else {
                            None
                        };
                        if !vkinst.device_supports_timeline_import_export(dev) {
                            debug!(
                                "Timeline semaphore import/export will not be supported: Dropping interface: {}",
                                EscapeWlName(intf)
                            );
                            return Ok(ProcMsg::Done);
                        }
                    }
                    DmabufDevice::Vulkan((_, vulk)) => {
                         
                        if !vulk.supports_timeline_import_export() {
                            debug!(
                                "Timeline semaphore import/export is not supported: Dropping interface: {}",
                                EscapeWlName(intf)
                            );
                            return Ok(ProcMsg::Done);
                        }
                    }
                }
            }

            let mut space = msg.len();
            if intf == ZWP_LINUX_DMABUF_V1 {
                let WpExtra::WlRegistry(ref mut reg) = obj.extra else {
                    return Err(tag!("Unexpected extra type for wl_registry"));
                };
                if !reg.syncobj_manager_replay.is_empty() {
                    space += length_evt_wl_registry_global(WP_LINUX_DRM_SYNCOBJ_MANAGER_V1.len())
                        * reg.syncobj_manager_replay.len();
                }
            }

            check_space!(space, 0, remaining_space);
            write_evt_wl_registry_global(dst, object_id, name, intf, version);

            if intf == ZWP_LINUX_DMABUF_V1 {
                 
                let WpExtra::WlRegistry(ref mut reg) = obj.extra else {
                    return Err(tag!("Unexpected extra type for wl_registry"));
                };
                let timelines_supported = match &glob.dmabuf_device {
                     
                    DmabufDevice::VulkanSetup(_) => true,
                    DmabufDevice::Unknown => {
                        if glob.on_display_side {
                            true
                        } else {
                            unreachable!();
                        }
                    }
                    DmabufDevice::Unavailable => unreachable!(),
                    DmabufDevice::Gbm(_) => false,
                    DmabufDevice::Vulkan((_, vulk)) => vulk.supports_timeline_import_export(),
                };
                if !timelines_supported && !reg.syncobj_manager_replay.is_empty() {
                    debug!(
                        "Timeline semaphore import/export is not supported, not replaying {} advertisements for {}",
                        reg.syncobj_manager_replay.len(),
                        EscapeWlName(WP_LINUX_DRM_SYNCOBJ_MANAGER_V1)
                    );
                }

                for (sync_name, sync_version) in reg.syncobj_manager_replay.drain(..) {
                    if timelines_supported {
                        write_evt_wl_registry_global(
                            dst,
                            object_id,
                            sync_name,
                            WP_LINUX_DRM_SYNCOBJ_MANAGER_V1,
                            sync_version,
                        );
                    }
                }
            }

            Ok(ProcMsg::Done)
        }
        (WaylandInterface::WlRegistry, OPCODE_WL_REGISTRY_BIND) => {
             
            let (_id, name, version, oid) = parse_req_wl_registry_bind(msg)?;
            if name == ZWP_LINUX_DMABUF_V1 {
                let light_setup = version >= 4 && glob.on_display_side;
                if matches!(glob.dmabuf_device, DmabufDevice::Unknown) {
                    let dev = if let Some(node) = &glob.opts.drm_node {
                         
                        Some(get_dev_for_drm_node_path(node)?)
                    } else {
                         
                        None
                    };
                    if light_setup {
                         
                        glob.dmabuf_device = try_setup_dmabuf_instance_light(&glob.opts, dev)?;
                    } else {
                        debug!(
                            "Client bound zwp_linux_dmabuf_v1 at version {} older than 4, using best-or-specified drm node",
                            version
                        );
                        glob.dmabuf_device = try_setup_dmabuf_instance_full(&glob.opts, dev)?;
                    }
                    assert!(!matches!(glob.dmabuf_device, DmabufDevice::Unknown));
                }
                if !light_setup && matches!(glob.dmabuf_device, DmabufDevice::VulkanSetup(_)) {
                    let dev = if let Some(node) = &glob.opts.drm_node {
                        Some(get_dev_for_drm_node_path(node)?)
                    } else {
                        None
                    };
                    complete_dmabuf_setup(&glob.opts, dev, &mut glob.dmabuf_device)?;
                }
                if matches!(glob.dmabuf_device, DmabufDevice::Unavailable) {
                    return Err(tag!("Failed to set up a device to handle DMABUFS"));
                }

                check_space!(msg.len(), 0, remaining_space);
                copy_msg(msg, dst);
                insert_new_object(
                    &mut glob.objects,
                    oid,
                    WpObject {
                        obj_type: WaylandInterface::ZwpLinuxDmabufV1,
                        extra: WpExtra::ZwpDmabuf(Box::new(ObjZwpLinuxDmabuf {
                            formats_seen: BTreeSet::new(),
                        })),
                    },
                )?;
                return Ok(ProcMsg::Done);
            }

             
            default_proc_way_msg(msg, dst, meth, is_req, object_id, glob)
        }

        _ => {
             
            default_proc_way_msg(msg, dst, meth, is_req, object_id, glob)
        }
    }
}

 
pub fn log_way_msg_output(
    orig_msg: &[u8],
    mut output_msgs: &[u8],
    objects: &BTreeMap<ObjId, WpObject>,
    is_req: bool,
) {
    if !log::log_enabled!(log::Level::Debug) {
        return;
    }

    if output_msgs.is_empty() {
        debug!("Dropped last {}", if is_req { "request" } else { "event" },);
        return;
    }
    if orig_msg[0..4] == output_msgs[0..4] && orig_msg[8..] == output_msgs[8..] {
         
        return;
    }

     
    while !output_msgs.is_empty() {
        let object_id = ObjId(u32::from_le_bytes(output_msgs[0..4].try_into().unwrap()));
        let header2 = u32::from_le_bytes(output_msgs[4..8].try_into().unwrap());
        let length = (header2 >> 16) as usize;
        let opcode = (header2 & ((1 << 11) - 1)) as usize;
        let msg = &output_msgs[..length];
        output_msgs = &output_msgs[length..];

        let Some(obj) = objects.get(&object_id) else {
             
            continue;
        };

        let opt_meth: Option<&WaylandMethod> = if is_req {
            INTERFACE_TABLE[obj.obj_type as usize].reqs.get(opcode)
        } else {
            INTERFACE_TABLE[obj.obj_type as usize].evts.get(opcode)
        };
        let Some(meth) = opt_meth else {
             
            continue;
        };
        debug!(
            "Modified {}: {}#{}.{}({})",
            if is_req { "request" } else { "event" },
            INTERFACE_TABLE[obj.obj_type as usize].name,
            object_id,
            meth.name,
            MethodArguments { meth, msg }
        );
    }
}

 
pub fn setup_object_map() -> BTreeMap<ObjId, WpObject> {
    let mut map = BTreeMap::new();
    map.insert(
        ObjId(1),
        WpObject {
            obj_type: WaylandInterface::WlDisplay,
            extra: WpExtra::None,
        },
    );
    map
}
