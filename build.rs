extern crate embed_resource;
extern crate ico;
extern crate image;

use ico::{IconDir, IconImage};
use image::{ImageBuffer, Rgba};
use std::fs::File;
use std::io::BufWriter;

fn main() {
    let mut image = ImageBuffer::new(64, 64);
    for (x, y, pixel) in image.enumerate_pixels_mut() {
        let border = 2;
        let is_border = x < border || x >= 64 - border || y < border || y >= 64 - border;

        // A simple, modern "S" shape for "SyncU"
        let is_s_shape = (x > 16 && x < 48 && y > 16 && y < 24) || // Top bar
            (x > 16 && x < 32 && y > 24 && y < 32) || // Top-left vertical
            (x > 16 && x < 48 && y > 32 && y < 40) || // Middle bar
            (x > 32 && x < 48 && y > 40 && y < 48) || // Bottom-right vertical
            (x > 16 && x < 48 && y > 48 && y < 56); // Bottom bar

        if is_border {
            *pixel = Rgba([0, 120, 215, 255]); // Blue border
        } else if is_s_shape {
            *pixel = Rgba([0, 120, 215, 255]); // Blue 'S'
        } else {
            *pixel = Rgba([255, 255, 255, 255]); // White background
        }
    }

    let icon_image = IconImage::from_rgba_data(64, 64, image.into_raw());
    let mut icon_dir = IconDir::new(ico::ResourceType::Icon);
    icon_dir.add_entry(ico::IconDirEntry::encode(&icon_image).unwrap());
    let file = BufWriter::new(File::create("icon.ico").unwrap());
    icon_dir.write(file).unwrap();

    let _ = embed_resource::compile("icon.rc", std::iter::empty::<&str>());
}
