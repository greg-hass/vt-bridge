//! End to end through the real binary: JSON in, JSON out, a real PTY in the middle.
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

struct Host {
    child: Child,
    stdin: ChildStdin,
    rx: Receiver<Value>,
    screen: Vec<String>,
    messages: Vec<Value>,
}

impl Host {
    fn start(args: &[&str]) -> Host {
        let mut child = Command::new(env!("CARGO_BIN_EXE_vt-host"))
            .args(["--cols", "60", "--rows", "8", "--"])
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                let v: Value = serde_json::from_str(&line).unwrap_or_else(|e| panic!("not JSON: {line:?}: {e}"));
                if tx.send(v).is_err() {
                    break;
                }
            }
        });
        Host { child, stdin, rx, screen: vec![String::new(); 8], messages: vec![] }
    }

    fn send(&mut self, v: Value) {
        writeln!(self.stdin, "{v}").unwrap();
    }

    fn wait_for(&mut self, what: &str, done: impl Fn(&Host) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if done(self) {
                return;
            }
            if let Ok(v) = self.rx.recv_timeout(Duration::from_millis(50)) {
                if v["t"] == "frame" {
                    self.screen.resize(v["rows_total"].as_u64().unwrap() as usize, String::new());
                    if v["dirty"] == "full" {
                        self.screen.iter_mut().for_each(String::clear);
                    }
                    for row in v["rows"].as_array().unwrap() {
                        let mut line = String::new();
                        for run in row["runs"].as_array().unwrap() {
                            while line.chars().count() < run["x"].as_u64().unwrap() as usize {
                                line.push(' ');
                            }
                            line.push_str(run["text"].as_str().unwrap());
                        }
                        self.screen[row["y"].as_u64().unwrap() as usize] = line;
                    }
                } else {
                    self.messages.push(v);
                }
            }
        }
        panic!("timed out waiting for {what}; screen:\n{}\nmessages: {:?}", self.screen.join("\n"), self.messages);
    }

    fn has_line(&self, s: &str) -> bool {
        self.screen.iter().any(|l| l.trim() == s)
    }
    fn key(&mut self, code: &str, text: Option<&str>) {
        self.send(json!({"t": "key", "code": code, "text": text}));
    }
}

#[test]
fn frames_arrive_and_exit_code_is_forwarded() {
    let mut h = Host::start(&["/bin/sh", "-c", "echo from-the-host; exit 3"]);
    h.wait_for("output", |h| h.has_line("from-the-host"));
    h.wait_for("exit message", |h| h.messages.iter().any(|m| m["t"] == "exit" && m["code"] == 3));
    assert_eq!(h.child.wait().unwrap().code(), Some(3));
}

#[test]
fn a_password_prompt_is_answered_over_the_pipe() {
    let mut h = Host::start(&["/bin/sh", "-c", "stty -echo; printf 'Password: '; read -r pw; stty echo; echo \"len=${#pw}\""]);
    h.wait_for("prompt", |h| h.screen.iter().any(|l| l.contains("Password:")));
    for (code, text) in [("KeyA", "a"), ("KeyB", "b"), ("KeyC", "c")] {
        h.key(code, Some(text));
    }
    h.key("Enter", None);
    h.wait_for("accepted", |h| h.screen.iter().any(|l| l.contains("len=3")));
    assert!(!h.screen.iter().any(|l| l.contains("abc")), "secret must not be echoed");
}

#[test]
fn paste_text_resize_and_title_commands_work() {
    let mut h = Host::start(&["/bin/sh"]);
    h.send(json!({"t": "text", "data": "echo viatext\n"}));
    h.wait_for("text", |h| h.has_line("viatext"));
    h.send(json!({"t": "resize", "cols": 40, "rows": 5, "cw": 8, "ch": 16}));
    h.send(json!({"t": "text", "data": "stty size\n"}));
    h.wait_for("resized", |h| h.has_line("5 40"));
    h.send(json!({"t": "title"}));
    h.wait_for("title reply", |h| h.messages.iter().any(|m| m["t"] == "title"));
}

#[test]
fn bad_commands_are_reported_and_do_not_kill_the_host() {
    let mut h = Host::start(&["/bin/sh"]);
    h.send(json!({"t": "nonsense"}));
    h.send(json!({"t": "key"}));
    h.wait_for("errors", |h| h.messages.iter().filter(|m| m["t"] == "error").count() >= 2);
    h.send(json!({"t": "text", "data": "echo still-alive\n"}));
    h.wait_for("still alive", |h| h.has_line("still-alive"));
}

#[test]
fn closing_stdin_takes_the_program_down() {
    let mut h = Host::start(&["/bin/sh", "-c", "sleep 30"]);
    std::thread::sleep(Duration::from_millis(200));
    drop(h.stdin);
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if h.child.try_wait().unwrap().is_some() {
            break;
        }
        assert!(Instant::now() < deadline, "vt-host should exit when stdin closes");
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn a_missing_program_is_an_error_not_a_hang() {
    let mut h = Host::start(&["/definitely/not/a/program"]);
    let status = h.child.wait().unwrap();
    assert!(!status.success());
    drop(h.rx);
}
