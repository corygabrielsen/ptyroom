//! CLI: frame-by-frame A/B comparison of replayed snapshot directories.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::Parser;
use term_recorder::color::HexColor;
use term_recorder::encode::TimingEntry;
use term_recorder::snapshot::{Cell, Snapshot};
use term_recorder::verify::list_numbered_snapshots;

#[derive(Parser)]
struct Args {
    /// Baseline snapshot directory.
    baseline: PathBuf,
    /// Candidate snapshot directory.
    candidate: PathBuf,
    /// Maximum concrete diff examples to print.
    #[arg(long, default_value_t = 40)]
    max_examples: usize,
    /// Do not compare `timing.json`.
    #[arg(long)]
    ignore_timing: bool,
}

fn main() -> ExitCode {
    match run(&Args::parse()) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(1),
        Err(err) => {
            eprintln!("compare_snapshots: {err:#}");
            ExitCode::from(2)
        }
    }
}

fn run(args: &Args) -> anyhow::Result<bool> {
    let baseline = load_named_snapshots(&args.baseline)?;
    let candidate = load_named_snapshots(&args.candidate)?;
    let mut report = DiffReport::new(args.max_examples);

    compare_frame_counts(&baseline, &candidate, &mut report);
    for ((base_name, base), (cand_name, cand)) in baseline.iter().zip(&candidate) {
        compare_snapshot(base_name, cand_name, base, cand, &mut report);
    }

    if !args.ignore_timing {
        compare_timing(&args.baseline, &args.candidate, &mut report)?;
    }

    report.print();
    Ok(report.is_clean())
}

fn load_named_snapshots(dir: &Path) -> anyhow::Result<Vec<(String, Snapshot)>> {
    list_numbered_snapshots(dir)?
        .into_iter()
        .map(|path| {
            let frame = path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .ok_or_else(|| anyhow::anyhow!("invalid snapshot filename: {}", path.display()))?
                .to_owned();
            let snapshot = Snapshot::load(&path)?;
            Ok((frame, snapshot))
        })
        .collect()
}

fn compare_frame_counts(
    baseline: &[(String, Snapshot)],
    candidate: &[(String, Snapshot)],
    report: &mut DiffReport,
) {
    if baseline.len() != candidate.len() {
        report.frame_diffs += baseline.len().abs_diff(candidate.len());
        report.example(format!(
            "frame count differs: baseline={} candidate={}",
            baseline.len(),
            candidate.len()
        ));
    }
}

fn compare_snapshot(
    base_name: &str,
    cand_name: &str,
    baseline: &Snapshot,
    candidate: &Snapshot,
    report: &mut DiffReport,
) {
    if base_name != cand_name {
        report.frame_diffs += 1;
        report.example(format!(
            "frame name differs at baseline {base_name}: candidate {cand_name}"
        ));
    }

    if baseline.rows() != candidate.rows() || baseline.cols() != candidate.cols() {
        report.frame_diffs += 1;
        report.example(format!(
            "frame {base_name}: dimensions differ baseline={}x{} candidate={}x{}",
            baseline.cols(),
            baseline.rows(),
            candidate.cols(),
            candidate.rows()
        ));
    }

    if baseline.bg != candidate.bg {
        report.frame_diffs += 1;
        report.example(format!(
            "frame {base_name}: default bg differs {} -> {}",
            baseline.bg, candidate.bg
        ));
    }

    if baseline.fg != candidate.fg {
        report.frame_diffs += 1;
        report.example(format!(
            "frame {base_name}: default fg differs {} -> {}",
            baseline.fg, candidate.fg
        ));
    }

    if baseline.palette != candidate.palette {
        report.frame_diffs += 1;
        report.example(format!("frame {base_name}: palette differs"));
    }

    let rows = baseline.rows().min(candidate.rows());
    let cols = baseline.cols().min(candidate.cols());
    for y in 0..rows {
        for x in 0..cols {
            let base = visual_cell(baseline.grid.cell(x, y), baseline);
            let cand = visual_cell(candidate.grid.cell(x, y), candidate);
            if base != cand {
                report.cell_diffs += 1;
                report.example(format!(
                    "frame {base_name}: cell ({x},{y}) differs {base} -> {cand}"
                ));
            }
        }
    }
}

fn compare_timing(
    baseline_dir: &Path,
    candidate_dir: &Path,
    report: &mut DiffReport,
) -> anyhow::Result<()> {
    let baseline = load_timing_if_present(baseline_dir)?;
    let candidate = load_timing_if_present(candidate_dir)?;

    match (baseline, candidate) {
        (None, None) => Ok(()),
        (Some(_), None) => {
            report.timing_diffs += 1;
            report.example("timing differs: candidate missing timing.json".to_owned());
            Ok(())
        }
        (None, Some(_)) => {
            report.timing_diffs += 1;
            report.example("timing differs: baseline missing timing.json".to_owned());
            Ok(())
        }
        (Some(baseline), Some(candidate)) => {
            if baseline.len() != candidate.len() {
                report.timing_diffs += baseline.len().abs_diff(candidate.len());
                report.example(format!(
                    "timing count differs: baseline={} candidate={}",
                    baseline.len(),
                    candidate.len()
                ));
            }
            for (idx, (base, cand)) in baseline.iter().zip(&candidate).enumerate() {
                if base.frame != cand.frame || base.dwell_ms != cand.dwell_ms {
                    report.timing_diffs += 1;
                    report.example(format!(
                        "timing[{idx}] differs {}:{}ms -> {}:{}ms",
                        base.frame, base.dwell_ms, cand.frame, cand.dwell_ms
                    ));
                }
            }
            Ok(())
        }
    }
}

fn load_timing_if_present(dir: &Path) -> anyhow::Result<Option<Vec<TimingEntry>>> {
    let path = dir.join("timing.json");
    if !path.exists() {
        return Ok(None);
    }
    Ok(Some(serde_json::from_slice(&std::fs::read(path)?)?))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct VisualCell {
    ch: char,
    fg: HexColor,
    bg: HexColor,
    attrs: AttrFlags,
}

impl std::fmt::Display for VisualCell {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{:?} fg={} bg={} flags={}{}{}{}{}",
            self.ch,
            self.fg,
            self.bg,
            self.attrs.flag(AttrFlags::BOLD, 'b'),
            self.attrs.flag(AttrFlags::DIM, 'd'),
            self.attrs.flag(AttrFlags::ITALIC, 'i'),
            self.attrs.flag(AttrFlags::UNDERLINE, 'u'),
            self.attrs.flag(AttrFlags::INVERSE, 'v'),
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AttrFlags(u8);

impl AttrFlags {
    const BOLD: u8 = 1 << 0;
    const DIM: u8 = 1 << 1;
    const ITALIC: u8 = 1 << 2;
    const UNDERLINE: u8 = 1 << 3;
    const INVERSE: u8 = 1 << 4;

    const fn empty() -> Self {
        Self(0)
    }

    const fn from_cell(cell: &Cell) -> Self {
        Self(
            (if cell.bold != 0 { Self::BOLD } else { 0 })
                | (if cell.dim != 0 { Self::DIM } else { 0 })
                | (if cell.italic != 0 { Self::ITALIC } else { 0 })
                | (if cell.underline != 0 {
                    Self::UNDERLINE
                } else {
                    0
                })
                | (if cell.inverse != 0 { Self::INVERSE } else { 0 }),
        )
    }

    const fn contains(self, bit: u8) -> bool {
        self.0 & bit != 0
    }

    const fn flag(self, bit: u8, ch: char) -> char {
        if self.contains(bit) { ch } else { '-' }
    }
}

fn visual_cell(cell: Option<&Cell>, snap: &Snapshot) -> VisualCell {
    let Some(cell) = cell else {
        return VisualCell {
            ch: ' ',
            fg: snap.fg,
            bg: snap.bg,
            attrs: AttrFlags::empty(),
        };
    };
    let (fg, bg) = cell.resolve_layers(snap);
    VisualCell {
        ch: cell.first_char(),
        fg,
        bg,
        attrs: AttrFlags::from_cell(cell),
    }
}

struct DiffReport {
    frame_diffs: usize,
    cell_diffs: usize,
    timing_diffs: usize,
    examples: Vec<String>,
    max_examples: usize,
}

impl DiffReport {
    const fn new(max_examples: usize) -> Self {
        Self {
            frame_diffs: 0,
            cell_diffs: 0,
            timing_diffs: 0,
            examples: Vec::new(),
            max_examples,
        }
    }

    const fn is_clean(&self) -> bool {
        self.frame_diffs == 0 && self.cell_diffs == 0 && self.timing_diffs == 0
    }

    fn example(&mut self, text: String) {
        if self.examples.len() < self.max_examples {
            self.examples.push(text);
        }
    }

    fn print(&self) {
        if self.is_clean() {
            println!("PASS snapshots match exactly");
            return;
        }

        println!(
            "FAIL frame_diffs={} cell_diffs={} timing_diffs={}",
            self.frame_diffs, self.cell_diffs, self.timing_diffs
        );
        for example in &self.examples {
            println!("- {example}");
        }
        let omitted = self
            .frame_diffs
            .saturating_add(self.cell_diffs)
            .saturating_add(self.timing_diffs)
            .saturating_sub(self.examples.len());
        if omitted > 0 {
            println!("- ... {omitted} additional differences omitted");
        }
    }
}
