//! Measures serialized snapshot size for a busy screen (full vs. one-row update).
use ghostty_vt::*;
use std::process::Command;

fn main() {
    let out = Command::new("ls").args(["--color=always", "-la", "/usr/lib"]).output().unwrap().stdout;
    let mut t = Terminal::new(120, 40).unwrap();
    t.feed(&out.iter().flat_map(|&b| if b == b'\n' { vec![b'\r', b'\n'] } else { vec![b] }).collect::<Vec<_>>());
    let full = serde_json::to_string(&t.snapshot(true).unwrap()).unwrap();
    t.feed(b"\x1b[40;1Hx");
    let delta = serde_json::to_string(&t.snapshot(true).unwrap()).unwrap();
    println!("full: {} bytes, one-row update: {} bytes", full.len(), delta.len());
}
