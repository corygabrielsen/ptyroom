/**
 * Read an asciinema v2 cast, replay it through @xterm/headless, and emit
 * one JSON snapshot per cast event capturing:
 *   - terminal background color (from OSC 11)
 *   - terminal foreground color (from OSC 10)
 *   - per-cell { char, fg, bg, attrs } for the entire visible buffer
 *
 * Output:
 *   <outdir>/0001.json, 0002.json, ...
 *   <outdir>/timing.json    [{ frame, dwell_ms }, ...]
 *
 * Usage: tsx renderer/snapshot.ts <cast-file> <out-dir>
 */

import * as fs from "fs";
import * as path from "path";
import { Terminal, type IBufferCell } from "@xterm/headless";

const TAIL_DWELL_MS = 0;

// ───────── types ─────────

interface CastHeader {
  width: number;
  height: number;
}

type CastEvent = [time: number, kind: "o" | "i", data: string];

interface PaletteRef {
  palette: number;
  fallback: string | null;
}

type CellColor = string | PaletteRef | null;

interface CellSnapshot {
  ch: string;
  fg: CellColor;
  bg: CellColor;
  bold: 0 | 1;
  dim: 0 | 1;
  italic: 0 | 1;
  underline: 0 | 1;
  inverse: 0 | 1;
}

interface FrameSnapshot {
  bg: string;
  fg: string;
  palette: Record<number, string>;
  grid: (CellSnapshot | null)[][];
}

interface TimingEntry {
  frame: string;
  dwell_ms: number;
}

interface ReplayState {
  bg: string;
  fg: string;
  palette: Record<number, string>;
}

// ───────── color parsing ─────────

// xterm OSC color formats: `rgb:RR[RR]/GG[GG]/BB[BB]` (4-digit-per-channel
// supported, we keep the high byte) or `#rrggbb`. Returns "#rrggbb" or null.
const COLOR_RE =
  /rgb:([0-9a-f]{2})[0-9a-f]*\/([0-9a-f]{2})[0-9a-f]*\/([0-9a-f]{2})|^#?([0-9a-f]{2})([0-9a-f]{2})([0-9a-f]{2})$/i;

function parseColor(s: string): string | null {
  const m = COLOR_RE.exec(s);
  if (!m) return null;
  const [r, g, b] = m[1] ? [m[1], m[2], m[3]] : [m[4], m[5], m[6]];
  return `#${r}${g}${b}`.toLowerCase();
}

function rgbHex(n: number): string {
  const r = (n >>> 16) & 0xff;
  const g = (n >>> 8) & 0xff;
  const b = n & 0xff;
  return `#${r.toString(16).padStart(2, "0")}${g.toString(16).padStart(2, "0")}${b.toString(16).padStart(2, "0")}`;
}

// ───────── snapshot extraction ─────────

function extractFg(cell: IBufferCell, state: ReplayState): CellColor {
  if (cell.isFgRGB()) return rgbHex(cell.getFgColor());
  if (cell.isFgPalette()) {
    const idx = cell.getFgColor();
    return { palette: idx, fallback: state.palette[idx] ?? null };
  }
  return null;
}

function extractBg(cell: IBufferCell, state: ReplayState): CellColor {
  if (cell.isBgRGB()) return rgbHex(cell.getBgColor());
  if (cell.isBgPalette()) {
    const idx = cell.getBgColor();
    return { palette: idx, fallback: state.palette[idx] ?? null };
  }
  return null;
}

function snapshot(term: Terminal, state: ReplayState): FrameSnapshot {
  const buf = term.buffer.active;
  const grid: (CellSnapshot | null)[][] = [];
  for (let y = 0; y < term.rows; y++) {
    const line = buf.getLine(y);
    const row: (CellSnapshot | null)[] = [];
    if (!line) {
      grid.push(row);
      continue;
    }
    for (let x = 0; x < term.cols; x++) {
      const cell = line.getCell(x);
      if (!cell) {
        row.push(null);
        continue;
      }
      row.push({
        ch: cell.getChars() || " ",
        fg: extractFg(cell, state),
        bg: extractBg(cell, state),
        bold: cell.isBold() ? 1 : 0,
        dim: cell.isDim() ? 1 : 0,
        italic: cell.isItalic() ? 1 : 0,
        underline: cell.isUnderline() ? 1 : 0,
        inverse: cell.isInverse() ? 1 : 0,
      });
    }
    grid.push(row);
  }
  return {
    bg: state.bg,
    fg: state.fg,
    palette: { ...state.palette },
    grid,
  };
}

// ───────── replay driver ─────────

async function run(castPath: string, outDir: string): Promise<void> {
  fs.mkdirSync(outDir, { recursive: true });
  const lines = fs.readFileSync(castPath, "utf8").split("\n").filter(Boolean);
  const header = JSON.parse(lines[0]) as CastHeader;
  const events: CastEvent[] = lines.slice(1).map((l) => JSON.parse(l) as CastEvent);

  const term = new Terminal({
    cols: header.width,
    rows: header.height,
    allowProposedApi: true,
  });

  // Defaults — both the snapshot's startup state AND the target for
  // reset (OSC 111/110/104). Match the recorder driver's stub default
  // so a `tint reset` returns to the same bg the demo started on,
  // making the GIF loop wrap around without a jarring transition.
  const DEFAULT_BG = "#1a1b26";
  const DEFAULT_FG = "#c0caf5";

  const state: ReplayState = {
    bg: DEFAULT_BG,
    fg: DEFAULT_FG,
    palette: {},
  };

  term.parser.registerOscHandler(11, (data) => {
    const c = parseColor(data);
    if (c) state.bg = c;
    return true;
  });
  term.parser.registerOscHandler(10, (data) => {
    const c = parseColor(data);
    if (c) state.fg = c;
    return true;
  });
  // OSC 4: "INDEX;color[;INDEX;color...]"
  term.parser.registerOscHandler(4, (data) => {
    const parts = data.split(";");
    for (let i = 0; i + 1 < parts.length; i += 2) {
      const idx = parseInt(parts[i], 10);
      const c = parseColor(parts[i + 1]);
      if (Number.isFinite(idx) && c) state.palette[idx] = c;
    }
    return true;
  });
  // OSC 111/110/104: reset bg/fg/palette to terminal defaults.
  // tint emits these on `tint reset`. Without these handlers, the
  // last-applied colors would persist visually even though the user
  // asked for a reset.
  term.parser.registerOscHandler(111, () => { state.bg = DEFAULT_BG; return true; });
  term.parser.registerOscHandler(110, () => { state.fg = DEFAULT_FG; return true; });
  term.parser.registerOscHandler(104, () => { state.palette = {}; return true; });

  const timing: TimingEntry[] = [];
  for (let i = 0; i < events.length; i++) {
    const [t, kind, data] = events[i];
    if (kind !== "o") continue;
    await new Promise<void>((resolve) => term.write(data, resolve));
    const snap = snapshot(term, state);
    const idx = String(i + 1).padStart(4, "0");
    fs.writeFileSync(path.join(outDir, `${idx}.json`), JSON.stringify(snap));
    const nextT = i + 1 < events.length ? events[i + 1][0] : null;
    timing.push({
      frame: idx,
      dwell_ms:
        nextT === null
          ? TAIL_DWELL_MS
          : Math.max(1, Math.round((nextT - t) * 1000)),
    });
  }
  fs.writeFileSync(path.join(outDir, "timing.json"), JSON.stringify(timing, null, 2));
  console.log(`wrote ${timing.length} snapshots to ${outDir}`);
}

// ───────── entry ─────────

const castArg = process.argv[2];
const outDirArg = process.argv[3];
if (!castArg || !outDirArg) {
  console.error("usage: tsx renderer/snapshot.ts <cast-file> <out-dir>");
  process.exit(2);
}

run(castArg, outDirArg).catch((e: unknown) => {
  console.error(e);
  process.exit(1);
});
