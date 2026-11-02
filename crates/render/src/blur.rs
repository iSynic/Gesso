//! The reference implementation of offscreen effects: separable Gaussian blur (with octave
//! downsampling for large σ), alpha dilation and erosion, and drop/inner shadow composition.
//! The GPU backend's shaders follow these steps exactly, so the two agree within rounding.
//!
//! Pixels are premultiplied RGBA8. Outside a surface everything is transparent.

use rayon::prelude::*;

/// Blur σ above which the image is halved first (and σ with it).
pub const OCTAVE_SIGMA: f64 = 8.0;
/// Kernel radius in σ.
pub const KERNEL_SIGMAS: f64 = 3.0;

/// Premultiplied RGBA8 pixels covering a device-space integer rectangle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pixels {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
    pub data: Vec<u8>,
}

impl Pixels {
    pub fn new(x: i32, y: i32, w: u32, h: u32) -> Self {
        Pixels { x, y, w, h, data: vec![0; w as usize * h as usize * 4] }
    }

    fn plane(&self, c: usize) -> Vec<f32> {
        self.data.as_chunks::<4>().0.iter().map(|p| p[c] as f32 / 255.0).collect()
    }

    fn set_plane(&mut self, c: usize, v: &[f32]) {
        for (p, x) in self.data.as_chunks_mut::<4>().0.iter_mut().zip(v) {
            p[c] = (x.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
        }
    }
}

/// Normalised Gaussian weights for offsets `-r..=r`.
pub fn kernel(sigma: f64) -> Vec<f32> {
    let r = (KERNEL_SIGMAS * sigma).ceil().max(1.0) as i32;
    let k: Vec<f64> = (-r..=r).map(|i| (-(i * i) as f64 / (2.0 * sigma * sigma)).exp()).collect();
    let sum: f64 = k.iter().sum();
    k.iter().map(|v| (v / sum) as f32).collect()
}

fn convolve(src: &[f32], w: usize, h: usize, k: &[f32], horizontal: bool) -> Vec<f32> {
    let r = (k.len() / 2) as isize;
    let mut out = vec![0.0f32; src.len()];
    out.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        for (x, o) in row.iter_mut().enumerate() {
            let mut acc = 0.0f32;
            for (i, kv) in k.iter().enumerate() {
                let d = i as isize - r;
                let (sx, sy) = if horizontal { (x as isize + d, y as isize) } else { (x as isize, y as isize + d) };
                if sx >= 0 && sy >= 0 && (sx as usize) < w && (sy as usize) < h {
                    acc += kv * src[sy as usize * w + sx as usize];
                }
            }
            *o = acc;
        }
    });
    out
}

/// Halves a plane (2×2 box average; missing pixels count as 0).
fn downsample(src: &[f32], w: usize, h: usize) -> (Vec<f32>, usize, usize) {
    let (nw, nh) = (w.div_ceil(2), h.div_ceil(2));
    let at = |x: usize, y: usize| if x < w && y < h { src[y * w + x] } else { 0.0 };
    let mut out = vec![0.0; nw * nh];
    for y in 0..nh {
        for x in 0..nw {
            out[y * nw + x] = (at(2 * x, 2 * y) + at(2 * x + 1, 2 * y) + at(2 * x, 2 * y + 1) + at(2 * x + 1, 2 * y + 1)) * 0.25;
        }
    }
    (out, nw, nh)
}

/// Bilinear upsample of a `sw × sh` plane to `w × h` (exactly 2× apart, rounded up).
fn upsample(src: &[f32], sw: usize, sh: usize, w: usize, h: usize) -> Vec<f32> {
    let at = |x: isize, y: isize| src[(y.clamp(0, sh as isize - 1) as usize) * sw + x.clamp(0, sw as isize - 1) as usize];
    let mut out = vec![0.0; w * h];
    for y in 0..h {
        let fy = (y as f32 + 0.5) / 2.0 - 0.5;
        let (y0, ty) = (fy.floor() as isize, fy - fy.floor());
        for x in 0..w {
            let fx = (x as f32 + 0.5) / 2.0 - 0.5;
            let (x0, tx) = (fx.floor() as isize, fx - fx.floor());
            let top = at(x0, y0) * (1.0 - tx) + at(x0 + 1, y0) * tx;
            let bot = at(x0, y0 + 1) * (1.0 - tx) + at(x0 + 1, y0 + 1) * tx;
            out[y * w + x] = top * (1.0 - ty) + bot * ty;
        }
    }
    out
}

/// Gaussian blur of one plane in place.
pub fn blur_plane(p: &mut Vec<f32>, w: usize, h: usize, sigma: f64) {
    if sigma < 0.1 || w == 0 || h == 0 {
        return;
    }
    let mut sizes = vec![(w, h)];
    let mut cur = std::mem::take(p);
    let mut s = sigma;
    while s > OCTAVE_SIGMA {
        let (cw, ch) = *sizes.last().unwrap_or(&(w, h));
        let (d, nw, nh) = downsample(&cur, cw, ch);
        cur = d;
        sizes.push((nw, nh));
        s /= 2.0;
    }
    let (cw, ch) = *sizes.last().unwrap_or(&(w, h));
    let k = kernel(s);
    cur = convolve(&convolve(&cur, cw, ch, &k, true), cw, ch, &k, false);
    for i in (1..sizes.len()).rev() {
        let (sw, sh) = sizes[i];
        let (tw, th) = sizes[i - 1];
        cur = upsample(&cur, sw, sh, tw, th);
    }
    *p = cur;
}

/// Gaussian blur of all four (premultiplied) channels.
pub fn blur(px: &mut Pixels, sigma: f64) {
    let (w, h) = (px.w as usize, px.h as usize);
    let planes: Vec<Vec<f32>> = (0..4)
        .into_par_iter()
        .map(|c| {
            let mut p = px.plane(c);
            blur_plane(&mut p, w, h, sigma);
            p
        })
        .collect();
    for (c, p) in planes.iter().enumerate() {
        px.set_plane(c, p);
    }
}

/// Grows (r > 0) or shrinks (r < 0) the alpha plane by `|r|` pixels (square structuring
/// element, separable max/min).
pub fn dilate(a: &mut [f32], w: usize, h: usize, r: i32, outside: f32) {
    if r == 0 {
        return;
    }
    let grow = r > 0;
    let r = r.unsigned_abs() as isize;
    for horizontal in [true, false] {
        let src = a.to_vec();
        for y in 0..h {
            for x in 0..w {
                let mut v = if grow { 0.0f32 } else { 1.0f32 };
                for d in -r..=r {
                    let (sx, sy) = if horizontal { (x as isize + d, y as isize) } else { (x as isize, y as isize + d) };
                    let s = if sx >= 0 && sy >= 0 && (sx as usize) < w && (sy as usize) < h { src[sy as usize * w + sx as usize] } else { outside };
                    v = if grow { v.max(s) } else { v.min(s) };
                }
                a[y * w + x] = v;
            }
        }
    }
}

/// Moves a plane by whole pixels; uncovered pixels get `fill`.
pub fn shift(a: &[f32], w: usize, h: usize, dx: i32, dy: i32, fill: f32) -> Vec<f32> {
    let mut out = vec![fill; w * h];
    for y in 0..h as i32 {
        for x in 0..w as i32 {
            let (sx, sy) = (x - dx, y - dy);
            if sx >= 0 && sy >= 0 && sx < w as i32 && sy < h as i32 {
                out[(y as usize) * w + x as usize] = a[sy as usize * w + sx as usize];
            }
        }
    }
    out
}

fn tinted(src: &Pixels, a: &[f32], color: [f32; 4]) -> Pixels {
    let mut out = Pixels::new(src.x, src.y, src.w, src.h);
    for (p, v) in out.data.as_chunks_mut::<4>().0.iter_mut().zip(a) {
        let al = v.clamp(0.0, 1.0) * color[3];
        for c in 0..3 {
            p[c] = (color[c] * al * 255.0 + 0.5) as u8;
        }
        p[3] = (al * 255.0 + 0.5) as u8;
    }
    out
}

/// A drop shadow of `src`'s alpha: dilated by `spread`, moved by `(dx, dy)`, blurred, tinted with
/// straight-alpha `color`. Lengths in device pixels.
pub fn drop_shadow(src: &Pixels, color: [f32; 4], dx: i32, dy: i32, sigma: f64, spread: i32) -> Pixels {
    let (w, h) = (src.w as usize, src.h as usize);
    let mut a = src.plane(3);
    dilate(&mut a, w, h, spread, 0.0);
    let mut a = shift(&a, w, h, dx, dy, 0.0);
    blur_plane(&mut a, w, h, sigma);
    tinted(src, &a, color)
}

/// An inner shadow inside `src`'s alpha: the outside (1 − alpha), grown by `spread`, moved by
/// `(dx, dy)`, blurred, then kept only where `src` is.
pub fn inner_shadow(src: &Pixels, color: [f32; 4], dx: i32, dy: i32, sigma: f64, spread: i32) -> Pixels {
    let (w, h) = (src.w as usize, src.h as usize);
    let a = src.plane(3);
    let mut inv: Vec<f32> = a.iter().map(|v| 1.0 - v).collect();
    dilate(&mut inv, w, h, spread, 1.0);
    let mut inv = shift(&inv, w, h, dx, dy, 1.0);
    blur_plane_outside(&mut inv, w, h, sigma);
    let masked: Vec<f32> = inv.iter().zip(&a).map(|(i, a)| i * a).collect();
    tinted(src, &masked, color)
}

/// Blur where everything outside the plane is 1 (the inverted alpha of an inner shadow).
fn blur_plane_outside(p: &mut Vec<f32>, w: usize, h: usize, sigma: f64) {
    for v in p.iter_mut() {
        *v = 1.0 - *v;
    }
    blur_plane(p, w, h, sigma);
    for v in p.iter_mut() {
        *v = 1.0 - *v;
    }
}
