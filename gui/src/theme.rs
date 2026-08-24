use gpui::{Hsla, rgb};

pub fn background() -> Hsla {
    rgb(0x0b0d12).into()
}
pub fn surface() -> Hsla {
    rgb(0x121620).into()
}
pub fn surface_hover() -> Hsla {
    rgb(0x181e2a).into()
}
pub fn border() -> Hsla {
    rgb(0x252c3a).into()
}
pub fn text() -> Hsla {
    rgb(0xe8ebf2).into()
}
pub fn muted() -> Hsla {
    rgb(0x858da0).into()
}
pub fn accent() -> Hsla {
    rgb(0x9b8cff).into()
}
pub fn ready() -> Hsla {
    rgb(0x71d9a6).into()
}
pub fn blocked() -> Hsla {
    rgb(0xe6ad63).into()
}
pub fn progress() -> Hsla {
    rgb(0x70a5ff).into()
}
pub fn danger() -> Hsla {
    rgb(0xff7188).into()
}

pub fn priority(priority: u8) -> Hsla {
    match priority {
        0 => rgb(0xff647c).into(),
        1 => rgb(0xffa45c).into(),
        2 => rgb(0xe4c76a).into(),
        3 => rgb(0x68c7c1).into(),
        _ => rgb(0x778198).into(),
    }
}
