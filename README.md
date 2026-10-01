# vt-bridge

Experiments embedding libghostty-vt (the Ghostty terminal engine) in Greg's apps
(Pi GUI, OmaKit, Agent Orchestrator).

## Build libghostty-vt (verified 2026-10-01, ghostty main @ 54bada3)

    git clone --depth 1 https://github.com/ghostty-org/ghostty ~/Projects/ghostty-src
    cd ~/Projects/ghostty-src && mise exec zig@0.16.0 -- zig build -Doptimize=ReleaseFast

Takes ~9 min. The command ends with an error in editor-syntax generation (full app
steps) but zig-out/{lib,include} are complete: libghostty-vt.{a,so}, include/ghostty/vt/*.h.

## spike/pty_dump.c

Runs a command in a PTY, feeds output to libghostty-vt, prints the screen as text.
Verified: ls --color (styles), btop (alt screen, braille, box drawing, queries answered).

    gcc -o pty_dump pty_dump.c -I$G/include -L$G/lib -lghostty-vt -lutil -Wl,-rpath,$G/lib   # G=~/Projects/ghostty-src/zig-out
    ./pty_dump btop

## Crates (verified 2026-10-01)

- `ghostty-vt-sys`: bindgen over `ghostty/vt.h`, statically links `libghostty-vt.a`.
  `GHOSTTY_VT_DIR` overrides the default `~/Projects/ghostty-src/zig-out`. Also generates
  `KEY_CODES` (W3C `KeyboardEvent.code` → GhosttyKey) by parsing the header.
- `ghostty-vt`: safe API. `Terminal::{new, feed, take_replies, take_events, resize, scroll,
  title, snapshot(only_dirty), encode_key}`. Snapshots are rows of styled runs with colours
  already resolved to RGB; `serde` feature makes them JSON (full 120x40 screen ≈ 18 KB,
  one-row update ≈ 0.4 KB). Events: bell, title, pwd, desktop notification, OSC 133 prompt steps.

Tests: `cargo test -p ghostty-vt` (11 tests: styles, wide chars, queries, events, dirty rows,
resize reflow, alt screen, key modes, Kitty keyboard). Examples:
`cargo run -p ghostty-vt --example pty -- --keys "KeyG:G,Escape" --resize 50x10 nvim file`.

Notes: after a cursor move the previous row is reported dirty too (to erase the cursor).
Not wrapped yet: mouse encoding, selection, search, Kitty graphics, scrollback offset query.
