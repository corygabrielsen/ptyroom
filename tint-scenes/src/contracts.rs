//! Per-scene contract registry.
//!
//! Adding a scene? Add an entry to [`registry`] *and* to [`SCENES`]. The
//! lib test `every_scene_has_a_contract` keeps them in sync.

use term_recorder::color::HexColor;
use term_recorder::verify::{
    Check, CheckResult, Contract, bg_reaches, final_bg_is, no_row_contains,
};

/// Tint-specific check: passes iff some snapshot has a row containing
/// both `↓` and `more` (the picker's "↓ N more" overflow indicator).
/// Lives here rather than in the recorder library because the marker
/// shape is specific to the tint picker UI.
#[must_use]
pub fn picker_scroll_indicator_visible() -> Check {
    Check {
        name: "picker_scroll_indicator_visible",
        eval: Box::new(|snaps| {
            for (i, s) in snaps.iter().enumerate() {
                for r in 0..s.rows() {
                    if let Some(text) = s.row_text(r)
                        && text.contains('↓')
                        && text.contains("more")
                    {
                        return CheckResult::Pass(format!(
                            "first seen at frame {:04} row {}",
                            i + 1,
                            r + 1
                        ));
                    }
                }
            }
            CheckResult::Fail("picker scroll indicator (↓ N more) never visible".into())
        }),
    }
}

/// Every scene name the registry knows about. Source of truth for any
/// "iterate over all scenes" workflow (Makefile `verify-all`, regression
/// loops, `tint-verify --list-scenes`, etc.).
pub const SCENES: &[&str] = &[
    "demo_full",
    "smoke",
    "picker",
    "cli",
    "cd_hook",
    "custom_theme",
    "bench_tiny",
    "bench_churn",
    "bench_subloops",
];

const fn rgb(r: u8, g: u8, b: u8) -> HexColor {
    HexColor::from_rgb(r, g, b)
}

const DARK_AZURE: HexColor = rgb(0x3a, 0x59, 0x78);
const DARK_INDIGO: HexColor = rgb(0x49, 0x3a, 0x78);
const DRACULA: HexColor = rgb(0x28, 0x2a, 0x36);
const SOLARIZED_LIGHT: HexColor = rgb(0xfd, 0xf6, 0xe3);
const MONOKAI: HexColor = rgb(0x27, 0x28, 0x22);
const DEEP_SKY_BLUE: HexColor = rgb(0x21, 0x3c, 0x45);
const DARK_GREEN: HexColor = rgb(0x3a, 0x78, 0x3a);
const PALE_YELLOW: HexColor = rgb(0xde, 0xde, 0xba);
const MATRIX: HexColor = rgb(0x00, 0x00, 0x00);
/// Snapshot bg after `tint reset` — matches the `recorder/snapshot.ts`
/// startup default (`#1a1b26`). Feature-loop contracts use this to check
/// the clear-to-blank ending matches the loop's start state.
const DEFAULT_BG: HexColor = rgb(0x1a, 0x1b, 0x26);

/// Look up the contract for `scene`. Returns `None` if `scene` isn't in
/// [`SCENES`]. Pure: no IO, no global state — safe to call concurrently
/// from a parallel `verify-all` fan-out.
#[must_use]
pub fn registry(scene: &str) -> Option<Contract> {
    match scene {
        "demo_full" => Some(demo_full()),
        "smoke" => Some(smoke()),
        "picker" => Some(picker()),
        "cli" => Some(cli()),
        "cd_hook" => Some(cd_hook()),
        "custom_theme" => Some(custom_theme()),
        "bench_tiny" => Some(open_contract("bench_tiny")),
        "bench_churn" => Some(open_contract("bench_churn")),
        "bench_subloops" => Some(open_contract("bench_subloops")),
        _ => None,
    }
}

fn demo_full() -> Contract {
    let checks: Vec<Check> = vec![
        picker_scroll_indicator_visible(),
        bg_reaches("dark-azure", DARK_AZURE),
        bg_reaches("dark-indigo-picker-overshoot", DARK_INDIGO),
        bg_reaches("dracula", DRACULA),
        bg_reaches("solarized-light", SOLARIZED_LIGHT),
        bg_reaches("monokai", MONOKAI),
        bg_reaches("deep-sky-blue-cd-hook", DEEP_SKY_BLUE),
        bg_reaches("dark-green-cd-hook", DARK_GREEN),
        bg_reaches("matrix-custom", MATRIX),
        no_row_contains("joined-picker-prompt", "dark-azuretint $"),
        no_row_contains("mkdir-exists", "cannot create directory"),
        // `tint reset` returns to the snapshot's default bg. Matches the
        // loop's start state for a graceful wrap-around.
        final_bg_is("reset-default", DEFAULT_BG),
    ];
    Contract {
        scene: "demo_full",
        checks,
    }
}

fn smoke() -> Contract {
    Contract {
        scene: "smoke",
        checks: vec![picker_scroll_indicator_visible()],
    }
}

fn picker() -> Contract {
    Contract {
        scene: "picker",
        checks: vec![
            picker_scroll_indicator_visible(),
            bg_reaches("dark-indigo-picker-overshoot", DARK_INDIGO),
            bg_reaches("dark-azure", DARK_AZURE),
            final_bg_is("reset-default", DEFAULT_BG),
        ],
    }
}

fn cli() -> Contract {
    Contract {
        scene: "cli",
        checks: vec![
            bg_reaches("dracula", DRACULA),
            bg_reaches("solarized-light", SOLARIZED_LIGHT),
            bg_reaches("monokai", MONOKAI),
            final_bg_is("reset-default", DEFAULT_BG),
        ],
    }
}

fn cd_hook() -> Contract {
    Contract {
        scene: "cd_hook",
        checks: vec![
            bg_reaches("deep-sky-blue", DEEP_SKY_BLUE),
            bg_reaches("dark-green", DARK_GREEN),
            bg_reaches("pale-yellow", PALE_YELLOW),
            no_row_contains("mkdir-exists", "cannot create directory"),
            final_bg_is("reset-default", DEFAULT_BG),
        ],
    }
}

fn custom_theme() -> Contract {
    Contract {
        scene: "custom_theme",
        checks: vec![
            bg_reaches("matrix", MATRIX),
            final_bg_is("reset-default", DEFAULT_BG),
        ],
    }
}

/// Open contract: scene exists but has no validation checks. Use for
/// exploratory scenes whose endpoints aren't predictable (random themes,
/// `tint -l` based pickers, mood loops). The render pipeline still runs
/// to completion; verify reports zero failures.
#[must_use]
pub fn open_contract(scene: &'static str) -> Contract {
    Contract {
        scene,
        checks: vec![],
    }
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
