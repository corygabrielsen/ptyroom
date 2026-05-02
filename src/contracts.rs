//! Per-scene contract registry.
//!
//! Adding a scene? Add an entry to [`registry`].

use crate::color::HexColor;
use crate::verify::{
    Check, Contract, bg_reaches, final_bg_is, picker_scroll_indicator_visible,
};

const fn rgb(r: u8, g: u8, b: u8) -> HexColor { HexColor::from_rgb(r, g, b) }

#[must_use] 
pub fn registry(scene: &str) -> Option<Contract> {
    match scene {
        "demo_full" => Some(demo_full()),
        "smoke" => Some(smoke()),
        _ => None,
    }
}

fn demo_full() -> Contract {
    let checks: Vec<Check> = vec![
        picker_scroll_indicator_visible(),
        bg_reaches("dark-orange",       rgb(0x78, 0x59, 0x3a)),
        bg_reaches("dracula",           rgb(0x28, 0x2a, 0x36)),
        bg_reaches("solarized-light",   rgb(0xfd, 0xf6, 0xe3)),
        bg_reaches("blue-cd-hook",      rgb(0x53, 0x53, 0xac)),
        bg_reaches("pale-rose-cd-hook", rgb(0xde, 0xba, 0xcc)),
        bg_reaches("hot-custom",        rgb(0xff, 0x00, 0x6e)),
        final_bg_is("hot-custom",       rgb(0xff, 0x00, 0x6e)),
    ];
    Contract { scene: "demo_full", checks }
}

fn smoke() -> Contract {
    Contract { scene: "smoke", checks: vec![picker_scroll_indicator_visible()] }
}
