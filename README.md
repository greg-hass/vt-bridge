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
