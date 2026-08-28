use std::sync::{OnceLock, RwLock};

use gpui::{Hsla, rgb};

#[derive(Clone, Copy, PartialEq)]
struct Palette {
    background: u32,
    surface: u32,
    surface_hover: u32,
    raised: u32,
    raised_hover: u32,
    border: u32,
    text: u32,
    muted: u32,
    accent: u32,
    ready: u32,
    blocked: u32,
    progress: u32,
    danger: u32,
    priorities: [u32; 5],
}

const DEFAULT: Palette = Palette {
    background: 0x0b0d12,
    surface: 0x121620,
    surface_hover: 0x181e2a,
    raised: 0x1a2132,
    raised_hover: 0x242d40,
    border: 0x252c3a,
    text: 0xe8ebf2,
    muted: 0x858da0,
    accent: 0x9b8cff,
    ready: 0x71d9a6,
    blocked: 0xe6ad63,
    progress: 0x70a5ff,
    danger: 0xff7188,
    priorities: [0xff647c, 0xffa45c, 0xe4c76a, 0x68c7c1, 0x778198],
};

static PALETTE: OnceLock<RwLock<Palette>> = OnceLock::new();

fn palette() -> &'static RwLock<Palette> {
    PALETTE.get_or_init(|| RwLock::new(omarchy_palette().unwrap_or(DEFAULT)))
}

/// Re-read Omarchy's materialized palette. Returns true when it changed.
/// The dashboard polls this alongside its existing background refresh.
pub fn refresh() -> bool {
    let Some(next) = omarchy_palette() else {
        return false;
    };
    let mut current = palette().write().unwrap_or_else(|error| error.into_inner());
    if *current == next {
        return false;
    }
    *current = next;
    true
}

fn color(select: impl FnOnce(&Palette) -> u32) -> Hsla {
    let palette = palette().read().unwrap_or_else(|error| error.into_inner());
    rgb(select(&palette)).into()
}

// Omarchy materializes the active theme here. Checking the installation as
// well as the file keeps a coincidentally similar path from changing the app
// on other Linux distributions; non-Linux builds retain the built-in theme.
#[cfg(target_os = "linux")]
fn omarchy_palette() -> Option<Palette> {
    use std::{env, fs, path::Path};

    if !Path::new("/usr/share/omarchy").is_dir() {
        return None;
    }
    let home = env::var_os("HOME")?;
    let colors =
        fs::read_to_string(Path::new(&home).join(".local/state/omarchy/current/theme/colors.toml"))
            .ok()?;
    palette_from_omarchy_colors(&colors)
}

#[cfg(not(target_os = "linux"))]
fn omarchy_palette() -> Option<Palette> {
    None
}

fn palette_from_omarchy_colors(input: &str) -> Option<Palette> {
    use std::collections::HashMap;

    let colors: HashMap<&str, u32> = input
        .lines()
        .filter_map(|line| {
            let (key, value) = line.split_once('=')?;
            let key = key.trim();
            if key.is_empty() || key.starts_with('#') {
                return None;
            }
            let value = value
                .split('#')
                .nth(1)?
                .get(..6)
                .filter(|hex| hex.bytes().all(|byte| byte.is_ascii_hexdigit()))?;
            Some((key, u32::from_str_radix(value, 16).ok()?))
        })
        .collect();
    let get = |names: &[&str]| names.iter().find_map(|name| colors.get(name).copied());

    // background and foreground are the only required semantic colors. New
    // Omarchy themes always provide them; aliases keep older themes working.
    let background = get(&["background", "bg", "color0"])?;
    let text = get(&["foreground", "fg", "color7"])?;
    let muted = get(&["muted", "dark_foreground", "dark_fg", "color8"])
        .unwrap_or_else(|| mix(background, text, 0.48));
    let accent = get(&["accent", "blue", "color4"]).unwrap_or(DEFAULT.accent);
    let green = get(&["green", "color2"]).unwrap_or(DEFAULT.ready);
    let yellow = get(&["yellow", "color3"]).unwrap_or(DEFAULT.blocked);
    let red = get(&["red", "color1"]).unwrap_or(DEFAULT.danger);
    let orange = get(&["orange"]).unwrap_or(yellow);
    let cyan = get(&["cyan", "color6"]).unwrap_or(accent);

    Some(Palette {
        background,
        // Theme-provided "lighter_background" and "selection" colors can be
        // intentionally high-contrast. Derive UI layers from the base colors
        // instead so cards stay subtle in both dark and light themes.
        surface: mix(background, text, 0.045),
        surface_hover: mix(background, text, 0.09),
        raised: mix(background, text, 0.075),
        raised_hover: mix(background, text, 0.13),
        border: mix(background, text, 0.14),
        text,
        muted,
        accent,
        ready: green,
        blocked: yellow,
        progress: get(&["blue", "color4"]).unwrap_or(accent),
        danger: red,
        priorities: [red, orange, yellow, cyan, muted],
    })
}

fn mix(start: u32, end: u32, amount: f32) -> u32 {
    let channel = |shift: u32| {
        let start = ((start >> shift) & 0xffu32) as f32;
        let end = ((end >> shift) & 0xffu32) as f32;
        (start + (end - start) * amount).round() as u32
    };
    (channel(16) << 16) | (channel(8) << 8) | channel(0)
}

pub fn background() -> Hsla {
    color(|palette| palette.background)
}
pub fn surface() -> Hsla {
    color(|palette| palette.surface)
}
pub fn surface_hover() -> Hsla {
    color(|palette| palette.surface_hover)
}
// The deck console sits on this so it reads as its own layer above the
// epic grid rather than blending into the card surfaces.
pub fn raised() -> Hsla {
    color(|palette| palette.raised)
}
pub fn raised_hover() -> Hsla {
    color(|palette| palette.raised_hover)
}
pub fn border() -> Hsla {
    color(|palette| palette.border)
}
pub fn text() -> Hsla {
    color(|palette| palette.text)
}
pub fn muted() -> Hsla {
    color(|palette| palette.muted)
}
pub fn accent() -> Hsla {
    color(|palette| palette.accent)
}
pub fn ready() -> Hsla {
    color(|palette| palette.ready)
}
pub fn blocked() -> Hsla {
    color(|palette| palette.blocked)
}
pub fn progress() -> Hsla {
    color(|palette| palette.progress)
}
pub fn danger() -> Hsla {
    color(|palette| palette.danger)
}
pub fn star() -> Hsla {
    rgb(0xf2c94c).into()
}

pub fn priority(priority: u8) -> Hsla {
    color(|palette| palette.priorities[usize::from(priority.min(4))])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_an_omarchy_semantic_palette() {
        let palette = palette_from_omarchy_colors(
            r##"
                mode = "dark"
                background = "#2c2525"
                lighter_background = "#3d2f2a"
                selection = "#403e41"
                foreground = "#e6d9db"
                muted = "#72696a"
                accent = "#f38d70"
                red = "#fd6883"
                yellow = "#f9cc6c"
                orange = "#fb9a77"
                green = "#adda78"
                cyan = "#85dacc"
                blue = "#f38d70"
            "##,
        )
        .unwrap();

        assert_eq!(palette.background, 0x2c2525);
        assert_eq!(palette.surface, 0x342d2d);
        assert_eq!(palette.text, 0xe6d9db);
        assert_eq!(palette.accent, 0xf38d70);
        assert_eq!(
            palette.priorities,
            [0xfd6883, 0xfb9a77, 0xf9cc6c, 0x85dacc, 0x72696a]
        );
    }

    #[test]
    fn accepts_legacy_palette_aliases() {
        let palette = palette_from_omarchy_colors(
            "bg = '#101112'\nfg = '#eeeeee'\ncolor1 = '#ff0000'\ncolor2 = '#00ff00'\n",
        )
        .unwrap();

        assert_eq!(palette.background, 0x101112);
        assert_eq!(palette.text, 0xeeeeee);
        assert_eq!(palette.danger, 0xff0000);
        assert_eq!(palette.ready, 0x00ff00);
    }
}
