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
        let background_value = 245;
        *pixel = Rgba([background_value, background_value, background_value, 255]);
        
        let center_x = x as f32 - 32.0;
        let center_y = y as f32 - 32.0;
        let distance_from_center = (center_x * center_x + center_y * center_y).sqrt();
        
        if distance_from_center < 28.0 {
            if (x > 18 && x < 46) && 
               ((y > 18 && y < 24) ||
                (x > 18 && x < 26 && y > 24 && y < 32) ||
                (y > 32 && y < 38) ||
                (x > 38 && x < 46 && y > 38 && y < 46) ||
                (y > 46 && y < 52)) {
                *pixel = Rgba([0, 120, 215, 255]);
            }
        }
        
        if distance_from_center > 26.0 && distance_from_center < 28.0 {
            *pixel = Rgba([200, 200, 200, 100]);
        }
    }

    let icon_image = IconImage::from_rgba_data(64, 64, image.into_raw());
    let mut icon_dir = IconDir::new(ico::ResourceType::Icon);
    icon_dir.add_entry(ico::IconDirEntry::encode(&icon_image).unwrap());
    let file = BufWriter::new(File::create("icon.ico").unwrap());
    icon_dir.write(file).unwrap();

    let _ = embed_resource::compile("icon.rc", std::iter::empty::<&str>());
}
