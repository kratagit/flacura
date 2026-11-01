#[path = "src/icon.rs"]
mod icon;

fn main() {
    println!("cargo:rerun-if-changed=src/icon.rs");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let version = std::env::var("CARGO_PKG_VERSION").expect("package version");
    let icon_path = std::path::PathBuf::from(std::env::var_os("OUT_DIR").expect("build output"))
        .join("flacura.ico");
    std::fs::write(&icon_path, windows_icon()).expect("write Windows icon");
    winresource::WindowsResource::new()
        .set_icon(icon_path.to_str().expect("icon path"))
        .set("ProductName", "Flacura")
        .set("FileDescription", &format!("Flacura v{version}"))
        .set("FileVersion", &version)
        .set("ProductVersion", &version)
        .set("InternalName", "flacura")
        .set("OriginalFilename", &format!("flacura-{version}.exe"))
        .compile()
        .expect("compile Windows icon and version resources");
}

// Uncompressed 32-bit DIB entries work with both rc.exe and GNU windres.
fn windows_icon() -> Vec<u8> {
    let sizes = [16usize, 24, 32, 48, 64, 128, 256];
    let mut output = Vec::new();
    for value in [0u16, 1, sizes.len() as u16] {
        output.extend(value.to_le_bytes());
    }
    let mut offset = 6 + 16 * sizes.len();
    let mut images = Vec::new();
    for size in sizes {
        let pixels = icon::rgba(size);
        let mask_stride = size.div_ceil(32) * 4;
        let image_len = 40 + size * size * 4 + mask_stride * size;
        output.extend([size as u8, size as u8, 0, 0]);
        output.extend(1u16.to_le_bytes());
        output.extend(32u16.to_le_bytes());
        output.extend((image_len as u32).to_le_bytes());
        output.extend((offset as u32).to_le_bytes());
        offset += image_len;

        for value in [40u32, size as u32, (size * 2) as u32] {
            images.extend(value.to_le_bytes());
        }
        images.extend(1u16.to_le_bytes());
        images.extend(32u16.to_le_bytes());
        for value in [0u32, (size * size * 4) as u32, 0, 0, 0, 0] {
            images.extend(value.to_le_bytes());
        }
        for row in pixels.chunks_exact(size * 4).rev() {
            for pixel in row.as_chunks::<4>().0 {
                images.extend([pixel[2], pixel[1], pixel[0], pixel[3]]);
            }
        }
        let mut mask = vec![0u8; mask_stride * size];
        for y in 0..size {
            for x in 0..size {
                if pixels[((size - 1 - y) * size + x) * 4 + 3] == 0 {
                    mask[y * mask_stride + x / 8] |= 0x80 >> (x % 8);
                }
            }
        }
        images.extend(mask);
    }
    output.extend(images);
    output
}
