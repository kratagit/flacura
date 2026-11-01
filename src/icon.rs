//! Shared golden-record artwork for the window and the Windows executable.

pub fn rgba(size: usize) -> Vec<u8> {
    let mut pixels = vec![0u8; size * size * 4];
    for y in 0..size {
        for x in 0..size {
            let dx = (x as f32 + 0.5) * 64.0 / size as f32 - 32.0;
            let dy = (y as f32 + 0.5) * 64.0 / size as f32 - 32.0;
            let r = dx.hypot(dy);
            let edge = ((30.5 - r) * size as f32 / 64.0).clamp(0.0, 1.0);
            let (color, alpha) = if r < 3.5 {
                ([255, 222, 140], 1.0)
            } else if r < 10.0 {
                ([14, 15, 18], 1.0)
            } else {
                let groove = if (r as i32) % 3 == 0 { 0.82 } else { 1.0 };
                ([236, 186, 76].map(|c| (c as f32 * groove) as u8), edge)
            };
            let offset = (y * size + x) * 4;
            pixels[offset..offset + 4].copy_from_slice(&[
                color[0],
                color[1],
                color[2],
                (alpha * 255.0) as u8,
            ]);
        }
    }
    pixels
}
