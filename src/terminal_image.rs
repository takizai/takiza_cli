//! Terminal graphics live outside text rows: wrapping/copying never see protocol bytes.
use std::io::{IsTerminal, Write};
use std::ops::Range;
use std::sync::{Arc, Mutex};

use base64::{engine::general_purpose::STANDARD, Engine};
use crossterm::{cursor, queue, style::ResetColor};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backend {
    Art,
    Kitty,
    Iterm,
    Sixel,
}

fn detect(get: impl Fn(&str) -> Option<String>) -> Backend {
    // Explicit override also permits using a protocol through a configured multiplexer.
    if let Some(mode) = get("TAKIZA_LOGO_RENDER") {
        match mode.as_str() {
            "art" => return Backend::Art,
            "kitty" => return Backend::Kitty,
            "iterm" => return Backend::Iterm,
            "sixel" => return Backend::Sixel,
            _ => {}
        }
    }
    // Outer-terminal variables can survive inside multiplexers without working passthrough.
    if get("TMUX").is_some() || get("STY").is_some() {
        return Backend::Art;
    }
    let term = get("TERM").unwrap_or_default().to_lowercase();
    let program = get("TERM_PROGRAM").unwrap_or_default().to_lowercase();
    if term == "dumb" || term.starts_with("screen") || term.starts_with("tmux") {
        return Backend::Art;
    }
    if get("KITTY_WINDOW_ID").is_some()
        || term.contains("kitty")
        || term.contains("ghostty")
        || program == "ghostty"
    {
        return Backend::Kitty;
    }
    if program == "iterm.app" || program == "wezterm" || get("WEZTERM_PANE").is_some() {
        return Backend::Iterm;
    }
    if term.starts_with("foot")
        || term.starts_with("mlterm")
        || term.contains("sixel")
        || get("KONSOLE_VERSION")
            .and_then(|v| v.parse::<u32>().ok())
            .is_some_and(|v| v >= 220400)
    {
        return Backend::Sixel;
    }
    Backend::Art
}

pub fn backend() -> Backend {
    if !std::io::stdout().is_terminal() {
        return Backend::Art;
    }
    detect(|name| std::env::var(name).ok().filter(|value| !value.is_empty()))
}

// One owned image, never delete other applications' graphics.
const IMAGE_ID: u32 = 0x54414b;

#[derive(Clone, Debug, PartialEq, Eq)]
struct Placement {
    generation: u64,
    backend: Backend,
    y: u16,
    skip: usize,
    rows: usize,
    cell_width: u32,
    cell_height: u32,
}

impl Placement {
    fn for_view(
        generation: u64,
        backend: Backend,
        cell: (u32, u32),
        cols: u16,
        view: Range<usize>,
        logo_top: Option<usize>,
    ) -> Option<Self> {
        if backend == Backend::Art || cols < 30 {
            return None;
        }
        let top = logo_top?;
        let start = view.start.max(top);
        let end = view.end.min(top + crate::logo::LOGO_HEIGHT);
        if start >= end {
            return None;
        }
        Some(Self {
            generation,
            backend,
            y: (start - view.start) as u16,
            skip: start - top,
            rows: end - start,
            cell_width: cell.0,
            cell_height: cell.1,
        })
    }

    fn raster(&self, photo: &image::RgbaImage) -> image::RgbaImage {
        let width = crate::logo::LOGO_WIDTH as u32 * self.cell_width;
        let height = crate::logo::LOGO_HEIGHT as u32 * self.cell_height;
        let fit = image::DynamicImage::ImageRgba8(photo.clone())
            .resize(width, height, image::imageops::FilterType::Lanczos3)
            .to_rgba8();
        let mut canvas = image::RgbaImage::new(width, height);
        image::imageops::overlay(
            &mut canvas,
            &fit,
            ((width - fit.width()) / 2) as i64,
            ((height - fit.height()) / 2) as i64,
        );
        image::imageops::crop_imm(
            &canvas,
            0,
            self.skip as u32 * self.cell_height,
            width,
            self.rows as u32 * self.cell_height,
        )
        .to_image()
    }

    fn encode(&self, photo: &image::RgbaImage) -> std::io::Result<Vec<u8>> {
        let raster = self.raster(photo);
        if self.backend == Backend::Sixel {
            return Ok(sixel(&raster).into_bytes());
        }
        let mut png = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(raster)
            .write_to(&mut png, image::ImageFormat::Png)
            .map_err(std::io::Error::other)?;
        let bytes = png.into_inner();
        let payload = STANDARD.encode(&bytes);
        if self.backend == Backend::Iterm {
            return Ok(format!("\x1b]1337;File=inline=1;size={};width=28;height={};preserveAspectRatio=1;doNotMoveCursor=1:{}\x07",
                bytes.len(), self.rows, payload).into_bytes());
        }
        let mut output = String::new();
        let chunks = payload.as_bytes().chunks(4096).collect::<Vec<_>>();
        for (index, chunk) in chunks.iter().enumerate() {
            let header = if index == 0 {
                format!(
                    "a=T,f=100,t=d,i={IMAGE_ID},p=1,q=2,C=1,c=28,r={},",
                    self.rows
                )
            } else {
                "q=2,".into()
            };
            output.push_str(&format!(
                "\x1b_G{header}m={};{}\x1b\\",
                u8::from(index + 1 < chunks.len()),
                std::str::from_utf8(chunk).unwrap()
            ));
        }
        Ok(output.into_bytes())
    }
}

struct Drawn {
    placement: Placement,
    encoded: Arc<Vec<u8>>,
}
static DRAWN: Mutex<Option<Drawn>> = Mutex::new(None);

/// Clear stale placements before the caller paints text, then overlay the new image afterwards.
pub struct Frame {
    pub repaint_text: bool,
    placement: Option<Placement>,
    encoded: Option<Arc<Vec<u8>>>,
}

impl Frame {
    pub fn begin(
        out: &mut impl Write,
        cols: u16,
        term_rows: u16,
        view: Range<usize>,
        logo_top: Option<usize>,
    ) -> std::io::Result<Self> {
        let photo = crate::custom_logo::current_photo();
        let cell = crossterm::terminal::window_size()
            .ok()
            .and_then(|size| {
                (size.columns > 0 && size.rows > 0 && size.width > 0 && size.height > 0).then(
                    || {
                        (
                            (size.width as u32 / size.columns as u32).clamp(1, 64),
                            (size.height as u32 / size.rows as u32).clamp(1, 128),
                        )
                    },
                )
            })
            .unwrap_or((8, 16));
        let placement = photo.as_ref().and_then(|(generation, _)| {
            Placement::for_view(*generation, backend(), cell, cols, view.clone(), logo_top)
        });
        let mut drawn = DRAWN.lock().unwrap();
        let changed = drawn.as_ref().map(|drawn| &drawn.placement) != placement.as_ref();
        if changed {
            if let Some(old) = drawn.take() {
                erase(out, &old.placement, cols, term_rows.min(view.len() as u16))?;
            }
            if let (Some(placement), Some((_, photo))) = (&placement, photo) {
                *drawn = Some(Drawn {
                    placement: placement.clone(),
                    encoded: Arc::new(placement.encode(&photo)?),
                });
            }
        }
        Ok(Self {
            repaint_text: changed,
            placement,
            encoded: drawn.as_ref().map(|drawn| Arc::clone(&drawn.encoded)),
        })
    }

    pub fn paint(&self, out: &mut impl Write, dirty: bool) -> std::io::Result<()> {
        if !dirty && !self.repaint_text {
            return Ok(());
        }
        if let (Some(placement), Some(encoded)) = (&self.placement, &self.encoded) {
            queue!(
                out,
                cursor::SavePosition,
                cursor::MoveTo(1, placement.y),
                ResetColor
            )?;
            out.write_all(encoded)?;
            queue!(out, cursor::RestorePosition)?;
        }
        Ok(())
    }

    pub fn overlaps(&self, row: usize) -> bool {
        self.placement
            .as_ref()
            .is_some_and(|p| row >= p.y as usize && row < p.y as usize + p.rows)
    }
}

fn erase(out: &mut impl Write, placement: &Placement, cols: u16, rows: u16) -> std::io::Result<()> {
    if placement.backend == Backend::Kitty {
        write!(out, "\x1b_Ga=d,d=I,i={IMAGE_ID},q=2\x1b\\")?;
    } else {
        queue!(out, cursor::SavePosition, ResetColor)?;
        for row in placement.y..(placement.y.saturating_add(placement.rows as u16)).min(rows) {
            queue!(out, cursor::MoveTo(1, row))?;
            write!(
                out,
                "{}",
                " ".repeat(28.min(cols.saturating_sub(2) as usize))
            )?;
        }
        queue!(out, cursor::RestorePosition)?;
    }
    Ok(())
}

pub fn clear(out: &mut impl Write) -> std::io::Result<()> {
    if let Some(drawn) = DRAWN.lock().unwrap().take() {
        let (cols, rows) = crossterm::terminal::size().unwrap_or((80, 24));
        erase(out, &drawn.placement, cols, rows)?;
    }
    Ok(())
}

/// Decorative text updates can erase SIXEL/iTerm images even if the transcript stays put.
pub fn repaint_current(out: &mut impl Write, changed_rows: &[usize]) -> std::io::Result<()> {
    let drawn = DRAWN.lock().unwrap();
    if let Some(drawn) = drawn.as_ref().filter(|drawn| {
        changed_rows.iter().any(|&row| {
            row >= drawn.placement.y as usize
                && row < drawn.placement.y as usize + drawn.placement.rows
        })
    }) {
        queue!(
            out,
            cursor::SavePosition,
            cursor::MoveTo(1, drawn.placement.y),
            ResetColor
        )?;
        out.write_all(&drawn.encoded)?;
        queue!(out, cursor::RestorePosition)?;
    }
    Ok(())
}

// DEC SIXEL: 216 RGB colors; transparent pixels leave the terminal background visible.
fn sixel(image: &image::RgbaImage) -> String {
    let mut output = format!("\x1bP0;1;0q\"1;1;{};{}", image.width(), image.height());
    for index in 0..216 {
        output.push_str(&format!(
            "#{index};2;{};{};{}",
            (index / 36) * 20,
            ((index / 6) % 6) * 20,
            (index % 6) * 20
        ));
    }
    for y in (0..image.height()).step_by(6) {
        let mut planes = vec![vec![0u8; image.width() as usize]; 216];
        for dy in 0..6.min(image.height() - y) {
            for x in 0..image.width() {
                let pixel = image.get_pixel(x, y + dy).0;
                if pixel[3] == 0 {
                    continue;
                }
                let q = |c: u8| ((c as u32 * pixel[3] as u32 * 5 + 32512) / 65025) as usize;
                let index = q(pixel[0]) * 36 + q(pixel[1]) * 6 + q(pixel[2]);
                planes[index][x as usize] |= 1 << dy;
            }
        }
        for (index, plane) in planes.iter().enumerate() {
            let Some(last) = plane.iter().rposition(|&v| v != 0) else {
                continue;
            };
            output.push_str(&format!("#{index}"));
            let mut x = 0;
            while x <= last {
                let value = plane[x];
                let count = plane[x..=last].iter().take_while(|&&v| v == value).count();
                if count > 3 {
                    output.push_str(&format!("!{count}{}", (value + 63) as char));
                } else {
                    output.extend(std::iter::repeat_n((value + 63) as char, count));
                }
                x += count;
            }
            output.push('$');
        }
        if y + 6 < image.height() {
            output.push('-');
        }
    }
    output.push_str("\x1b\\");
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    fn detected(values: &[(&str, &str)]) -> Backend {
        detect(|key| {
            values
                .iter()
                .find(|(name, _)| *name == key)
                .map(|(_, value)| value.to_string())
        })
    }

    #[test]
    fn auto_selects_known_protocols_and_keeps_unknown_or_multiplexed_terminals_safe() {
        for (values, expected) in [
            (vec![("TERM", "xterm-kitty")], Backend::Kitty),
            (vec![("TERM", "xterm-ghostty")], Backend::Kitty),
            (vec![("TERM_PROGRAM", "iTerm.app")], Backend::Iterm),
            (vec![("TERM_PROGRAM", "WezTerm")], Backend::Iterm),
            (vec![("TERM", "foot")], Backend::Sixel),
            (vec![("KONSOLE_VERSION", "240800")], Backend::Sixel),
            (vec![("KONSOLE_VERSION", "210000")], Backend::Art),
            (
                vec![("TERM", "xterm-256color"), ("VTE_VERSION", "8401")],
                Backend::Art,
            ),
            (vec![("TERM_PROGRAM", "vscode")], Backend::Art),
            (
                vec![("TERM", "dumb"), ("KITTY_WINDOW_ID", "1")],
                Backend::Art,
            ),
            (
                vec![("TERM", "xterm-kitty"), ("TMUX", "session")],
                Backend::Art,
            ),
            (
                vec![("TERM_PROGRAM", "iTerm.app"), ("STY", "session")],
                Backend::Art,
            ),
            (
                vec![("TAKIZA_LOGO_RENDER", "art"), ("TERM", "xterm-kitty")],
                Backend::Art,
            ),
            (vec![("TAKIZA_LOGO_RENDER", "sixel")], Backend::Sixel),
            (vec![], Backend::Art),
        ] {
            assert_eq!(detected(&values), expected, "{values:?}");
        }
    }

    #[test]
    fn scrolled_logo_is_cropped_and_never_crosses_menu_or_input_boundary() {
        let top = Placement::for_view(1, Backend::Kitty, (8, 16), 80, 0..5, Some(1)).unwrap();
        assert_eq!((top.y, top.skip, top.rows), (1, 0, 4));
        let clipped = Placement::for_view(1, Backend::Kitty, (8, 16), 80, 4..12, Some(1)).unwrap();
        assert_eq!((clipped.y, clipped.skip, clipped.rows), (0, 3, 5));
        let raster = clipped.raster(&image::RgbaImage::from_pixel(
            320,
            320,
            image::Rgba([120, 45, 210, 255]),
        ));
        assert_eq!(raster.dimensions(), (224, 80));
        assert_eq!(raster.get_pixel(0, 0).0, [0, 0, 0, 0]);
        assert_eq!(raster.get_pixel(100, 0).0, [120, 45, 210, 255]);
        assert!(Placement::for_view(1, Backend::Kitty, (8, 16), 80, 9..20, Some(1)).is_none());
        assert!(Placement::for_view(1, Backend::Kitty, (8, 16), 80, 0..20, None).is_none());
    }

    #[test]
    fn kitty_chunks_roundtrip_png_and_suppress_input_responses() {
        let photo = image::RgbaImage::from_fn(560, 320, |x, y| {
            image::Rgba([
                (x * 13 + y * 17) as u8,
                (x * 31 + y * 7) as u8,
                (x * y) as u8,
                255,
            ])
        });
        let placement =
            Placement::for_view(1, Backend::Kitty, (8, 16), 80, 0..20, Some(1)).unwrap();
        let encoded = String::from_utf8(placement.encode(&photo).unwrap()).unwrap();
        let commands = encoded
            .split("\x1b\\")
            .filter(|v| !v.is_empty())
            .collect::<Vec<_>>();
        assert!(commands.len() > 1);
        let mut payload = String::new();
        for command in &commands {
            let (header, data) = command.split_once(';').unwrap();
            assert!(header.contains("q=2"));
            assert!(data.len() <= 4096 && data.len() % 4 == 0);
            payload.push_str(data);
        }
        assert!(commands[0].contains("C=1,c=28,r=8"));
        assert!(commands.last().unwrap().contains("m=0;"));
        let image = image::load_from_memory(&STANDARD.decode(payload).unwrap()).unwrap();
        assert_eq!((image.width(), image.height()), (224, 128));
    }

    #[test]
    fn iterm_uses_inline_png_and_sixel_encodes_pixel_bands() {
        let photo = image::RgbaImage::from_pixel(2, 2, image::Rgba([255, 0, 0, 255]));
        let p = Placement::for_view(1, Backend::Iterm, (8, 16), 80, 0..20, Some(1)).unwrap();
        let encoded = String::from_utf8(p.encode(&photo).unwrap()).unwrap();
        assert!(encoded.starts_with("\x1b]1337;File=inline=1;size="));
        assert!(encoded.contains("width=28;height=8;preserveAspectRatio=1"));
        assert!(encoded.ends_with('\x07'));
        let png = STANDARD
            .decode(encoded.split_once(':').unwrap().1.trim_end_matches('\x07'))
            .unwrap();
        assert!(image::load_from_memory(&png).is_ok());
        let sixel = sixel(&photo);
        assert!(sixel.starts_with("\x1bP0;1;0q\"1;1;2;2"));
        assert!(sixel.contains("#180;2;100;0;0"));
        assert!(sixel.ends_with("#180BB$\x1b\\"));
        let mut erased = Vec::new();
        erase(
            &mut erased,
            &Placement {
                backend: Backend::Kitty,
                ..p
            },
            80,
            24,
        )
        .unwrap();
        assert_eq!(
            String::from_utf8(erased).unwrap(),
            format!("\x1b_Ga=d,d=I,i={IMAGE_ID},q=2\x1b\\")
        );
    }
}
