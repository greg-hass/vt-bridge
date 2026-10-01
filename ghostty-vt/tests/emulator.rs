use ghostty_vt::*;

fn screen(t: &mut Terminal) -> Vec<String> {
    t.snapshot(false).unwrap().rows.iter().map(|r| r.text()).collect()
}

#[test]
fn plain_text_and_newlines() {
    let mut t = Terminal::new(20, 4).unwrap();
    t.feed(b"hello\r\nworld");
    let s = screen(&mut t);
    assert_eq!(s[0], "hello");
    assert_eq!(s[1], "world");
}

#[test]
fn colours_and_styles_become_runs() {
    let mut t = Terminal::new(30, 2).unwrap();
    t.feed(b"a \x1b[1;38;2;255;128;0mBOLD\x1b[0m z");
    let snap = t.snapshot(false).unwrap();
    let runs = &snap.rows[0].runs;
    assert_eq!(runs.len(), 3, "{runs:?}");
    assert_eq!(runs[1].text, "BOLD");
    assert_eq!(runs[1].fg, Some(Rgb(255, 128, 0)));
    assert!(runs[1].flags.bold);
    assert_eq!(runs[1].x, 2);
}

#[test]
fn palette_colours_resolve_to_rgb() {
    let mut t = Terminal::new(10, 1).unwrap();
    t.feed(b"\x1b[31mred");
    let snap = t.snapshot(false).unwrap();
    assert!(snap.rows[0].runs[0].fg.is_some());
}

#[test]
fn wide_characters_take_two_columns() {
    let mut t = Terminal::new(10, 1).unwrap();
    t.feed("a日本b".as_bytes());
    let snap = t.snapshot(false).unwrap();
    let row = &snap.rows[0];
    let total: u16 = row.runs.iter().map(|r| r.width).sum();
    assert_eq!(total, 6);
    assert_eq!(row.text(), "a日本b");
}

#[test]
fn queries_are_answered() {
    let mut t = Terminal::new(10, 3).unwrap();
    t.feed(b"\x1b[3;5H\x1b[6n"); // move cursor, ask for position
    assert_eq!(t.take_replies(), b"\x1b[3;5R");
    t.feed(b"\x1b[c"); // primary device attributes
    assert!(t.take_replies().starts_with(b"\x1b[?"));
}

#[test]
fn events_title_bell_notification_prompt() {
    let mut t = Terminal::new(10, 3).unwrap();
    t.feed(b"\x1b]0;my title\x07\x07");
    t.feed(b"\x1b]777;notify;Build;done\x07");
    t.feed(b"\x1b]133;A\x07\x1b]133;B\x07\x1b]133;C\x07\x1b]133;D;2\x07");
    assert_eq!(t.title(), "my title");
    let ev = t.take_events();
    assert!(ev.contains(&Event::TitleChanged), "{ev:?}");
    assert!(ev.contains(&Event::Bell), "{ev:?}");
    assert!(ev.contains(&Event::Notification { title: "Build".into(), body: "done".into() }), "{ev:?}");
    assert!(ev.contains(&Event::Prompt(PromptEvent::CommandEnd { exit_code: Some(2) })), "{ev:?}");
    assert!(ev.contains(&Event::Prompt(PromptEvent::PromptStart)), "{ev:?}");
}

#[test]
fn dirty_snapshots_report_only_changes() {
    let mut t = Terminal::new(10, 4).unwrap();
    t.feed(b"one\r\ntwo");
    let first = t.snapshot(true).unwrap();
    assert!(first.rows.len() >= 2);
    let idle = t.snapshot(true).unwrap();
    assert_eq!(idle.dirty, Dirty::Clean);
    assert!(idle.rows.is_empty(), "{:?}", idle.rows);
    t.feed(b"\x1b[4;1Hx");
    let next = t.snapshot(true).unwrap();
    // The new text, plus the row the cursor left (it must be redrawn without the cursor).
    let ys: Vec<u16> = next.rows.iter().map(|r| r.y).collect();
    assert_eq!(ys, vec![1, 3]);
    assert_eq!(next.rows[1].text(), "x");
}

#[test]
fn resize_reflows() {
    let mut t = Terminal::new(10, 3).unwrap();
    t.feed(b"abcdefghij1234");
    t.resize(20, 3, (8, 16)).unwrap();
    assert_eq!(t.size(), (20, 3));
    assert_eq!(screen(&mut t)[0], "abcdefghij1234");
}

#[test]
fn alternate_screen_restores() {
    let mut t = Terminal::new(10, 3).unwrap();
    t.feed(b"main");
    t.feed(b"\x1b[?1049h\x1b[2J\x1b[Halt");
    assert_eq!(screen(&mut t)[0], "alt");
    t.feed(b"\x1b[?1049l");
    assert_eq!(screen(&mut t)[0], "main");
}

#[test]
fn keys_follow_terminal_modes() {
    let mut t = Terminal::new(10, 3).unwrap();
    let none = Mods::default();
    assert_eq!(t.encode_key("ArrowUp", None, none, KeyAction::Press), b"\x1b[A");
    t.feed(b"\x1b[?1h"); // application cursor keys
    assert_eq!(t.encode_key("ArrowUp", None, none, KeyAction::Press), b"\x1bOA");
    assert_eq!(t.encode_key("KeyA", Some("a"), none, KeyAction::Press), b"a");
    let ctrl = Mods { ctrl: true, ..none };
    assert_eq!(t.encode_key("KeyC", Some("c"), ctrl, KeyAction::Press), b"\x03");
    assert_eq!(t.encode_key("Enter", None, none, KeyAction::Press), b"\r");
    assert_eq!(t.encode_key("NotAKey", None, none, KeyAction::Press), b"");
}

#[test]
fn kitty_keyboard_protocol_is_honoured() {
    let mut t = Terminal::new(10, 3).unwrap();
    // Program pushes Kitty flags: disambiguate escape codes.
    t.feed(b"\x1b[>1u");
    let esc = t.encode_key("Escape", None, Mods::default(), KeyAction::Press);
    assert_eq!(esc, b"\x1b[27u");
}

fn click(x: f32, y: f32, action: MouseAction, button: Option<u8>) -> MouseEvent {
    MouseEvent { action, button, mods: Mods::default(), x, y, any_button_pressed: action == MouseAction::Press }
}

#[test]
fn mouse_is_reported_only_when_the_program_asks() {
    let mut t = Terminal::new(80, 24).unwrap();
    assert!(!t.mouse_tracking());
    assert!(t.encode_mouse(click(44.0, 56.0, MouseAction::Press, Some(1)), (8, 16)).is_empty());
    t.feed(b"\x1b[?1000h\x1b[?1006h"); // normal tracking, SGR format
    assert!(t.mouse_tracking());
    // Pixel (44, 56) with 8x16 cells is column 5, row 3 (zero based) -> 6;4 one based.
    assert_eq!(t.encode_mouse(click(44.0, 56.0, MouseAction::Press, Some(1)), (8, 16)), b"\x1b[<0;6;4M");
    assert_eq!(t.encode_mouse(click(44.0, 56.0, MouseAction::Release, Some(1)), (8, 16)), b"\x1b[<0;6;4m");
    // Wheel up is button 4 -> code 64.
    assert_eq!(t.encode_mouse(click(0.0, 0.0, MouseAction::Press, Some(4)), (8, 16)), b"\x1b[<64;1;1M");
}

#[test]
fn paste_is_bracketed_on_request_and_sanitised() {
    let mut t = Terminal::new(10, 3).unwrap();
    assert_eq!(t.encode_paste("a\nb"), b"a\rb");
    t.feed(b"\x1b[?2004h");
    assert_eq!(t.encode_paste("a\nb"), b"\x1b[200~a\nb\x1b[201~");
    assert_eq!(t.encode_paste("x\x1b[31my"), b"\x1b[200~x [31my\x1b[201~");
}

#[test]
fn focus_reports_follow_mode_1004() {
    let mut t = Terminal::new(10, 3).unwrap();
    assert!(t.encode_focus(true).is_empty());
    t.feed(b"\x1b[?1004h");
    assert_eq!(t.encode_focus(true), b"\x1b[I");
    assert_eq!(t.encode_focus(false), b"\x1b[O");
}

#[test]
fn scrollback_and_scroll_offset() {
    let mut t = Terminal::new(10, 3).unwrap();
    for i in 0..20 {
        t.feed(format!("line{i}\r\n").as_bytes());
    }
    let at_bottom = t.snapshot(false).unwrap();
    assert!(at_bottom.scrollbar.total > 3);
    assert_eq!(at_bottom.scrollbar.offset + at_bottom.scrollbar.len, at_bottom.scrollbar.total);
    t.scroll(-5);
    let up = t.snapshot(false).unwrap();
    assert_eq!(up.scrollbar.offset, at_bottom.scrollbar.offset - 5);
    assert_eq!(up.dirty, Dirty::Full);
    assert!(up.rows[0].text().starts_with("line"));
    assert_ne!(up.rows[0].text(), at_bottom.rows[0].text());
}

#[test]
fn theme_colours_apply_to_defaults_and_palette() {
    let mut t = Terminal::new(10, 2).unwrap();
    let mut ansi = [Rgb(0, 0, 0); 16];
    ansi[1] = Rgb(200, 10, 20);
    t.set_colors(Rgb(1, 2, 3), Rgb(4, 5, 6), Rgb(7, 8, 9), &ansi);
    t.feed(b"\x1b[31mred");
    let s = t.snapshot(false).unwrap();
    assert_eq!((s.fg, s.bg), (Rgb(1, 2, 3), Rgb(4, 5, 6)));
    assert_eq!(s.cursor_color, Some(Rgb(7, 8, 9)));
    assert_eq!(s.rows[0].runs[0].fg, Some(Rgb(200, 10, 20)));
}

#[test]
fn alt_screen_flag() {
    let mut t = Terminal::new(10, 2).unwrap();
    assert!(!t.alt_screen());
    t.feed(b"\x1b[?1049h");
    assert!(t.alt_screen());
}

#[test]
fn non_ascii_runs_carry_per_column_cells() {
    let mut t = Terminal::new(10, 1).unwrap();
    t.feed("a日b─".as_bytes());
    let snap = t.snapshot(false).unwrap();
    let run = &snap.rows[0].runs[0];
    assert_eq!(run.cells.as_deref().unwrap(), ["a", "日", "", "b", "─"]);
    assert_eq!(run.width, 5);
    // Plain ASCII doesn't pay for it.
    t.feed(b"\x1b[2J\x1b[Hplain");
    assert!(t.snapshot(false).unwrap().rows[0].runs[0].cells.is_none());
}

#[cfg(feature = "serde")]
#[test]
fn events_and_snapshots_serialise_for_the_webview() {
    let mut t = Terminal::new(10, 2).unwrap();
    t.feed(b"\x1b]133;D;3\x07\x1b[?1000hhi");
    let ev = t.take_events();
    let json = serde_json::to_value(&ev).unwrap();
    assert!(json.to_string().contains(r#""type":"prompt""#), "{json}");
    assert!(json.to_string().contains(r#""step":"command_end""#), "{json}");
    assert!(json.to_string().contains(r#""exit_code":3"#), "{json}");
    let snap = serde_json::to_value(t.snapshot(false).unwrap()).unwrap();
    assert_eq!(snap["mouse_tracking"], true);
    assert_eq!(snap["rows"][0]["runs"][0]["text"], "hi");
    assert_eq!(snap["fg"].as_array().unwrap().len(), 3);
    assert_eq!(snap["cursor"]["style"], "block");
}
