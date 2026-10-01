//! Run a command in a PTY and print the full snapshot as the JSON the Pi GUI webview receives.
//!   cargo run -p ghostty-vt --features serde --example dump_frame -- COLSxROWS CMD ARGS...
use ghostty_vt::*;
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use std::io::{Read, Write};
use std::sync::mpsc;
use std::time::{Duration, Instant};

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let size = args.remove(0);
    let (c, r) = size.split_once('x').unwrap();
    let (cols, rows): (u16, u16) = (c.parse().unwrap(), r.parse().unwrap());
    let pair = native_pty_system().openpty(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 }).unwrap();
    let mut cmd = CommandBuilder::new(&args[0]);
    cmd.args(&args[1..]);
    cmd.env("TERM", "xterm-256color");
    cmd.env("COLORTERM", "truecolor");
    let mut child = pair.slave.spawn_command(cmd).unwrap();
    let mut reader = pair.master.try_clone_reader().unwrap();
    let mut writer = pair.master.take_writer().unwrap();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = [0u8; 65536];
        while let Ok(n) = reader.read(&mut buf) {
            if n == 0 || tx.send(buf[..n].to_vec()).is_err() { break; }
        }
    });
    let mut t = Terminal::new(cols, rows).unwrap();
    let end = Instant::now() + Duration::from_millis(2500);
    while let Some(left) = end.checked_duration_since(Instant::now()) {
        if let Ok(b) = rx.recv_timeout(left) {
            t.feed(&b);
            let rep = t.take_replies();
            if !rep.is_empty() { writer.write_all(&rep).unwrap(); }
        }
    }
    let snap = t.snapshot(false).unwrap();
    let mut v = serde_json::to_value(&snap).unwrap();
    v["sessionId"] = "dump".into();
    println!("{v}");
    let _ = child.kill();
}
