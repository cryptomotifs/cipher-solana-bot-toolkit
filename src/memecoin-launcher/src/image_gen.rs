//! Image generator — template-based 1024x1024 PNG logos.
//!
//! [VERIFIED 2026] pumpfun_token_creation_technical_2026.md s6: "PNG, 1024x1024, under 5MB"
//! [VERIFIED 2026] pumpfun_token_creation_technical_2026.md s11 Option 3: "Template-Based Generation"

use anyhow::Result;
use image::{Rgba, RgbaImage};
use rand::Rng;

use crate::concept::TokenConcept;

/// Generate a simple but visually distinct token logo.
/// Creates a 1024x1024 PNG with a vibrant circle and symbol text.
pub fn generate_logo(concept: &TokenConcept) -> Result<String> {
    let size = 1024u32;
    let mut img = RgbaImage::new(size, size);
    let mut rng = rand::thread_rng();

    // Random vibrant background color
    let bg = Rgba([
        rng.gen_range(40..220),
        rng.gen_range(40..220),
        rng.gen_range(40..220),
        255,
    ]);

    // Dark background
    let dark_bg = Rgba([15, 15, 25, 255]);

    // Fill with dark background
    for pixel in img.pixels_mut() {
        *pixel = dark_bg;
    }

    // Draw filled circle
    let center = (size / 2) as i32;
    let radius = (size / 2 - 40) as i32;
    for y in 0..size {
        for x in 0..size {
            let dx = x as i32 - center;
            let dy = y as i32 - center;
            if dx * dx + dy * dy <= radius * radius {
                img.put_pixel(x, y, bg);
            }
        }
    }

    // Draw a lighter inner ring for depth
    let inner_radius = radius - 30;
    let lighter = Rgba([
        (bg[0] as u16 + 40).min(255) as u8,
        (bg[1] as u16 + 40).min(255) as u8,
        (bg[2] as u16 + 40).min(255) as u8,
        255,
    ]);
    for y in 0..size {
        for x in 0..size {
            let dx = x as i32 - center;
            let dy = y as i32 - center;
            let dist_sq = dx * dx + dy * dy;
            if dist_sq <= inner_radius * inner_radius && dist_sq > (inner_radius - 8) * (inner_radius - 8) {
                img.put_pixel(x, y, lighter);
            }
        }
    }

    // Draw symbol text (simple pixel font — just the first 1-3 chars, large)
    let symbol = &concept.symbol;
    let text_color = Rgba([255, 255, 255, 255]);

    // Simple large block letters in the center
    draw_text_centered(&mut img, symbol, center, center, text_color);

    // Save to temp file
    let temp_dir = std::env::temp_dir();
    let filename = format!("launcher_{}.png", concept.symbol.to_lowercase());
    let path = temp_dir.join(&filename);
    img.save(&path)?;

    let path_str = path.to_string_lossy().to_string();
    tracing::info!("Generated logo: {} ({}x{})", path_str, size, size);

    Ok(path_str)
}

/// Draw text centered on the image using simple block characters.
/// This is a basic implementation — no font dependency needed.
fn draw_text_centered(img: &mut RgbaImage, text: &str, cx: i32, cy: i32, color: Rgba<u8>) {
    let chars: Vec<char> = text.chars().take(4).collect();
    let char_width = 120i32;
    let char_height = 160i32;
    let spacing = 20i32;
    let total_width = chars.len() as i32 * (char_width + spacing) - spacing;
    let start_x = cx - total_width / 2;
    let start_y = cy - char_height / 2;

    for (i, ch) in chars.iter().enumerate() {
        let x = start_x + i as i32 * (char_width + spacing);
        draw_block_char(img, *ch, x, start_y, char_width, char_height, color);
    }
}

/// Draw a single character as a simple block pattern.
fn draw_block_char(img: &mut RgbaImage, ch: char, x: i32, y: i32, w: i32, h: i32, color: Rgba<u8>) {
    let thickness = w / 5;

    // Simple block letter rendering (covers A-Z, 0-9)
    match ch.to_ascii_uppercase() {
        'A' => {
            draw_rect(img, x, y, thickness, h, color); // left
            draw_rect(img, x + w - thickness, y, thickness, h, color); // right
            draw_rect(img, x, y, w, thickness, color); // top
            draw_rect(img, x, y + h / 2, w, thickness, color); // middle
        }
        'B' | 'D' | 'P' | 'R' => {
            draw_rect(img, x, y, thickness, h, color); // left
            draw_rect(img, x, y, w, thickness, color); // top
            draw_rect(img, x + w - thickness, y, thickness, h / 2, color); // right top
            draw_rect(img, x, y + h / 2, w, thickness, color); // middle
        }
        'C' | 'G' => {
            draw_rect(img, x, y, thickness, h, color); // left
            draw_rect(img, x, y, w, thickness, color); // top
            draw_rect(img, x, y + h - thickness, w, thickness, color); // bottom
        }
        'E' | 'F' => {
            draw_rect(img, x, y, thickness, h, color); // left
            draw_rect(img, x, y, w, thickness, color); // top
            draw_rect(img, x, y + h / 2, w * 3 / 4, thickness, color); // middle
            if ch == 'E' {
                draw_rect(img, x, y + h - thickness, w, thickness, color); // bottom
            }
        }
        'H' | 'K' => {
            draw_rect(img, x, y, thickness, h, color); // left
            draw_rect(img, x + w - thickness, y, thickness, h, color); // right
            draw_rect(img, x, y + h / 2, w, thickness, color); // middle
        }
        'I' => {
            draw_rect(img, x + w / 2 - thickness / 2, y, thickness, h, color); // center
            draw_rect(img, x, y, w, thickness, color); // top
            draw_rect(img, x, y + h - thickness, w, thickness, color); // bottom
        }
        'L' => {
            draw_rect(img, x, y, thickness, h, color); // left
            draw_rect(img, x, y + h - thickness, w, thickness, color); // bottom
        }
        'M' => {
            draw_rect(img, x, y, thickness, h, color); // left
            draw_rect(img, x + w - thickness, y, thickness, h, color); // right
            draw_rect(img, x + w / 2 - thickness / 2, y, thickness, h / 2, color); // center
            draw_rect(img, x, y, w, thickness, color); // top
        }
        'N' => {
            draw_rect(img, x, y, thickness, h, color); // left
            draw_rect(img, x + w - thickness, y, thickness, h, color); // right
            draw_rect(img, x, y, w, thickness, color); // top
        }
        'O' | 'Q' | '0' => {
            draw_rect(img, x, y, thickness, h, color); // left
            draw_rect(img, x + w - thickness, y, thickness, h, color); // right
            draw_rect(img, x, y, w, thickness, color); // top
            draw_rect(img, x, y + h - thickness, w, thickness, color); // bottom
        }
        'S' | '5' => {
            draw_rect(img, x, y, w, thickness, color); // top
            draw_rect(img, x, y, thickness, h / 2, color); // left top
            draw_rect(img, x, y + h / 2, w, thickness, color); // middle
            draw_rect(img, x + w - thickness, y + h / 2, thickness, h / 2, color); // right bottom
            draw_rect(img, x, y + h - thickness, w, thickness, color); // bottom
        }
        'T' => {
            draw_rect(img, x, y, w, thickness, color); // top
            draw_rect(img, x + w / 2 - thickness / 2, y, thickness, h, color); // center
        }
        'U' | 'V' => {
            draw_rect(img, x, y, thickness, h, color); // left
            draw_rect(img, x + w - thickness, y, thickness, h, color); // right
            draw_rect(img, x, y + h - thickness, w, thickness, color); // bottom
        }
        'W' => {
            draw_rect(img, x, y, thickness, h, color); // left
            draw_rect(img, x + w - thickness, y, thickness, h, color); // right
            draw_rect(img, x + w / 2 - thickness / 2, y + h / 2, thickness, h / 2, color); // center
            draw_rect(img, x, y + h - thickness, w, thickness, color); // bottom
        }
        'X' | 'Y' | 'Z' => {
            draw_rect(img, x, y, thickness, h / 2, color); // left top
            draw_rect(img, x + w - thickness, y + h / 2, thickness, h / 2, color); // right bottom
            draw_rect(img, x, y + h / 2, w, thickness, color); // middle
        }
        _ => {
            // Default: filled rectangle
            draw_rect(img, x, y, w, h, color);
        }
    }
}

/// Draw a filled rectangle on the image.
fn draw_rect(img: &mut RgbaImage, x: i32, y: i32, w: i32, h: i32, color: Rgba<u8>) {
    let (iw, ih) = (img.width() as i32, img.height() as i32);
    for dy in 0..h {
        for dx in 0..w {
            let px = x + dx;
            let py = y + dy;
            if px >= 0 && px < iw && py >= 0 && py < ih {
                img.put_pixel(px as u32, py as u32, color);
            }
        }
    }
}
