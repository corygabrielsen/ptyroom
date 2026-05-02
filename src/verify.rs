//! Per-scene visual-assertion contracts.
//!
//! A [`Contract`] is a list of named [`Check`]s. Each check is a closure
//! that inspects the loaded snapshots and returns either pass with detail or
//! fail with detail. Contracts catch regressions; they don't validate every
//! pixel.

use std::path::Path;

use crate::color::HexColor;
use crate::snapshot::Snapshot;

/// A check function: takes the loaded snapshots, returns pass/fail with detail.
pub type CheckFn = Box<dyn Fn(&[Snapshot]) -> CheckResult + Send + Sync>;

/// A single named check, evaluated against the loaded snapshots.
pub struct Check {
    pub name: &'static str,
    pub eval: CheckFn,
}

#[derive(Debug, Clone)]
pub enum CheckResult {
    Pass(String),
    Fail(String),
}

impl CheckResult {
    #[must_use] 
    pub fn passed(&self) -> bool { matches!(self, CheckResult::Pass(_)) }
    #[must_use] 
    pub fn detail(&self) -> &str {
        match self { CheckResult::Pass(d) | CheckResult::Fail(d) => d }
    }
}

pub struct Contract {
    pub scene: &'static str,
    pub checks: Vec<Check>,
}

impl Contract {
    #[must_use] 
    pub fn run(&self, snaps: &[Snapshot]) -> ContractReport {
        let results: Vec<(String, CheckResult)> = self.checks.iter()
            .map(|c| (c.name.to_string(), (c.eval)(snaps)))
            .collect();
        let failed = results.iter().filter(|(_, r)| !r.passed()).count();
        ContractReport { scene: self.scene.to_string(), results, failed }
    }
}

#[derive(Debug)]
pub struct ContractReport {
    pub scene: String,
    pub results: Vec<(String, CheckResult)>,
    pub failed: usize,
}

impl ContractReport {
    pub fn print(&self) {
        let name_width = self.results.iter().map(|(n, _)| n.len()).max().unwrap_or(0);
        for (name, r) in &self.results {
            let marker = if r.passed() { "PASS" } else { "FAIL" };
            println!("{marker}  {}/{name:<name_width$}  {}", self.scene, r.detail());
        }
    }

    #[must_use]
    pub fn exit_code(&self) -> i32 { i32::from(self.failed != 0) }
}

// ─────────────── Builders ───────────────

#[must_use] 
pub fn bg_reaches(label: &'static str, color: HexColor) -> Check {
    Check {
        name: leak_str(format!("bg_reaches_{label}")),
        eval: Box::new(move |snaps| match find_first_bg(snaps, color) {
            Some(idx) => CheckResult::Pass(
                format!("{label} ({color}) reached at frame {idx:04}")),
            None => CheckResult::Fail(
                format!("{label} ({color}) never applied")),
        }),
    }
}

#[must_use] 
pub fn final_bg_is(label: &'static str, color: HexColor) -> Check {
    Check {
        name: leak_str(format!("final_bg_{label}")),
        eval: Box::new(move |snaps| match snaps.last() {
            Some(last) if last.bg == color =>
                CheckResult::Pass(format!("final bg={color} ({label})")),
            Some(last) => CheckResult::Fail(
                format!("final bg={}, expected {color} ({label})", last.bg)),
            None => CheckResult::Fail("no snapshots".into()),
        }),
    }
}

#[must_use] 
pub fn picker_scroll_indicator_visible() -> Check {
    Check {
        name: "picker_scroll_indicator_visible",
        eval: Box::new(|snaps| {
            for (i, s) in snaps.iter().enumerate() {
                for r in 0..s.rows() {
                    if let Some(text) = s.row_text(r)
                        && text.contains('↓') && text.contains("more")
                    {
                        return CheckResult::Pass(
                            format!("first seen at frame {:04} row {}", i + 1, r + 1));
                    }
                }
            }
            CheckResult::Fail("picker scroll indicator (↓ N more) never visible".into())
        }),
    }
}

fn find_first_bg(snaps: &[Snapshot], color: HexColor) -> Option<usize> {
    snaps.iter().position(|s| s.bg == color).map(|i| i + 1)
}

/// Leak a `String` to `&'static str`. We use this only for `Check::name`
/// produced by builders at startup; the leaked memory lives for the
/// process lifetime, no leak in the GC sense.
fn leak_str(s: String) -> &'static str {
    Box::leak(s.into_boxed_str())
}

// ─────────────── Snapshot loading ───────────────

/// List numbered `*.json` snapshot paths in `dir`, sorted ascending.
/// Filters to entries whose stem is all ASCII digits (`0001.json`, etc.).
///
/// # Errors
/// IO error reading `dir`, or zero matching entries.
pub fn list_numbered_snapshots(dir: &Path) -> anyhow::Result<Vec<std::path::PathBuf>> {
    let mut paths: Vec<_> = std::fs::read_dir(dir)?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("json"))
        .filter(|p| p.file_stem().and_then(|s| s.to_str())
                     .is_some_and(|n| n.chars().all(|c| c.is_ascii_digit())))
        .collect();
    paths.sort();
    if paths.is_empty() {
        anyhow::bail!("no numbered snapshots in {}", dir.display());
    }
    Ok(paths)
}

/// Load every numbered snapshot under `dir`, in order.
///
/// # Errors
/// Any error from [`list_numbered_snapshots`] or per-snapshot load.
pub fn load_snapshots_dir(dir: &Path) -> anyhow::Result<Vec<Snapshot>> {
    list_numbered_snapshots(dir)?.iter().map(Snapshot::load).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::{CellColor, PaletteOverrides};
    use crate::snapshot::{Cell, Grid};

    fn snap_with_bg(bg: HexColor, ch: char) -> Snapshot {
        Snapshot {
            bg,
            fg: HexColor::from_rgb(0xff, 0xff, 0xff),
            palette: PaletteOverrides::new(),
            grid: Grid::from_unchecked(vec![vec![Some(Cell {
                ch: ch.to_string(),
                fg: CellColor::Default, bg: CellColor::Default,
                bold:0,dim:0,italic:0,underline:0,inverse:0,
            })]]),
        }
    }

    #[test]
    fn bg_reaches_passes_when_color_present() {
        let target = HexColor::from_rgb(0x12, 0x34, 0x56);
        let snaps = vec![
            snap_with_bg(HexColor::from_rgb(0,0,0), 'a'),
            snap_with_bg(target, 'b'),
        ];
        let r = (bg_reaches("target", target).eval)(&snaps);
        assert!(r.passed(), "{}", r.detail());
    }

    #[test]
    fn bg_reaches_fails_when_color_missing() {
        let target = HexColor::from_rgb(0x12, 0x34, 0x56);
        let snaps = vec![snap_with_bg(HexColor::from_rgb(0,0,0), 'a')];
        let r = (bg_reaches("target", target).eval)(&snaps);
        assert!(!r.passed());
    }

    #[test]
    fn final_bg_is_checks_last_snapshot() {
        let want = HexColor::from_rgb(0xff, 0x00, 0x6e);
        let snaps = vec![
            snap_with_bg(HexColor::from_rgb(0,0,0), 'a'),
            snap_with_bg(want, 'b'),
        ];
        assert!((final_bg_is("hot", want).eval)(&snaps).passed());
    }

    #[test]
    fn picker_indicator_finds_arrow_more() {
        // Build a snapshot whose row 0 contains "↓ 5 more"
        let mut grid = vec![vec![None; 12]];
        let chars = ['↓', ' ', '5', ' ', 'm', 'o', 'r', 'e'];
        for (i, ch) in chars.iter().enumerate() {
            grid[0][i] = Some(Cell {
                ch: ch.to_string(),
                fg: CellColor::Default, bg: CellColor::Default,
                bold:0,dim:0,italic:0,underline:0,inverse:0,
            });
        }
        let snaps = vec![Snapshot {
            bg: HexColor::from_rgb(0,0,0), fg: HexColor::from_rgb(255,255,255),
            palette: PaletteOverrides::new(), grid: Grid::from_unchecked(grid),
        }];
        assert!((picker_scroll_indicator_visible().eval)(&snaps).passed());
    }
}
