use alacritty_terminal::term::color::{COUNT, Colors};
use alacritty_terminal::vte::ansi::{Color, NamedColor, Rgb};
use gpui_kit::{Hsla, Rgba};

const ANSI: [u32; 16] = [
    0x1e1e1e, 0xf44747, 0x6a9955, 0xd7ba7d, 0x569cd6, 0xc586c0, 0x4ec9b0, 0xd4d4d4, //
    0x808080, 0xf14c4c, 0x23d18b, 0xf5f543, 0x3b8eea, 0xd670d6, 0x29b8db, 0xffffff,
];
const FOREGROUND: u32 = 0xcccccc;
const BACKGROUND: u32 = 0x181818;
const CURSOR: u32 = 0xaeafad;

/// Every color a cell can reference, indexed the same way as alacritty's `Colors`.
pub struct Palette {
    colors: [Rgb; COUNT],
}

impl Default for Palette {
    fn default() -> Self {
        let mut colors = [Rgb::default(); COUNT];

        for (index, hex) in ANSI.iter().enumerate() {
            colors[index] = rgb(*hex);
        }

        let levels = [0, 95, 135, 175, 215, 255];
        for index in 0..216 {
            colors[16 + index] = Rgb {
                r: levels[index / 36],
                g: levels[(index / 6) % 6],
                b: levels[index % 6],
            };
        }

        for index in 0..24 {
            let level = 8 + 10 * index as u8;
            colors[232 + index] = Rgb { r: level, g: level, b: level };
        }

        colors[NamedColor::Foreground as usize] = rgb(FOREGROUND);
        colors[NamedColor::Background as usize] = rgb(BACKGROUND);
        colors[NamedColor::Cursor as usize] = rgb(CURSOR);
        colors[NamedColor::BrightForeground as usize] = rgb(0xffffff);
        colors[NamedColor::DimForeground as usize] = dim(rgb(FOREGROUND));
        for index in 0..8 {
            colors[NamedColor::DimBlack as usize + index] = dim(colors[index]);
        }

        Self { colors }
    }
}

impl Palette {
    /// Looks up an indexed color, preferring any override the program set with OSC 4/10/11/12.
    pub fn rgb(&self, index: usize, overrides: &Colors) -> Rgb {
        overrides[index].unwrap_or(self.colors[index])
    }

    pub fn resolve(&self, color: Color, overrides: &Colors) -> Hsla {
        let rgb = match color {
            Color::Spec(rgb) => rgb,
            Color::Named(named) => self.rgb(named as usize, overrides),
            Color::Indexed(index) => self.rgb(index as usize, overrides),
        };
        to_hsla(rgb)
    }

    pub fn named(&self, color: NamedColor, overrides: &Colors) -> Hsla {
        self.resolve(Color::Named(color), overrides)
    }
}

pub fn to_hsla(rgb: Rgb) -> Hsla {
    Rgba {
        r: rgb.r as f32 / 255.,
        g: rgb.g as f32 / 255.,
        b: rgb.b as f32 / 255.,
        a: 1.,
    }
    .into()
}

fn rgb(hex: u32) -> Rgb {
    Rgb {
        r: (hex >> 16) as u8,
        g: (hex >> 8) as u8,
        b: hex as u8,
    }
}

fn dim(color: Rgb) -> Rgb {
    color * 0.66
}
