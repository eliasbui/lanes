//! Draws the Lanes icons into assets/: the app icon (a glossy blue tile with a download arrow over three lanes),
//! a plain window icon, and a black template for the macOS menu bar. Run once with `cargo run --example make_icons`.
//! Everything is supersampled four times and box-filtered down, so the rounded edges stay smooth.

use std::{fs::File, io::BufWriter, path::Path};

const SS: usize = 4;

fn main() {
    write_png("assets/icon.png", app_icon(1024));
    write_png("assets/icon-256.png", app_icon(256));
    write_png("assets/icon-64.png", app_icon(64));
    write_png("assets/tray.png", tray(44));
    write_ico("assets/icon.ico", &app_icon(256));
    println!("wrote assets/icon.png, icon-256.png, icon-64.png, tray.png, icon.ico");
}

/// Colour at a pixel, with alpha, before downsampling. `(x, y)` are in the supersampled grid of side `n*SS`.
type Shade = Box<dyn Fn(f32, f32) -> [f32; 4]>;

fn app_icon(size: usize) -> Image {
    let big = size * SS;
    let s = big as f32;
    let radius = s * 0.225;
    // Apple-blue tile, lighter at the top, with a soft highlight, like the glass tiles of macOS.
    let top = [0.16, 0.56, 1.0];
    let bottom = [0.0, 0.25, 0.86];
    let shade: Shade = Box::new(move |x, y| {
        let inside = rounded_coverage(x, y, s, radius);
        if inside <= 0.0 {
            return [0.0, 0.0, 0.0, 0.0];
        }
        let t = (y / s).clamp(0.0, 1.0);
        let mut rgb = mix(top, bottom, t);
        // A gentle diagonal gloss across the upper third.
        let gloss = (1.0 - ((x + y) / (s * 1.4))).clamp(0.0, 1.0).powf(2.2) * 0.18;
        rgb = [(rgb[0] + gloss).min(1.0), (rgb[1] + gloss).min(1.0), (rgb[2] + gloss).min(1.0)];
        // Three faint lanes behind the arrow.
        let lane = lane_mask(x, y, s) * 0.08;
        rgb = [(rgb[0] + lane).min(1.0), (rgb[1] + lane).min(1.0), (rgb[2] + lane).min(1.0)];
        // The white download arrow on top.
        let arrow = arrow_mask(x, y, s);
        rgb = mix(rgb, [1.0, 1.0, 1.0], arrow);
        [rgb[0], rgb[1], rgb[2], inside]
    });
    render(big, size, &shade)
}

fn tray(size: usize) -> Image {
    let big = size * SS;
    let s = big as f32;
    // A black arrow on transparent: macOS tints a template icon itself.
    let shade: Shade = Box::new(move |x, y| {
        let a = arrow_mask(x, y, s);
        [0.0, 0.0, 0.0, a]
    });
    render(big, size, &shade)
}

/// The white download glyph: a vertical stem, a solid arrowhead, and a tray line under it.
fn arrow_mask(x: f32, y: f32, s: f32) -> f32 {
    let cx = s * 0.5;
    let stem = band(x, cx, s * 0.085) * span(y, s * 0.22, s * 0.47);
    let head = triangle(x, y, (cx - s * 0.185, s * 0.44), (cx + s * 0.185, s * 0.44), (cx, s * 0.66));
    let tray = band(y, s * 0.745, s * 0.05) * span(x, s * 0.30, s * 0.70);
    (stem.max(head) + tray).min(1.0)
}

/// Soft coverage of a filled triangle, whatever the winding of its three points: a point is inside when it falls on
/// the same side of all three edges, so the interior distance is the larger of "all positive" and "all negative".
fn triangle(px: f32, py: f32, a: (f32, f32), b: (f32, f32), c: (f32, f32)) -> f32 {
    let edge = |p: (f32, f32), q: (f32, f32)| {
        let (ex, ey) = (q.0 - p.0, q.1 - p.1);
        let len = (ex * ex + ey * ey).sqrt().max(1e-6);
        ((px - p.0) * ey - (py - p.1) * ex) / len
    };
    let (d1, d2, d3) = (edge(a, b), edge(b, c), edge(c, a));
    let inside = d1.min(d2).min(d3).max(-(d1.max(d2).max(d3)));
    smooth(-1.5, 1.5, inside)
}

/// Three dim vertical lanes behind the glyph.
fn lane_mask(x: f32, y: f32, s: f32) -> f32 {
    let w = s * 0.045;
    let mut m: f32 = 0.0;
    for cx in [0.34, 0.5, 0.66] {
        m = m.max(band(x, s * cx, w) * span(y, s * 0.20, s * 0.74));
    }
    m
}

/// A soft-edged band of half-width `w` around `centre` in one axis.
fn band(v: f32, centre: f32, w: f32) -> f32 {
    let d = (v - centre).abs();
    smooth(w + 1.5, w - 1.5, d)
}

/// 1 inside `[lo, hi]` with a soft edge.
fn span(v: f32, lo: f32, hi: f32) -> f32 {
    smooth(lo - 1.5, lo + 1.5, v) * smooth(hi + 1.5, hi - 1.5, v)
}

/// Coverage of a rounded square of side `s` and corner `r`, 1 inside, 0 outside, soft at the edge.
fn rounded_coverage(x: f32, y: f32, s: f32, r: f32) -> f32 {
    let dx = (x - s * 0.5).abs() - (s * 0.5 - r);
    let dy = (y - s * 0.5).abs() - (s * 0.5 - r);
    let outside = dx.max(0.0).hypot(dy.max(0.0)) - r;
    let inside = dx.max(dy).min(0.0);
    smooth(1.5, -1.5, outside + inside)
}

/// A smoothstep from 0 at `edge0` to 1 at `edge1` (either order).
fn smooth(edge0: f32, edge1: f32, v: f32) -> f32 {
    let t = ((v - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn mix(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t]
}

struct Image {
    size: usize,
    rgba: Vec<u8>,
}

/// Renders the shade at `big = size*SS`, then box-filters down to `size`, straight-alpha to premultiplied and back
/// so the edges do not darken.
fn render(big: usize, size: usize, shade: &Shade) -> Image {
    let mut src = vec![0.0f32; big * big * 4];
    for y in 0..big {
        for x in 0..big {
            let c = shade(x as f32 + 0.5, y as f32 + 0.5);
            let i = (y * big + x) * 4;
            src[i] = c[0] * c[3];
            src[i + 1] = c[1] * c[3];
            src[i + 2] = c[2] * c[3];
            src[i + 3] = c[3];
        }
    }
    let mut rgba = vec![0u8; size * size * 4];
    let n = (SS * SS) as f32;
    for y in 0..size {
        for x in 0..size {
            let mut acc = [0.0f32; 4];
            for sy in 0..SS {
                for sx in 0..SS {
                    let i = (((y * SS + sy) * big) + (x * SS + sx)) * 4;
                    for k in 0..4 {
                        acc[k] += src[i + k];
                    }
                }
            }
            let a = acc[3] / n;
            let o = (y * size + x) * 4;
            let unmul = |c: f32| if a > 0.0 { ((c / n) / a * 255.0).round().clamp(0.0, 255.0) as u8 } else { 0 };
            rgba[o] = unmul(acc[0]);
            rgba[o + 1] = unmul(acc[1]);
            rgba[o + 2] = unmul(acc[2]);
            rgba[o + 3] = (a * 255.0).round().clamp(0.0, 255.0) as u8;
        }
    }
    Image { size, rgba }
}

fn write_png(path: &str, image: Image) {
    let file = BufWriter::new(File::create(Path::new(path)).expect("create png"));
    let mut encoder = png::Encoder::new(file, image.size as u32, image.size as u32);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.write_header().expect("header").write_image_data(&image.rgba).expect("pixels");
}

/// A minimal single-image .ico wrapping a PNG, which Windows Vista and later accept.
fn write_ico(path: &str, image: &Image) {
    let mut png = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut png, image.size as u32, image.size as u32);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.write_header().unwrap().write_image_data(&image.rgba).unwrap();
    }
    let mut ico = Vec::new();
    ico.extend_from_slice(&[0, 0, 1, 0, 1, 0]); // ICONDIR: reserved, type 1, one image
    let side = if image.size >= 256 { 0u8 } else { image.size as u8 };
    ico.extend_from_slice(&[side, side, 0, 0, 1, 0, 32, 0]); // width, height, colours, reserved, planes, bpp
    ico.extend_from_slice(&(png.len() as u32).to_le_bytes());
    ico.extend_from_slice(&22u32.to_le_bytes()); // offset to the PNG
    ico.extend_from_slice(&png);
    std::fs::write(path, ico).expect("write ico");
}
