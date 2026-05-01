#!/usr/bin/env node
/**
 * Read an asciinema v2 cast, replay it through @xterm/headless, and emit
 * one JSON snapshot per cast event capturing:
 *   - terminal background color (from OSC 11)
 *   - terminal foreground color (from OSC 10)
 *   - per-cell { char, fg, bg, attrs } for the entire visible buffer
 *
 * Output:
 *   <outdir>/0001.json, 0002.json, ...
 *   <outdir>/timing.json    [{frame, dwell_ms}, ...]
 *
 * Usage:
 *   node snapshot.js <cast-file> <out-dir>
 */

const fs = require('fs');
const path = require('path');
const { Terminal } = require('@xterm/headless');

const CAST = process.argv[2];
const OUTDIR = process.argv[3];
if (!CAST || !OUTDIR) {
    console.error('usage: node snapshot.js <cast-file> <out-dir>');
    process.exit(2);
}
fs.mkdirSync(OUTDIR, { recursive: true });

const lines = fs.readFileSync(CAST, 'utf8').split('\n').filter(Boolean);
const header = JSON.parse(lines[0]);
const events = lines.slice(1).map(JSON.parse);

const term = new Terminal({
    cols: header.width,
    rows: header.height,
    allowProposedApi: true,
});

// Mutable state we capture from OSC handlers
const state = {
    bg: '#1a1b26',   // matches our driver stub default
    fg: '#c0caf5',
    palette: {},     // index → '#rrggbb' from OSC 4
};

// xterm OSC color formats: `rgb:RR[RR]/GG[GG]/BB[BB]` (4-digit-per-channel
// supported, we keep the high byte) or `#rrggbb`. Returns "#rrggbb" or null.
const COLOR_RE = /rgb:([0-9a-f]{2})[0-9a-f]*\/([0-9a-f]{2})[0-9a-f]*\/([0-9a-f]{2})|^#?([0-9a-f]{2})([0-9a-f]{2})([0-9a-f]{2})$/i;
function parseColor(s) {
    const m = COLOR_RE.exec(s);
    if (!m) return null;
    const [r, g, b] = m[1] ? [m[1], m[2], m[3]] : [m[4], m[5], m[6]];
    return `#${r}${g}${b}`.toLowerCase();
}

term.parser.registerOscHandler(11, (data) => {
    const c = parseColor(data); if (c) state.bg = c; return true;
});
term.parser.registerOscHandler(10, (data) => {
    const c = parseColor(data); if (c) state.fg = c; return true;
});
// OSC 4: "INDEX;color[;INDEX;color...]"
term.parser.registerOscHandler(4, (data) => {
    const parts = data.split(';');
    for (let i = 0; i + 1 < parts.length; i += 2) {
        const idx = parseInt(parts[i], 10);
        const c = parseColor(parts[i + 1]);
        if (Number.isFinite(idx) && c) state.palette[idx] = c;
    }
    return true;
});

// Snapshot buffer state into a serializable object
function snapshot() {
    const buf = term.buffer.active;
    const grid = [];
    for (let y = 0; y < term.rows; y++) {
        const line = buf.getLine(y);
        const row = [];
        for (let x = 0; x < term.cols; x++) {
            const cell = line.getCell(x);
            if (!cell) { row.push(null); continue; }
            row.push({
                ch: cell.getChars() || ' ',
                fg: extractColor(cell, 'fg'),
                bg: extractColor(cell, 'bg'),
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

function extractColor(cell, which) {
    // Use the boolean classifiers — the integer mode value is internal
    // (0x3000000 for RGB, 0x2000000 for palette) and not part of the public API.
    const isRGB     = cell[`is${cap(which)}RGB`];
    const isPalette = cell[`is${cap(which)}Palette`];
    const getColor  = cell[`get${cap(which)}Color`];
    if (isRGB.call(cell)) {
        return rgbHex(getColor.call(cell));
    }
    if (isPalette.call(cell)) {
        const idx = getColor.call(cell);
        return { palette: idx, fallback: state.palette[idx] };
    }
    return null;  // default fg/bg
}

function rgbHex(n) {
    const r = (n >>> 16) & 0xff;
    const g = (n >>> 8) & 0xff;
    const b = n & 0xff;
    return `#${r.toString(16).padStart(2,'0')}${g.toString(16).padStart(2,'0')}${b.toString(16).padStart(2,'0')}`;
}

const cap = (s) => s[0].toUpperCase() + s.slice(1);

// Drive the cast, snapshot after each event
async function run() {
    const timing = [];
    let prevT = 0;
    for (let i = 0; i < events.length; i++) {
        const [t, kind, data] = events[i];
        if (kind !== 'o') continue;
        // Feed the bytes synchronously (await write completion)
        await new Promise((resolve) => term.write(data, resolve));
        const snap = snapshot();
        const idx = String(i + 1).padStart(4, '0');
        fs.writeFileSync(
            path.join(OUTDIR, `${idx}.json`),
            JSON.stringify(snap),
        );
        // dwell_ms = (next event's t - this event's t), or 1000ms tail
        const nextT = (i + 1 < events.length) ? events[i + 1][0] : t + 1.0;
        timing.push({
            frame: idx,
            dwell_ms: Math.max(1, Math.round((nextT - t) * 1000)),
        });
    }
    fs.writeFileSync(
        path.join(OUTDIR, 'timing.json'),
        JSON.stringify(timing, null, 2),
    );
    console.log(`wrote ${timing.length} snapshots to ${OUTDIR}`);
}

run().catch((e) => { console.error(e); process.exit(1); });
