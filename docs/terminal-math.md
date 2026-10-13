# Terminal math rendering

This fork can typeset LaTeX math in direct Ghostty and Kitty sessions. Codex
parses and measures equations, reserves their terminal cells, then uploads
transparent PNGs using Kitty's Unicode placeholder protocol. The terminal
supplies cell dimensions and draws those images. No browser or TeX installation
is needed.

Inline math (`$x^2$` or `\(x^2\)`) shares the surrounding sentence's baseline.
Tall fractions reserve extra rows around that baseline. Each equation wraps as
one unit. Standalone display math (`$$...$$` or `\[...\]`) is centered within
the available content width. Transcript resizing recalculates placement;
viewport clipping and scrolling operate on the same cells as ordinary text.
Codex's selection copying restores the original LaTeX.

## Try it

From `codex-rs`, in Ghostty or Kitty:

```sh
cargo build --locked -p codex-cli -p codex-code-mode-host \
  --bin codex --bin codex-code-mode-host
./target/debug/codex -c tui.rendering.math=true
```

Build both packages: Code Mode uses the separate `codex-code-mode-host`
executable beside `codex` to run JavaScript that coordinates tool calls. Building
only the CLI leaves that helper missing and causes the "Code Mode is unavailable"
warning. After building the helper, restart Codex so it checks availability again.

If the helper build reports a missing V8 download, use the matching
`RUSTY_V8_ARCHIVE` and `RUSTY_V8_SRC_BINDING_PATH` overrides described in
[the V8 build notes](../third_party/v8/README.md). The repository's
`.github/actions/setup-rusty-v8` action downloads and verifies those artifacts
for Cargo builds. The archive and bindings must match the locked V8 version.

Ask for a response containing these examples, without code fences:

```text
Before $x^2$ after, with $\frac{a}{b}$ inline.

$$
\int_0^1 x^2\,dx = \frac{1}{3}
$$

Following paragraph.
```

Check wide and narrow window sizes, font zoom, scrolling, text selection,
and the transcript overlay. The display equation should stay centered, inline
prose should stay on one baseline, and the next paragraph should remain below
the equation. Copying should produce LaTeX, not image placeholder characters.
Launch the built binary above; an already installed Codex executable does not
contain this change.

The existing `tui.rendering.math = false` setting disables math rendering.

## Boundaries

- The terminal must report nonzero pixel and cell dimensions. Other terminals,
  multiplexers, and sessions with terminal colors disabled use the existing text
  renderer. Math inside tables and text-only consumers also use that fallback.
- The native renderer supports a subset of LaTeX math. Unsupported syntax and
  equations too large for the available width retain the existing Unicode or
  source representation. It does not execute TeX commands or read external files.
- Font size and baseline use a three-quarter-cell estimate because terminals
  report cell size, not font ascent. Exact alignment with every terminal font
  needs a visual check on that font.
- Select with Codex's selection controls to copy LaTeX. Native terminal selection
  sees the graphics protocol's placeholder cells. Very old images in terminal
  scrollback may be evicted by the terminal; the owned transcript reuploads
  visible equations.

Inputs, nesting, raster dimensions, and the raster cache are bounded. Rendering
uses `latex-rust` with the embedded STIX Two Math font; the redistribution
notices are in `third_party/latex-rust`.

## Checks

```sh
cd codex-rs
RUST_MIN_STACK=33554432 cargo test --locked -p codex-tui --lib -- math_graphics math_reflow
cargo check --locked -p codex-tui --lib
```

The tests inject terminal cell metrics without requiring a graphical terminal.
They cover inline and display placement, wrapping, clipping, transcript reflow,
copying, streaming, transparent raster dimensions, and Kitty image transfers.
