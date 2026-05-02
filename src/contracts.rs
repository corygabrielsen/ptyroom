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
const PALE_BLUE:        HexColor = rgb(0xba, 0xba, 0xde);
const PALE_YELLOW:      HexColor = rgb(0xde, 0xde, 0xba);
const MATRIX:           HexColor = rgb(0x00, 0x11, 0x00);
/// Snapshot bg after `tint reset` — matches the `recorder/snapshot.ts`
/// startup default (`#1a1b26`). Used by `demo_full`'s act-5 reset check.
const DEFAULT_BG:       HexColor = rgb(0x1a, 0x1b, 0x26);

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
        bg_reaches("pale-blue-cd-hook",    PALE_BLUE),
        bg_reaches("pale-yellow-cd-hook",  PALE_YELLOW),
        bg_reaches("matrix-custom",        MATRIX),
        // Act 5: `tint reset` returns to the snapshot's default bg.
        // Matches the loop's start state — graceful wrap-around.
        final_bg_is("reset-default",       DEFAULT_BG),
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
            bg_reaches("pale-blue",    PALE_BLUE),
            bg_reaches("pale-yellow",  PALE_YELLOW),
            final_bg_is("pale-yellow", PALE_YELLOW),
        ],
    }
}

fn custom_theme() -> Contract {
    Contract {
        scene: "custom_theme",
        checks: vec![
            bg_reaches("matrix",  MATRIX),
            final_bg_is("matrix", MATRIX),
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
