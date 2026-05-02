//! Per-scene contract registry.
//!
//! Adding a scene? Add an entry to [`registry`] *and* to [`SCENES`]. The
//! lib test `every_scene_has_a_contract` keeps them in sync.

use crate::color::HexColor;
use crate::verify::{
    Check, Contract, bg_reaches, final_bg_is, picker_scroll_indicator_visible,
};

/// Every scene name the registry knows about. Source of truth for any
/// "iterate over all scenes" workflow (Makefile `verify-all`, regression
/// loops, `tint-verify --list-scenes`, etc.).
pub const SCENES: &[&str] = &[
    "demo_full", "smoke",
    "picker", "cli", "cd_hook", "custom_theme",
];

const fn rgb(r: u8, g: u8, b: u8) -> HexColor { HexColor::from_rgb(r, g, b) }

const DARK_AZURE:       HexColor = rgb(0x3a, 0x59, 0x78);
const DRACULA:          HexColor = rgb(0x28, 0x2a, 0x36);
const SOLARIZED_LIGHT:  HexColor = rgb(0xfd, 0xf6, 0xe3);
const MONOKAI:          HexColor = rgb(0x27, 0x28, 0x22);
const PALE_EMERALD:     HexColor = rgb(0xba, 0xde, 0xc3);
const PALE_AMBER:       HexColor = rgb(0xde, 0xd5, 0xba);
const HOT:              HexColor = rgb(0xff, 0x00, 0x6e);

#[must_use]
pub fn registry(scene: &str) -> Option<Contract> {
    match scene {
        "demo_full"    => Some(demo_full()),
        "smoke"        => Some(smoke()),
        "picker"       => Some(picker()),
        "cli"          => Some(cli()),
        "cd_hook"      => Some(cd_hook()),
        "custom_theme" => Some(custom_theme()),
        _              => None,
    }
}

fn demo_full() -> Contract {
    let checks: Vec<Check> = vec![
        picker_scroll_indicator_visible(),
        bg_reaches("dark-azure",           DARK_AZURE),
        bg_reaches("dracula",              DRACULA),
        bg_reaches("solarized-light",      SOLARIZED_LIGHT),
        bg_reaches("monokai",              MONOKAI),
        bg_reaches("pale-emerald-cd-hook", PALE_EMERALD),
        bg_reaches("pale-amber-cd-hook",   PALE_AMBER),
        bg_reaches("hot-custom",           HOT),
        final_bg_is("hot-custom",          HOT),
    ];
    Contract { scene: "demo_full", checks }
}

fn smoke() -> Contract {
    Contract { scene: "smoke", checks: vec![picker_scroll_indicator_visible()] }
}

fn picker() -> Contract {
    Contract {
        scene: "picker",
        checks: vec![
            picker_scroll_indicator_visible(),
            bg_reaches("dark-azure", DARK_AZURE),
            final_bg_is("dark-azure", DARK_AZURE),
        ],
    }
}

fn cli() -> Contract {
    Contract {
        scene: "cli",
        checks: vec![
            bg_reaches("dracula",         DRACULA),
            bg_reaches("solarized-light", SOLARIZED_LIGHT),
            bg_reaches("monokai",         MONOKAI),
            final_bg_is("monokai",        MONOKAI),
        ],
    }
}

fn cd_hook() -> Contract {
    Contract {
        scene: "cd_hook",
        checks: vec![
            bg_reaches("pale-emerald",  PALE_EMERALD),
            bg_reaches("pale-amber",    PALE_AMBER),
            final_bg_is("pale-amber",   PALE_AMBER),
        ],
    }
}

fn custom_theme() -> Contract {
    Contract {
        scene: "custom_theme",
        checks: vec![
            bg_reaches("hot",  HOT),
            final_bg_is("hot", HOT),
        ],
    }
}

/// Open contract: scene exists but has no validation checks. Use for
/// exploratory scenes whose endpoints aren't predictable (random themes,
/// `tint -l` based pickers, mood loops). The render pipeline still runs
/// to completion; verify reports zero failures.
#[must_use]
pub fn open_contract(scene: &'static str) -> Contract {
    Contract { scene, checks: vec![] }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_scene_has_a_contract() {
        for name in SCENES {
            assert!(
                registry(name).is_some(),
                "SCENES lists {name:?} but registry returns None — \
                 add the match arm or remove the entry"
            );
        }
    }
}
