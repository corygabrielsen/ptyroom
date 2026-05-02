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

const DARK_ORANGE:      HexColor = rgb(0x78, 0x59, 0x3a);
const DRACULA:          HexColor = rgb(0x28, 0x2a, 0x36);
const SOLARIZED_LIGHT:  HexColor = rgb(0xfd, 0xf6, 0xe3);
const BLUE:             HexColor = rgb(0x53, 0x53, 0xac);
const PALE_ROSE:        HexColor = rgb(0xde, 0xba, 0xcc);
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
        bg_reaches("dark-orange",       DARK_ORANGE),
        bg_reaches("dracula",           DRACULA),
        bg_reaches("solarized-light",   SOLARIZED_LIGHT),
        bg_reaches("blue-cd-hook",      BLUE),
        bg_reaches("pale-rose-cd-hook", PALE_ROSE),
        bg_reaches("hot-custom",        HOT),
        final_bg_is("hot-custom",       HOT),
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
            bg_reaches("dark-orange", DARK_ORANGE),
            final_bg_is("dark-orange", DARK_ORANGE),
        ],
    }
}

fn cli() -> Contract {
    Contract {
        scene: "cli",
        checks: vec![
            bg_reaches("dracula",         DRACULA),
            bg_reaches("solarized-light", SOLARIZED_LIGHT),
            final_bg_is("solarized-light", SOLARIZED_LIGHT),
        ],
    }
}

fn cd_hook() -> Contract {
    Contract {
        scene: "cd_hook",
        checks: vec![
            bg_reaches("blue",      BLUE),
            bg_reaches("pale-rose", PALE_ROSE),
            final_bg_is("pale-rose", PALE_ROSE),
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
