//! vt-host: a terminal behind a pipe.
//!
//!   vt-host [--cols N] [--rows N] [--cwd DIR] -- PROGRAM [ARGS...]
//!
//! stdout (one JSON object per line):
//!   {"t":"frame", ...}          changed rows and cursor, see ghostty-vt's Snapshot
//!   {"t":"event", ...}          bell, title_changed, notification, prompt steps
//!   {"t":"exit","code":N}       the program ended; vt-host exits with the same code
//! stdin (one JSON object per line):
//!   {"t":"key","code":"KeyA","text":"a","shift":false,"ctrl":false,"alt":false,"repeat":false}
//!   {"t":"text","data":"..."}   {"t":"paste","text":"..."}   {"t":"focus","gained":true}
//!   {"t":"resize","cols":80,"rows":24,"cw":8,"ch":16}   {"t":"redraw"}   {"t":"title"}
//!   {"t":"wheel","lines":-3,"x":0,"y":0,"shift":false}   {"t":"scroll","delta":-5}
//!   {"t":"mouse","action":"press","button":1,"x":44,"y":56,"shift":false,"ctrl":false,"alt":false,"any":true}
//!   {"t":"theme","fg":"#..","bg":"#..","cursor":"#..","ansi":[16 colours]}   {"t":"kill"}
//! Mouse and wheel positions are in cells x 8 (columns) and cells x 16 (rows).
//! Closing stdin kills the program.
use ghostty_vt_session::ghostty_vt::{Mods, MouseAction, MouseEvent};
use ghostty_vt_session::{Emit, Options, Output, Session, Theme};
use serde::Deserialize;
use std::io::{BufRead, Write};
use std::sync::{mpsc, Arc, Mutex};

const CELL: (u32, u32) = (8, 16);

#[derive(Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
enum Command {
    Key { code: String, text: Option<String>, #[serde(default)] shift: bool, #[serde(default)] ctrl: bool, #[serde(default)] alt: bool, #[serde(default)] repeat: bool },
    Text { data: String },
    Paste { text: String },
    Focus { gained: bool },
    Resize { cols: u16, rows: u16, #[serde(default)] cw: Option<u32>, #[serde(default)] ch: Option<u32> },
    Redraw,
    Title,
    Wheel { lines: i32, #[serde(default)] x: f32, #[serde(default)] y: f32, #[serde(default)] shift: bool },
    Scroll { delta: isize },
    Mouse { action: String, button: Option<u8>, x: f32, y: f32, #[serde(default)] shift: bool, #[serde(default)] ctrl: bool, #[serde(default)] alt: bool, #[serde(default)] any: bool },
    Theme(Theme),
    Kill,
}

fn usage() -> ! {
    eprintln!("usage: vt-host [--cols N] [--rows N] [--cwd DIR] -- PROGRAM [ARGS...]");
    std::process::exit(2)
}

fn mods(shift: bool, ctrl: bool, alt: bool) -> Mods {
    Mods { shift, ctrl, alt, super_: false }
}

fn handle(session: &Session, line: &str, out: &Emit) -> Result<(), String> {
    let cmd: Command = serde_json::from_str(line).map_err(|e| format!("bad command: {e}"))?;
    match cmd {
        Command::Key { code, text, shift, ctrl, alt, repeat } => session.key(&code, text.as_deref(), mods(shift, ctrl, alt), repeat),
        Command::Text { data } => session.write_text(&data),
        Command::Paste { text } => session.paste(&text),
        Command::Focus { gained } => session.focus(gained),
        Command::Resize { cols, rows, cw, ch } => session.resize(cols, rows, (cw.unwrap_or(8), ch.unwrap_or(16))),
        Command::Redraw => {
            session.redraw();
            Ok(())
        }
        Command::Title => {
            println!("{}", serde_json::json!({"t": "title", "title": session.title()}));
            Ok(())
        }
        Command::Wheel { lines, x, y, shift } => session.wheel(lines, x, y, CELL, shift),
        Command::Scroll { delta } => session.scroll(delta),
        Command::Mouse { action, button, x, y, shift, ctrl, alt, any } => {
            let action = match action.as_str() {
                "press" => MouseAction::Press,
                "release" => MouseAction::Release,
                _ => MouseAction::Motion,
            };
            session.mouse(MouseEvent { action, button, mods: mods(shift, ctrl, alt), x, y, any_button_pressed: any }, CELL)
        }
        Command::Theme(theme) => session.set_theme(&theme),
        Command::Kill => {
            session.kill();
            Ok(())
        }
    }
    .map_err(|e| {
        let _ = out; // errors are reported on stdout like everything else
        e
    })
}

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let (mut cols, mut rows, mut cwd) = (80u16, 24u16, None);
    while args.first().is_some_and(|a| a.starts_with("--") && a != "--") {
        let flag = args.remove(0);
        if args.is_empty() {
            usage();
        }
        let val = args.remove(0);
        match flag.as_str() {
            "--cols" => cols = val.parse().unwrap_or_else(|_| usage()),
            "--rows" => rows = val.parse().unwrap_or_else(|_| usage()),
            "--cwd" => cwd = Some(val),
            _ => usage(),
        }
    }
    if args.first().map(String::as_str) == Some("--") {
        args.remove(0);
    }
    if args.is_empty() {
        usage();
    }

    let stdout = Arc::new(Mutex::new(std::io::stdout()));
    let (exit_tx, exit_rx) = mpsc::channel::<Option<u32>>();
    let exit_tx = Mutex::new(exit_tx);
    let emit: Emit = {
        let stdout = Arc::clone(&stdout);
        Arc::new(move |output: Output| {
            if let Ok(json) = serde_json::to_string(&output) {
                let mut out = stdout.lock().unwrap();
                let _ = writeln!(out, "{json}");
                let _ = out.flush();
            }
            if let Output::Exit { code } = output {
                let _ = exit_tx.lock().unwrap().send(code);
            }
        })
    };

    let mut opts = Options::new(args.remove(0));
    opts.args = args;
    opts.cols = cols;
    opts.rows = rows;
    opts.cwd = cwd;
    opts.env.push(("TERM_PROGRAM".into(), "vt-host".into()));
    let session = match Session::start(opts, Arc::clone(&emit)) {
        Ok(s) => s,
        Err(message) => {
            println!("{}", serde_json::json!({"t": "error", "message": message}));
            std::process::exit(127);
        }
    };

    // Commands arrive on stdin; EOF means the owner went away, so take the program with us.
    let reader_session = Arc::clone(&session);
    let reader_emit = Arc::clone(&emit);
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines() {
            let Ok(line) = line else { break };
            if line.trim().is_empty() {
                continue;
            }
            if let Err(message) = handle(&reader_session, &line, &reader_emit) {
                println!("{}", serde_json::json!({"t": "error", "message": message}));
            }
        }
        reader_session.stop();
    });

    let code = exit_rx.recv().ok().flatten().unwrap_or(1);
    std::process::exit(code as i32);
}
