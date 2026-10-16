//! Compact, persistent artwork for the existing 28x8-cell banner.
use std::path::Path;
use std::sync::Arc;
use std::sync::{Mutex, OnceLock};

use anyhow::{ensure, Context};
use base64::{engine::general_purpose::STANDARD, Engine};
use image::ImageDecoder;
use serde::{Deserialize, Serialize};

const WIDTH: u32 = crate::logo::LOGO_WIDTH as u32;
const HEIGHT: u32 = crate::logo::LOGO_HEIGHT as u32 * 2;
const BYTES: usize = (WIDTH * HEIGHT * 4) as usize;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CustomLogo {
    /// Only the reduced RGBA raster is persisted, never the original file.
    pub rgba: String,
    /// A bounded thumbnail for pixel-capable terminals; older preferences omit it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub png: Option<String>,
}

impl CustomLogo {
    pub fn from_path(path: &Path) -> anyhow::Result<Self> {
        ensure!(path.is_file(), "Image path must refer to a regular file");
        let mut reader = image::ImageReader::open(path)?.with_guessed_format()?;
        let mut limits = image::Limits::default();
        limits.max_image_width = Some(16384);
        limits.max_image_height = Some(16384);
        limits.max_alloc = Some(128 * 1024 * 1024);
        reader.limits(limits);
        let mut decoder = reader.into_decoder().context("Could not decode image")?;
        let orientation = decoder.orientation()?;
        let mut source =
            image::DynamicImage::from_decoder(decoder).context("Could not decode image")?;
        source.apply_orientation(orientation);
        let photo = source.resize(560, 320, image::imageops::FilterType::Lanczos3);
        let mut png = std::io::Cursor::new(Vec::new());
        photo.write_to(&mut png, image::ImageFormat::Png)?;
        let resized = source
            .resize(WIDTH, HEIGHT, image::imageops::FilterType::Lanczos3)
            .to_rgba8();
        let mut canvas = image::RgbaImage::new(WIDTH, HEIGHT);
        image::imageops::overlay(
            &mut canvas,
            &resized,
            ((WIDTH - resized.width()) / 2) as i64,
            ((HEIGHT - resized.height()) / 2) as i64,
        );
        Ok(Self {
            rgba: STANDARD.encode(canvas.as_raw()),
            png: Some(STANDARD.encode(png.into_inner())),
        })
    }

    pub fn lines(&self) -> anyhow::Result<Vec<String>> {
        ensure!(
            self.rgba.len() <= BYTES.div_ceil(3) * 4,
            "Invalid logo data size"
        );
        let pixels = STANDARD.decode(&self.rgba).context("Invalid logo pixels")?;
        ensure!(pixels.len() == BYTES, "Invalid logo dimensions");
        let mut lines = Vec::new();
        for y in (0..HEIGHT).step_by(2) {
            let mut line = String::new();
            for x in 0..WIDTH {
                let offset = ((y * WIDTH + x) * 4) as usize;
                let top = &pixels[offset..offset + 4];
                let bottom_offset = offset + WIDTH as usize * 4;
                let bottom = &pixels[bottom_offset..bottom_offset + 4];
                let color = |pixel: &[u8], foreground: bool| {
                    // Partial alpha is blended against the dark terminal ground.
                    let rgb: Vec<u8> = pixel[..3]
                        .iter()
                        .map(|c| ((*c as u32 * pixel[3] as u32) / 255) as u8)
                        .collect();
                    format!(
                        "\x1b[{};2;{};{};{}m",
                        if foreground { 38 } else { 48 },
                        rgb[0],
                        rgb[1],
                        rgb[2]
                    )
                };
                match (top[3] > 0, bottom[3] > 0) {
                    (false, false) => line.push_str("\x1b[0m "),
                    (true, false) => line.push_str(&format!("\x1b[0m{}▀", color(top, true))),
                    (false, true) => line.push_str(&format!("\x1b[0m{}▄", color(bottom, true))),
                    (true, true) => {
                        line.push_str(&format!("{}{}▀", color(top, true), color(bottom, false)))
                    }
                }
            }
            line.push_str("\x1b[0m");
            lines.push(line);
        }
        Ok(lines)
    }
}

struct CurrentLogo {
    lines: Vec<String>,
    photo: Arc<image::RgbaImage>,
    generation: u64,
}

static CURRENT: OnceLock<Mutex<Option<CurrentLogo>>> = OnceLock::new();
static GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

pub fn set_current(logo: Option<&CustomLogo>) {
    let current = logo.and_then(|logo| {
        let lines = logo.lines().ok()?;
        let photo = logo
            .png
            .as_ref()
            .filter(|png| png.len() <= 2 * 1024 * 1024)
            .and_then(|png| STANDARD.decode(png).ok())
            .and_then(|bytes| {
                let mut reader = image::ImageReader::with_format(
                    std::io::Cursor::new(bytes),
                    image::ImageFormat::Png,
                );
                let mut limits = image::Limits::default();
                limits.max_image_width = Some(560);
                limits.max_image_height = Some(320);
                limits.max_alloc = Some(4 * 1024 * 1024);
                reader.limits(limits);
                reader.decode().ok().map(|image| image.to_rgba8())
            })
            .or_else(|| {
                image::RgbaImage::from_raw(WIDTH, HEIGHT, STANDARD.decode(&logo.rgba).ok()?)
            })?;
        Some(CurrentLogo {
            lines,
            photo: Arc::new(photo),
            generation: GENERATION.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1,
        })
    });
    *CURRENT.get_or_init(|| Mutex::new(None)).lock().unwrap() = current;
}

pub fn current_lines() -> Vec<String> {
    if crate::terminal_image::backend() != crate::terminal_image::Backend::Art {
        return vec![" ".repeat(WIDTH as usize); HEIGHT as usize / 2];
    }
    CURRENT
        .get_or_init(|| Mutex::new(None))
        .lock()
        .unwrap()
        .as_ref()
        .map(|logo| logo.lines.clone())
        .unwrap_or_else(|| {
            crate::logo::LOGO_LINES
                .iter()
                .map(|line| line.to_string())
                .collect()
        })
}

fn stock_photo() -> Arc<image::RgbaImage> {
    static PHOTO: OnceLock<Arc<image::RgbaImage>> = OnceLock::new();
    Arc::clone(PHOTO.get_or_init(|| {
        Arc::new(
            image::load_from_memory_with_format(
                include_bytes!("../assets/logo.png"),
                image::ImageFormat::Png,
            )
            .expect("bundled Takiza logo must be a valid PNG")
            .to_rgba8(),
        )
    }))
}

pub fn current_photo() -> Option<(u64, Arc<image::RgbaImage>)> {
    CURRENT
        .get_or_init(|| Mutex::new(None))
        .lock()
        .unwrap()
        .as_ref()
        .map(|logo| (logo.generation, Arc::clone(&logo.photo)))
        .or_else(|| Some((0, stock_photo())))
}

pub fn image_path(value: &str) -> std::path::PathBuf {
    let value = value.trim();
    if let Some(json) = value
        .strip_prefix("[Image: ")
        .and_then(|value| value.strip_suffix(']'))
    {
        if let Ok(path) = serde_json::from_str::<String>(json) {
            return path.into();
        }
    }
    let value = if value.len() >= 2
        && ((value.starts_with('"') && value.ends_with('"'))
            || (value.starts_with('\'') && value.ends_with('\'')))
    {
        &value[1..value.len() - 1]
    } else {
        value
    };
    if let Some(relative) = value.strip_prefix("~/") {
        if let Some(home) = crate::theme::dirs_next_or_home() {
            return home.join(relative);
        }
    }
    value.into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_logo_has_detail_and_transparency() {
        let photo = stock_photo();
        assert_eq!(photo.dimensions(), (543, 320));
        assert!(photo.pixels().any(|p| p[3] == 0));
        assert!(photo.pixels().any(|p| p[3] == 255 && p[0] > 200 && p[1] > 150 && p[2] < 100));
        assert!(Arc::ptr_eq(&photo, &stock_photo()));
    }

    #[test]
    fn square_is_centered_and_keeps_colors_and_transparency() {
        let path = std::env::temp_dir().join(format!("takiza-logo-{}.png", std::process::id()));
        image::RgbaImage::from_pixel(1024, 1024, image::Rgba([120, 45, 210, 255]))
            .save(&path)
            .unwrap();
        let logo = CustomLogo::from_path(&path).unwrap();
        std::fs::remove_file(path).unwrap();
        let pixels = STANDARD.decode(&logo.rgba).unwrap();
        assert_eq!(pixels.len(), BYTES);
        for y in 0..HEIGHT {
            for x in 0..WIDTH {
                let start = ((y * WIDTH + x) * 4) as usize;
                assert_eq!(
                    &pixels[start..start + 4],
                    if (6..22).contains(&x) {
                        &[120, 45, 210, 255]
                    } else {
                        &[0, 0, 0, 0]
                    }
                );
            }
        }
        let restored: CustomLogo =
            serde_json::from_str(&serde_json::to_string(&logo).unwrap()).unwrap();
        let lines = restored.lines().unwrap();
        assert_eq!(lines.len(), 8);
        assert!(lines[0].contains("38;2;120;45;210"));
        assert_eq!(crate::onboarding::visible_width(&lines[0]), 28);
    }

    #[test]
    fn corrupt_preferences_cannot_inject_terminal_sequences() {
        assert!(CustomLogo {
            rgba: "\x1b[2J".into(),
            png: None,
        }
        .lines()
        .is_err());
        assert!(CustomLogo {
            rgba: STANDARD.encode([0; 4]),
            png: None,
        }
        .lines()
        .is_err());
        assert_eq!(
            image_path("\"image with spaces.png\""),
            Path::new("image with spaces.png")
        );
        assert_eq!(
            image_path("[Image: \"image with spaces.png\"] "),
            Path::new("image with spaces.png")
        );
        assert_eq!(image_path("\""), Path::new("\""));
    }
}
