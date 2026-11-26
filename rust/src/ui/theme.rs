//! The glass.css design tokens as colours, for dark and light themes.

use super::gfx::Rgba;
use crate::model::Level;

#[derive(Clone, Copy)]
pub struct Theme {
    pub light: bool,
    pub fg: Rgba,
    pub sub: Rgba,
    pub faint: Rgba,
    pub track: Rgba,
    pub line: Rgba,
    pub chip: Rgba,
    pub tint_a: Rgba,
    pub tint_b: Rgba,
    pub sheen: Rgba,
    pub ok: (Rgba, Rgba, Rgba),
    pub warn: (Rgba, Rgba, Rgba),
    pub crit: (Rgba, Rgba, Rgba),
    pub brand: Rgba,
    pub body: Rgba,
}

/// Over the live acrylic the tint can be lighter than over the old pre-blurred screenshot.
const TINT_OVER_ACRYLIC: f32 = 0.55;

pub fn dark() -> Theme {
    let fgc = Rgba(235.0 / 255.0, 238.0 / 255.0, 245.0 / 255.0, 1.0);
    Theme {
        light: false,
        fg: Rgba::hex(0xf4f5f8, 1.0),
        sub: fgc.alpha(0.62),
        faint: fgc.alpha(0.38),
        track: Rgba(1.0, 1.0, 1.0, 0.10),
        line: Rgba(1.0, 1.0, 1.0, 0.09),
        chip: Rgba(1.0, 1.0, 1.0, 0.08),
        tint_a: Rgba::hex(0x202232, 0.64 * TINT_OVER_ACRYLIC),
        tint_b: Rgba::hex(0x0e0f16, 0.76 * TINT_OVER_ACRYLIC),
        sheen: Rgba(1.0, 1.0, 1.0, 0.07),
        ok: (Rgba::hex(0x5eead4, 1.0), Rgba::hex(0x4ade80, 1.0), Rgba::hex(0x5eead4, 0.55)),
        warn: (Rgba::hex(0xfde047, 1.0), Rgba::hex(0xf59e0b, 1.0), Rgba::hex(0xf59e0b, 0.55)),
        crit: (Rgba::hex(0xfb7185, 1.0), Rgba::hex(0xef4444, 1.0), Rgba::hex(0xf43f5e, 0.6)),
        brand: Rgba::hex(0xe8743b, 1.0),
        body: Rgba::hex(0x1b1c22, 1.0),
    }
}

pub fn light() -> Theme {
    let fgc = Rgba::hex(0x16171c, 1.0);
    let ink = Rgba::hex(0x0a1428, 1.0);
    Theme {
        light: true,
        fg: fgc,
        sub: fgc.alpha(0.62),
        faint: fgc.alpha(0.40),
        track: ink.alpha(0.09),
        line: ink.alpha(0.08),
        chip: ink.alpha(0.05),
        tint_a: Rgba(1.0, 1.0, 1.0, 0.80 * TINT_OVER_ACRYLIC),
        tint_b: Rgba::hex(0xf0f2f8, 0.88 * TINT_OVER_ACRYLIC),
        sheen: Rgba(1.0, 1.0, 1.0, 0.6),
        ok: (Rgba::hex(0x0d9488, 1.0), Rgba::hex(0x16a34a, 1.0), Rgba::hex(0x0d9488, 0.35)),
        warn: (Rgba::hex(0xd97706, 1.0), Rgba::hex(0xb45309, 1.0), Rgba::hex(0xd97706, 0.35)),
        crit: (Rgba::hex(0xe11d48, 1.0), Rgba::hex(0xb91c1c, 1.0), Rgba::hex(0xe11d48, 0.35)),
        brand: Rgba::hex(0xe8743b, 1.0),
        body: Rgba::hex(0xeef0f5, 1.0),
    }
}

impl Theme {
    /// (gradient start, gradient end, glow) for a usage level.
    pub fn level(&self, l: Level) -> (Rgba, Rgba, Rgba) {
        match l {
            Level::Ok => self.ok,
            Level::Warn => self.warn,
            Level::Crit => self.crit,
        }
    }
}
