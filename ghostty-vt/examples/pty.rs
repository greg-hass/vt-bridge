//! Run a command in a PTY through the emulator, optionally typing keys, and print the screen.
//!   cargo run -p ghostty-vt --example pty -- [--keys "KeyI,KeyH:h,Escape"] [--resize 60x15] CMD ARGS...
use ghostty_vt::*;
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use std::io::{Read, Write};
use std::sync::mpsc;
use std::time::{Duration, Instant};

fn pump(t: &mut Terminal, rx: &mpsc::Receiver<Vec<u8>>, w: &mut dyn Write, ms: u64) {
    let end = Instant::now() + Duration::from_millis(ms);
    while let Some(left) = end.checked_duration_since(Instant::now()) {
        match rx.recv_timeout(left) {
            Ok(b) => {
                t.feed(&b);
                let r = t.take_replies();
                if !r.is_empty() {
                    w.write_all(&r).unwrap();
                }
            }
            Err(_) => break,
        }
    }
}

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let mut keys = String::new();
    let mut resize = None;
    while args.first().is_some_and(|a| a.starts_with("--")) {
        let flag = args.remove(0);
        let val = args.remove(0);
        match flag.as_str() {
            "--keys" => keys = val,
            "--resize" => resize = Some(val),
            _ => panic!("unknown flag {flag}"),
        }
    }
    let (cols, rows) = (80u16, 24u16);
    let pair = native_pty_system().openpty(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 }).unwrap();
    let mut cmd = CommandBuilder::new(&args[0]);
    cmd.args(&args[1..]);
    cmd.env("TERM", "xterm-256color");
    let mut child = pair.slave.spawn_command(cmd).unwrap();
    let mut reader = pair.master.try_clone_reader().unwrap();
    let mut writer = pair.master.take_writer().unwrap();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = [0u8; 65536];
        while let Ok(n) = reader.read(&mut buf) {
            if n == 0 || tx.send(buf[..n].to_vec()).is_err() {
                break;
            }
        }
    });

    let mut t = Terminal::new(cols, rows).unwrap();
    pump(&mut t, &rx, &mut writer, 1500);
    for k in keys.split(',').filter(|k| !k.is_empty()) {
        let (code, text) = k.split_once(':').map_or((k, None), |(c, t)| (c, Some(t)));
        let bytes = if let Some(m) = code.strip_prefix("C-") {
            t.encode_key(m, text, Mods { ctrl: true, ..Default::default() }, KeyAction::Press)
        } else {
            t.encode_key(code, text, Mods::default(), KeyAction::Press)
        };
        writer.write_all(&bytes).unwrap();
        pump(&mut t, &rx, &mut writer, 150);
    }
    if let Some(r) = resize {
        let (c, rr) = r.split_once('x').unwrap();
        let (c, rr): (u16, u16) = (c.parse().unwrap(), rr.parse().unwrap());
        pair.master.resize(PtySize { rows: rr, cols: c, pixel_width: 0, pixel_height: 0 }).unwrap();
        t.resize(c, rr, (8, 16)).unwrap();
        pump(&mut t, &rx, &mut writer, 800);
    }
    let snap = t.snapshot(false).unwrap();
    println!("title: {:?}  cursor: {:?}", t.title(), snap.cursor);
    for r in &snap.rows {
        println!("{:2}|{}", r.y, r.text());
    }
    let _ = child.kill();
}
