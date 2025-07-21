use image::{ImageBuffer, Rgba};

fn main() {
    let mut image = ImageBuffer::new(64, 64);
    for (x, y, pixel) in image.enumerate_pixels_mut() {
        let is_border = x < 2 || x > 61 || y < 2 || y > 61;
        let is_u_shape = (x > 15 && x < 48 && y > 15 && y < 48) && !(x > 18 && x < 45 && y > 18 && y < 45);
        
        if is_border {
            *pixel = Rgba([0, 120, 215, 255]); // Blue border
        } else if is_u_shape {
            *pixel = Rgba([0, 120, 215, 255]); // Blue 'U'
        } else {
            *pixel = Rgba([255, 255, 255, 255]); // White background
        }
    }
    image.save(".cargo/icon.png").unwrap();
}