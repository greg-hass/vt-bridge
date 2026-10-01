//! A terminal session: a program running in a PTY, driven through libghostty-vt.
//!
//! Output bytes go reader thread -> emulator; a flusher thread turns changed rows into
//! [`Output::Frame`]s (coalesced to ~120/s); all input is serialised through one writer thread so
//! a large paste can never deadlock against the emulator's query replies.
use ghostty_vt::{Event, KeyAction, Mods, MouseAction, MouseEvent, Rgb, Snapshot, Terminal};
use portable_pty::{native_pty_system, ChildKiller, CommandBuilder, MasterPty, PtySize};
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::Duration;

pub use ghostty_vt;

/// Coalescing delay between a burst of output and the frame sent to the view.
pub const FRAME_INTERVAL: Duration = Duration::from_millis(8);

/// What a session tells its owner.
#[derive(Serialize, Clone, Debug)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum Output {
    Frame(Snapshot),
    Event(Event),
    /// The program ended. `code` is its exit status when known.
    Exit { code: Option<u32> },
}

pub type Emit = Arc<dyn Fn(Output) + Send + Sync>;

/// Colours as `#rrggbb`.
#[derive(Deserialize, Clone, Debug)]
pub struct Theme {
    pub fg: String,
    pub bg: String,
    pub cursor: String,
    pub ansi: Vec<String>,
}

pub struct Options {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: Option<String>,
    pub cols: u16,
    pub rows: u16,
    pub env: Vec<(String, String)>,
    pub theme: Option<Theme>,
}

impl Options {
    pub fn new(program: impl Into<String>) -> Self {
        Options { program: program.into(), args: vec![], cwd: None, cols: 80, rows: 24, env: vec![], theme: None }
    }
}

#[derive(Default)]
struct FrameSignal {
    state: Mutex<SignalState>,
    wake: Condvar,
}

#[derive(Default)]
struct SignalState {
    pending: bool,
    closed: bool,
}

impl FrameSignal {
    fn notify(&self) {
        self.state.lock().unwrap().pending = true;
        self.wake.notify_one();
    }
    fn close(&self) {
        self.state.lock().unwrap().closed = true;
        self.wake.notify_one();
    }
    /// Blocks until there is something to send; false once closed.
    fn wait(&self) -> bool {
        let mut st = self.state.lock().unwrap();
        while !st.pending && !st.closed {
            st = self.wake.wait(st).unwrap();
        }
        let open = !st.closed || st.pending;
        st.pending = false;
        open
    }
}

pub struct Session {
    term: Arc<Mutex<Terminal>>,
    input: Sender<Vec<u8>>,
    master: Mutex<Box<dyn MasterPty + Send>>,
    killer: Mutex<Box<dyn ChildKiller + Send + Sync>>,
    signal: Arc<FrameSignal>,
    emit: Emit,
}

fn parse_hex(value: &str) -> Option<Rgb> {
    let v = value.trim().strip_prefix('#')?;
    if v.len() != 6 {
        return None;
    }
    let n = u32::from_str_radix(v, 16).ok()?;
    Some(Rgb((n >> 16) as u8, (n >> 8) as u8, n as u8))
}

pub fn apply_theme(term: &mut Terminal, theme: &Theme) -> Result<(), String> {
    let bad = || "Invalid terminal theme".to_string();
    if theme.ansi.len() != 16 {
        return Err(bad());
    }
    let mut ansi = [Rgb(0, 0, 0); 16];
    for (slot, c) in ansi.iter_mut().zip(&theme.ansi) {
        *slot = parse_hex(c).ok_or_else(bad)?;
    }
    term.set_colors(
        parse_hex(&theme.fg).ok_or_else(bad)?,
        parse_hex(&theme.bg).ok_or_else(bad)?,
        parse_hex(&theme.cursor).ok_or_else(bad)?,
        &ansi,
    );
    Ok(())
}

fn pty_size(cols: u16, rows: u16) -> PtySize {
    PtySize { rows: rows.max(1), cols: cols.max(1), pixel_width: 0, pixel_height: 0 }
}

const GONE: &str = "Terminal session is no longer running";
const UNAVAILABLE: &str = "Terminal is unavailable";

impl Session {
    pub fn start(opts: Options, emit: Emit) -> Result<Arc<Session>, String> {
        let (cols, rows) = (opts.cols.max(1), opts.rows.max(1));
        let pair = native_pty_system()
            .openpty(pty_size(cols, rows))
            .map_err(|e| format!("Unable to create terminal: {e}"))?;

        let mut command = CommandBuilder::new(&opts.program);
        command.args(&opts.args);
        if let Some(cwd) = &opts.cwd {
            command.cwd(cwd);
        }
        command.env("TERM", "xterm-256color");
        command.env("COLORTERM", "truecolor");
        for (k, v) in &opts.env {
            command.env(k, v);
        }
        let mut child = pair
            .slave
            .spawn_command(command)
            .map_err(|e| format!("Unable to start {}: {e}", opts.program))?;
        let mut reader = pair.master.try_clone_reader().map_err(|e| format!("Unable to read terminal output: {e}"))?;
        let mut writer = pair.master.take_writer().map_err(|e| format!("Unable to write to terminal: {e}"))?;
        let killer = child.clone_killer();

        let mut term = Terminal::new(cols, rows).map_err(|e| format!("Unable to start emulator: {e}"))?;
        if let Some(theme) = &opts.theme {
            apply_theme(&mut term, theme)?;
        }
        let term = Arc::new(Mutex::new(term));
        let signal = Arc::new(FrameSignal::default());
        let (input, input_rx) = mpsc::channel::<Vec<u8>>();

        let session = Arc::new(Session {
            term: Arc::clone(&term),
            input: input.clone(),
            master: Mutex::new(pair.master),
            killer: Mutex::new(killer),
            signal: Arc::clone(&signal),
            emit: Arc::clone(&emit),
        });

        // Writer: every byte for the program goes through here, in order.
        std::thread::spawn(move || {
            while let Ok(bytes) = input_rx.recv() {
                if writer.write_all(&bytes).and_then(|_| writer.flush()).is_err() {
                    break;
                }
            }
        });

        // Flusher: send changed rows, at most one frame per interval.
        {
            let (term, signal, emit) = (Arc::clone(&term), Arc::clone(&signal), Arc::clone(&emit));
            std::thread::spawn(move || {
                let mut last = None;
                while signal.wait() {
                    std::thread::sleep(FRAME_INTERVAL);
                    let (snapshot, events) = {
                        let mut t = match term.lock() {
                            Ok(t) => t,
                            Err(_) => break,
                        };
                        (t.snapshot(true), t.take_events())
                    };
                    if let Ok(snapshot) = snapshot {
                        // Rows changed, or a mode / scrollbar change the view must know about.
                        let meta = (snapshot.mouse_tracking, snapshot.alt_screen, snapshot.scrollbar);
                        if !snapshot.rows.is_empty() || last != Some(meta) {
                            last = Some(meta);
                            emit(Output::Frame(snapshot));
                        }
                    }
                    for event in events {
                        emit(Output::Event(event));
                    }
                }
            });
        }

        // Reader: program output into the emulator; its replies back to the program.
        std::thread::spawn(move || {
            let mut buffer = [0_u8; 32 * 1024];
            loop {
                match reader.read(&mut buffer) {
                    Ok(0) | Err(_) => break,
                    Ok(size) => {
                        let replies = match term.lock() {
                            Ok(mut t) => {
                                t.feed(&buffer[..size]);
                                t.take_replies()
                            }
                            Err(_) => break,
                        };
                        if !replies.is_empty() {
                            let _ = input.send(replies);
                        }
                        signal.notify();
                    }
                }
            }
            let code = child.wait().ok().map(|s| s.exit_code());
            // Let the flusher send the last output before the exit notice.
            std::thread::sleep(FRAME_INTERVAL * 2);
            signal.close();
            emit(Output::Exit { code });
        });

        Ok(session)
    }

    fn term(&self) -> Result<MutexGuard<'_, Terminal>, String> {
        self.term.lock().map_err(|_| UNAVAILABLE.to_string())
    }

    fn send(&self, bytes: Vec<u8>) -> Result<(), String> {
        if bytes.is_empty() {
            return Ok(());
        }
        self.input.send(bytes).map_err(|_| GONE.to_string())
    }

    /// Raw text for the program (e.g. an IME commit).
    pub fn write_text(&self, text: &str) -> Result<(), String> {
        self.send(text.as_bytes().to_vec())
    }

    pub fn key(&self, code: &str, text: Option<&str>, m: Mods, repeat: bool) -> Result<(), String> {
        let action = if repeat { KeyAction::Repeat } else { KeyAction::Press };
        let mut bytes = self.term()?.encode_key(code, text, m, action);
        // Virtual, remote and on-screen keyboards can report a key code the encoder has no text
        // for. A plain printable character must still arrive, so fall back to the text itself.
        if bytes.is_empty() && !m.ctrl && !m.alt && !m.super_ {
            if let Some(t) = text.filter(|t| !t.is_empty() && !t.chars().any(char::is_control)) {
                bytes = t.as_bytes().to_vec();
            }
        }
        self.send(bytes)
    }

    pub fn paste(&self, text: &str) -> Result<(), String> {
        let bytes = self.term()?.encode_paste(text);
        self.send(bytes)
    }

    pub fn focus(&self, gained: bool) -> Result<(), String> {
        let bytes = self.term()?.encode_focus(gained);
        self.send(bytes)
    }

    pub fn mouse(&self, event: MouseEvent, cell: (u32, u32)) -> Result<(), String> {
        let bytes = self.term()?.encode_mouse(event, (cell.0.max(1), cell.1.max(1)));
        self.send(bytes)
    }

    /// Wheel scrolling by `lines` (negative = up): the program's mouse reports if it wants them,
    /// arrow keys on the alternate screen (like most terminals), otherwise the scrollback.
    pub fn wheel(&self, lines: i32, x: f32, y: f32, cell: (u32, u32), shift: bool) -> Result<(), String> {
        if lines == 0 {
            return Ok(());
        }
        let mut term = self.term()?;
        let steps = lines.unsigned_abs().min(20);
        let up = lines < 0;
        let mut out = Vec::new();
        if term.mouse_tracking() && !shift {
            let ev = MouseEvent {
                action: MouseAction::Press,
                button: Some(if up { 4 } else { 5 }),
                mods: Mods::default(),
                x,
                y,
                any_button_pressed: false,
            };
            for _ in 0..steps {
                out.extend(term.encode_mouse(ev, (cell.0.max(1), cell.1.max(1))));
            }
        } else if term.alt_screen() {
            let key = if up { "ArrowUp" } else { "ArrowDown" };
            for _ in 0..steps {
                out.extend(term.encode_key(key, None, Mods::default(), KeyAction::Press));
            }
        } else {
            term.scroll(lines as isize);
            drop(term);
            self.signal.notify();
            return Ok(());
        }
        drop(term);
        self.send(out)
    }

    pub fn resize(&self, cols: u16, rows: u16, cell: (u32, u32)) -> Result<(), String> {
        let (cols, rows) = (cols.max(1), rows.max(1));
        self.master
            .lock()
            .map_err(|_| UNAVAILABLE.to_string())?
            .resize(pty_size(cols, rows))
            .map_err(|e| format!("Unable to resize terminal: {e}"))?;
        self.term()?
            .resize(cols, rows, (cell.0.max(1), cell.1.max(1)))
            .map_err(|e| format!("Unable to resize terminal: {e}"))?;
        self.signal.notify();
        Ok(())
    }

    pub fn set_theme(&self, theme: &Theme) -> Result<(), String> {
        {
            let mut term = self.term()?;
            apply_theme(&mut term, theme)?;
        }
        // Every cell may resolve to a new colour: ask for a full redraw.
        self.redraw();
        Ok(())
    }

    /// Send the whole current screen (after a theme change, or to a view that mounted late).
    pub fn redraw(&self) {
        let snapshot = self.term().and_then(|mut t| t.snapshot(false).map_err(|e| e.to_string()));
        if let Ok(snapshot) = snapshot {
            (self.emit)(Output::Frame(snapshot));
        }
    }

    pub fn title(&self) -> String {
        self.term().map(|t| t.title()).unwrap_or_default()
    }

    /// Scroll the scrollback viewport directly.
    pub fn scroll(&self, delta: isize) -> Result<(), String> {
        self.term()?.scroll(delta);
        self.signal.notify();
        Ok(())
    }

    pub fn kill(&self) {
        if let Ok(mut k) = self.killer.lock() {
            let _ = k.kill();
        }
    }

    /// Stop frames and kill the program (the exit notice still follows).
    pub fn stop(&self) {
        self.signal.close();
        self.kill();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::Receiver;
    use std::time::Instant;

    struct Harness {
        session: Arc<Session>,
        rx: Receiver<Output>,
        screen: Vec<String>,
        mouse_tracking: bool,
        events: Vec<Event>,
        exit: Option<Option<u32>>,
    }

    impl Harness {
        fn start(program: &str, args: &[&str]) -> Harness {
            let (tx, rx) = mpsc::channel();
            let tx = Mutex::new(tx);
            let emit: Emit = Arc::new(move |o| {
                let _ = tx.lock().unwrap().send(o);
            });
            let mut opts = Options::new(program);
            opts.args = args.iter().map(|s| s.to_string()).collect();
            opts.cols = 60;
            opts.rows = 8;
            opts.cwd = Some("/tmp".into());
            let session = Session::start(opts, emit).unwrap();
            Harness { session, rx, screen: vec![String::new(); 8], mouse_tracking: false, events: vec![], exit: None }
        }

        fn wait_for(&mut self, what: &str, done: impl Fn(&Harness) -> bool) {
            let deadline = Instant::now() + Duration::from_secs(10);
            while Instant::now() < deadline {
                if done(self) {
                    return;
                }
                if let Ok(o) = self.rx.recv_timeout(Duration::from_millis(50)) {
                    self.absorb(o);
                }
            }
            panic!("timed out waiting for {what}; screen:\n{}", self.screen.join("\n"));
        }

        fn absorb(&mut self, o: Output) {
            match o {
                Output::Frame(f) => {
                    self.mouse_tracking = f.mouse_tracking;
                    self.screen.resize(f.rows_total as usize, String::new());
                    if f.dirty == ghostty_vt::Dirty::Full {
                        self.screen.iter_mut().for_each(String::clear);
                    }
                    for row in &f.rows {
                        self.screen[row.y as usize] = row.text();
                    }
                }
                Output::Event(e) => self.events.push(e),
                Output::Exit { code } => self.exit = Some(code),
            }
        }

        fn has_line(&self, exact: &str) -> bool {
            self.screen.iter().any(|l| l.trim() == exact)
        }
        fn has(&self, t: &str) -> bool {
            self.screen.iter().any(|l| l.contains(t))
        }
        fn type_text(&self, text: &str) {
            for ch in text.chars() {
                let (code, t) = match ch {
                    '\n' => ("Enter".to_string(), None),
                    ' ' => ("Space".to_string(), Some(" ".to_string())),
                    c if c.is_ascii_lowercase() => (format!("Key{}", c.to_ascii_uppercase()), Some(c.to_string())),
                    c if c.is_ascii_digit() => (format!("Digit{c}"), Some(c.to_string())),
                    '-' => ("Minus".to_string(), Some("-".to_string())),
                    c => panic!("type_text: unsupported {c:?}"),
                };
                self.session.key(&code, t.as_deref(), Mods::default(), false).unwrap();
            }
        }
        fn run(&self, line: &str) {
            self.session.paste(line).unwrap();
            self.type_text("\n");
        }
    }

    #[test]
    fn typed_keys_reach_the_program_and_output_comes_back() {
        let mut h = Harness::start("/bin/sh", &[]);
        h.type_text("echo pi-ok\n");
        h.wait_for("output", |h| h.has_line("pi-ok"));
    }

    #[test]
    fn a_command_runs_to_completion_and_reports_its_exit_code() {
        let mut h = Harness::start("/bin/sh", &["-c", "echo working; exit 7"]);
        h.wait_for("exit", |h| h.exit.is_some());
        assert_eq!(h.exit, Some(Some(7)));
        assert!(h.has_line("working"), "output must arrive before the exit notice:\n{}", h.screen.join("\n"));
    }

    #[test]
    fn a_password_style_prompt_can_be_answered() {
        // sudo reads from the terminal with echo off; the same shape.
        let mut h = Harness::start("/bin/sh", &["-c", "stty -echo; printf 'Password: '; read -r pw; stty echo; echo \"len=${#pw}\""]);
        h.wait_for("prompt", |h| h.has("Password:"));
        h.type_text("hunter2\n");
        h.wait_for("answer accepted", |h| h.has("len=7"));
        assert!(!h.has("hunter2"), "typed secret must not be echoed:\n{}", h.screen.join("\n"));
    }

    #[test]
    fn terminal_replies_to_queries_go_back_to_the_program() {
        let mut h = Harness::start("/bin/sh", &[]);
        h.type_text("stty -echo\n");
        h.run("printf '\\033[6n'; read -r -d R x; echo got-${x#*;}");
        h.wait_for("cursor report", |h| h.has_line("got-1"));
    }

    #[test]
    fn bracketed_paste_waits_for_enter_and_resize_reaches_the_program() {
        let mut h = Harness::start("/bin/sh", &[]);
        h.run("stty size");
        h.wait_for("initial size", |h| h.has_line("8 60"));
        h.session.resize(40, 5, (8, 16)).unwrap();
        h.run("stty size");
        h.wait_for("resized size", |h| h.has_line("5 40"));
    }

    #[test]
    fn mouse_mode_is_reported_and_clicks_are_encoded() {
        let mut h = Harness::start("/bin/sh", &[]);
        h.type_text("stty -echo\n");
        h.run("printf '\\033[?1000h\\033[?1006h'; read -r -n 9 m; printf '\\033[?1000l'; printf 'MOUSE:%s\\n' \"$(printf %s \"$m\" | od -An -c | tr -d ' \\n')\"");
        h.wait_for("mouse mode", |h| h.mouse_tracking);
        let click = MouseEvent { action: MouseAction::Press, button: Some(1), mods: Mods::default(), x: 44.0, y: 56.0, any_button_pressed: true };
        h.session.mouse(click, (8, 16)).unwrap();
        h.wait_for("click seen", |h| h.has_line("MOUSE:033[<0;6;4M"));
    }

    #[test]
    fn notifications_and_titles_arrive_as_events() {
        let mut h = Harness::start("/bin/sh", &["-c", "printf '\\033]0;job title\\007\\033]777;notify;Done;all good\\007'; sleep 1"]);
        h.wait_for("events", |h| h.events.len() >= 2);
        assert!(h.events.contains(&Event::TitleChanged));
        assert!(h.events.contains(&Event::Notification { title: "Done".into(), body: "all good".into() }));
        assert_eq!(h.session.title(), "job title");
    }

    #[test]
    fn printable_text_survives_an_unusable_key_code() {
        let mut h = Harness::start("/bin/sh", &[]);
        for (code, text) in [("ControlRight", "e"), ("Bogus", "c"), ("ShiftLeft", "h"), ("", "o")] {
            h.session.key(code, Some(text), Mods::default(), false).unwrap();
        }
        h.type_text("\n");
        h.wait_for("echoed text", |h| h.has("echo"));
    }

    #[test]
    fn stop_ends_a_long_running_program() {
        let mut h = Harness::start("/bin/sh", &["-c", "sleep 30"]);
        std::thread::sleep(Duration::from_millis(200));
        h.session.stop();
        h.wait_for("exit after kill", |h| h.exit.is_some());
    }

    #[test]
    fn theme_needs_sixteen_colours() {
        let mut term = Terminal::new(10, 2).unwrap();
        let mut theme = Theme { fg: "#ffffff".into(), bg: "#000000".into(), cursor: "#ff0000".into(), ansi: vec!["#000000".into(); 15] };
        assert!(apply_theme(&mut term, &theme).is_err());
        theme.ansi.push("#ffffff".into());
        assert!(apply_theme(&mut term, &theme).is_ok());
        assert!(parse_hex("rgb(1,2,3)").is_none());
    }
}
