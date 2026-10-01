//! Safe wrapper over libghostty-vt: a headless terminal emulator.
//!
//! Feed it bytes from a PTY with [`Terminal::feed`], send back whatever
//! [`Terminal::take_replies`] returns, and draw [`Terminal::snapshot`].

use std::os::raw::c_void;
use std::ptr;

use ghostty_vt_sys as sys;

#[cfg(feature = "serde")]
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize))]
pub struct Rgb(pub u8, pub u8, pub u8);

impl From<sys::GhosttyColorRgb> for Rgb {
    fn from(c: sys::GhosttyColorRgb) -> Self {
        Rgb(c.r, c.g, c.b)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[cfg_attr(feature = "serde", derive(Serialize))]
pub struct Flags {
    pub bold: bool,
    pub italic: bool,
    pub faint: bool,
    pub blink: bool,
    pub inverse: bool,
    pub invisible: bool,
    pub strike: bool,
    pub overline: bool,
    /// 0 none, 1 single, 2 double, 3 curly, 4 dotted, 5 dashed.
    pub underline: u8,
}

/// A run of adjacent cells with identical style. `x` is the column of the first cell;
/// wide characters occupy two columns but appear once in `text`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize))]
pub struct Run {
    pub x: u16,
    pub text: String,
    pub width: u16,
    /// Per-column text, present only when `text` isn't plain ASCII (wide characters, emoji,
    /// combining marks): one entry per column, with "" for the second half of a wide
    /// character. Lets a renderer pin every glyph to its column instead of trusting font advances.
    #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
    pub cells: Option<Vec<String>>,
    /// Resolved colours; `None` means the terminal default.
    pub fg: Option<Rgb>,
    pub bg: Option<Rgb>,
    pub flags: Flags,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize))]
pub struct Row {
    pub y: u16,
    pub runs: Vec<Run>,
}

impl Row {
    pub fn text(&self) -> String {
        let mut s = String::new();
        let mut col = 0u16;
        for r in &self.runs {
            while col < r.x {
                s.push(' ');
                col += 1;
            }
            s.push_str(&r.text);
            col = r.x + r.width;
        }
        s
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
pub enum CursorStyle {
    Bar,
    Block,
    Underline,
    BlockHollow,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize))]
pub struct Cursor {
    pub x: u16,
    pub y: u16,
    pub style: CursorStyle,
    pub visible: bool,
    pub blinking: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
pub enum Dirty {
    Clean,
    Partial,
    Full,
}

/// What to draw. With `only_dirty`, `rows` holds just the changed rows.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize))]
pub struct Snapshot {
    pub cols: u16,
    pub rows_total: u16,
    pub dirty: Dirty,
    pub fg: Rgb,
    pub bg: Rgb,
    pub cursor: Option<Cursor>,
    /// Explicit cursor colour set by the program or [`Terminal::set_colors`].
    pub cursor_color: Option<Rgb>,
    pub scrollbar: Scrollbar,
    /// The program asked for mouse reports (clicks should go to it, not to selection).
    pub mouse_tracking: bool,
    /// The program is using the alternate screen (a full-screen TUI).
    pub alt_screen: bool,
    pub rows: Vec<Row>,
}

/// Scrollback geometry: `offset` is the first visible line out of `total`, `len` lines visible.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize))]
pub struct Scrollbar {
    pub total: u64,
    pub offset: u64,
    pub len: u64,
}

/// Things the program running in the terminal asked the host to do.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize))]
#[cfg_attr(feature = "serde", serde(tag = "type", rename_all = "snake_case"))]
pub enum Event {
    Bell,
    TitleChanged,
    PwdChanged,
    Notification { title: String, body: String },
    /// OSC 133 shell integration.
    Prompt(PromptEvent),
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize))]
#[cfg_attr(feature = "serde", serde(tag = "step", rename_all = "snake_case"))]
pub enum PromptEvent {
    PromptStart,
    InputStart,
    OutputStart { command: String },
    CommandEnd { exit_code: Option<i32> },
}

#[derive(Default)]
struct Shared {
    replies: Vec<u8>,
    events: Vec<Event>,
}

unsafe fn lossy(s: sys::GhosttyString) -> String {
    if s.ptr.is_null() || s.len == 0 {
        return String::new();
    }
    String::from_utf8_lossy(std::slice::from_raw_parts(s.ptr, s.len)).into_owned()
}

unsafe fn shared<'a>(ud: *mut c_void) -> &'a mut Shared {
    &mut *(ud as *mut Shared)
}

unsafe extern "C" fn cb_write_pty(_t: sys::GhosttyTerminal, ud: *mut c_void, data: *const u8, len: usize) {
    shared(ud).replies.extend_from_slice(std::slice::from_raw_parts(data, len));
}
unsafe extern "C" fn cb_bell(_t: sys::GhosttyTerminal, ud: *mut c_void) {
    shared(ud).events.push(Event::Bell);
}
unsafe extern "C" fn cb_title(_t: sys::GhosttyTerminal, ud: *mut c_void) {
    shared(ud).events.push(Event::TitleChanged);
}
unsafe extern "C" fn cb_pwd(_t: sys::GhosttyTerminal, ud: *mut c_void) {
    shared(ud).events.push(Event::PwdChanged);
}
unsafe extern "C" fn cb_notify(
    _t: sys::GhosttyTerminal,
    ud: *mut c_void,
    n: *const sys::GhosttyTerminalDesktopNotification,
) {
    let n = &*n;
    shared(ud).events.push(Event::Notification { title: lossy(n.title), body: lossy(n.body) });
}
unsafe extern "C" fn cb_prompt(
    _t: sys::GhosttyTerminal,
    ud: *mut c_void,
    e: *const sys::GhosttyTerminalSemanticPrompt,
) {
    use sys::GhosttySemanticPromptKind as K;
    let e = &*e;
    let ev = match e.kind {
        K::GHOSTTY_SEMANTIC_PROMPT_PROMPT_START => PromptEvent::PromptStart,
        K::GHOSTTY_SEMANTIC_PROMPT_INPUT_START => PromptEvent::InputStart,
        K::GHOSTTY_SEMANTIC_PROMPT_OUTPUT_START => PromptEvent::OutputStart { command: lossy(e.command) },
        K::GHOSTTY_SEMANTIC_PROMPT_COMMAND_END => {
            PromptEvent::CommandEnd { exit_code: e.has_exit_code.then_some(e.exit_code) }
        }
        _ => return,
    };
    shared(ud).events.push(Event::Prompt(ev));
}

#[derive(Debug)]
pub struct Error(pub i32);
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "libghostty-vt error {}", self.0)
    }
}
impl std::error::Error for Error {}

fn check(r: sys::GhosttyResult::Type) -> Result<(), Error> {
    if r == sys::GhosttyResult::GHOSTTY_SUCCESS { Ok(()) } else { Err(Error(r)) }
}

macro_rules! sized {
    ($t:ty) => {{
        let mut v = <$t>::default();
        v.size = std::mem::size_of::<$t>();
        v
    }};
}

pub struct Terminal {
    term: sys::GhosttyTerminal,
    render: sys::GhosttyRenderState,
    rows_it: sys::GhosttyRenderStateRowIterator,
    cells: sys::GhosttyRenderStateRowCells,
    encoder: sys::GhosttyKeyEncoder,
    key_event: sys::GhosttyKeyEvent,
    mouse_encoder: sys::GhosttyMouseEncoder,
    mouse_event: sys::GhosttyMouseEvent,
    // Boxed so the pointer handed to C stays valid when `Terminal` moves.
    shared: Box<Shared>,
    cols: u16,
    rows: u16,
}

// The C handles are only touched through &mut self.
unsafe impl Send for Terminal {}

impl Terminal {
    pub fn new(cols: u16, rows: u16) -> Result<Self, Error> {
        unsafe {
            let mut shared = Box::<Shared>::default();
            let (mut term, mut render, mut rows_it, mut cells) = (ptr::null_mut(), ptr::null_mut(), ptr::null_mut(), ptr::null_mut());
            let (mut encoder, mut key_event) = (ptr::null_mut(), ptr::null_mut());
            let (mut mouse_encoder, mut mouse_event) = (ptr::null_mut(), ptr::null_mut());
            check(sys::ghostty_terminal_new(ptr::null(), &mut term, cols, rows))?;
            check(sys::ghostty_render_state_new(ptr::null(), &mut render))?;
            check(sys::ghostty_render_state_row_iterator_new(ptr::null(), &mut rows_it))?;
            check(sys::ghostty_render_state_row_cells_new(ptr::null(), &mut cells))?;
            check(sys::ghostty_key_encoder_new(ptr::null(), &mut encoder))?;
            check(sys::ghostty_key_event_new(ptr::null(), &mut key_event))?;
            check(sys::ghostty_mouse_encoder_new(ptr::null(), &mut mouse_encoder))?;
            check(sys::ghostty_mouse_event_new(ptr::null(), &mut mouse_event))?;

            use sys::GhosttyTerminalOption as O;
            let ud = &mut *shared as *mut Shared as *mut c_void;
            sys::ghostty_terminal_set(term, O::GHOSTTY_TERMINAL_OPT_USERDATA, ud);
            let set = |opt, f: *const c_void| {
                sys::ghostty_terminal_set(term, opt, f);
            };
            set(O::GHOSTTY_TERMINAL_OPT_WRITE_PTY, cb_write_pty as *const c_void);
            set(O::GHOSTTY_TERMINAL_OPT_BELL, cb_bell as *const c_void);
            set(O::GHOSTTY_TERMINAL_OPT_TITLE_CHANGED, cb_title as *const c_void);
            set(O::GHOSTTY_TERMINAL_OPT_PWD_CHANGED, cb_pwd as *const c_void);
            set(O::GHOSTTY_TERMINAL_OPT_DESKTOP_NOTIFICATION, cb_notify as *const c_void);
            set(O::GHOSTTY_TERMINAL_OPT_SEMANTIC_PROMPT, cb_prompt as *const c_void);

            Ok(Terminal { term, render, rows_it, cells, encoder, key_event, mouse_encoder, mouse_event, shared, cols, rows })
        }
    }

    pub fn size(&self) -> (u16, u16) {
        (self.cols, self.rows)
    }

    /// Process output from the program (bytes read from the PTY).
    pub fn feed(&mut self, bytes: &[u8]) {
        unsafe { sys::ghostty_terminal_vt_write(self.term, bytes.as_ptr(), bytes.len()) }
    }

    /// Bytes the terminal wants written back to the PTY (query replies). Drain after `feed`.
    pub fn take_replies(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.shared.replies)
    }

    pub fn take_events(&mut self) -> Vec<Event> {
        std::mem::take(&mut self.shared.events)
    }

    /// `cell_px` is the cell size in pixels, used for pixel-size reports and graphics.
    pub fn resize(&mut self, cols: u16, rows: u16, cell_px: (u32, u32)) -> Result<(), Error> {
        check(unsafe { sys::ghostty_terminal_resize(self.term, cols, rows, cell_px.0, cell_px.1) })?;
        self.cols = cols;
        self.rows = rows;
        Ok(())
    }

    /// Scroll the viewport through scrollback; negative is up.
    pub fn scroll(&mut self, delta: isize) {
        let mut s = sys::GhosttyTerminalScrollViewport::default();
        s.tag = sys::GhosttyTerminalScrollViewportTag::GHOSTTY_SCROLL_VIEWPORT_DELTA;
        s.value.delta = delta;
        unsafe { sys::ghostty_terminal_scroll_viewport(self.term, s) }
    }

    pub fn title(&self) -> String {
        unsafe {
            let mut s = sys::GhosttyString { ptr: ptr::null(), len: 0 };
            sys::ghostty_terminal_get(
                self.term,
                sys::GhosttyTerminalData::GHOSTTY_TERMINAL_DATA_TITLE,
                &mut s as *mut _ as *mut c_void,
            );
            lossy(s)
        }
    }

    /// Capture what to draw. Pass `only_dirty` to get only changed rows (and mark them clean).
    pub fn snapshot(&mut self, only_dirty: bool) -> Result<Snapshot, Error> {
        unsafe {
            use sys::GhosttyRenderStateData as D;
            check(sys::ghostty_render_state_update(self.render, self.term))?;

            let mut dirty_raw = 0;
            sys::ghostty_render_state_get(self.render, D::GHOSTTY_RENDER_STATE_DATA_DIRTY, &mut dirty_raw as *mut _ as *mut c_void);
            let dirty = match dirty_raw {
                sys::GhosttyRenderStateDirty::GHOSTTY_RENDER_STATE_DIRTY_FALSE => Dirty::Clean,
                sys::GhosttyRenderStateDirty::GHOSTTY_RENDER_STATE_DIRTY_PARTIAL => Dirty::Partial,
                _ => Dirty::Full,
            };

            let mut colors = sized!(sys::GhosttyRenderStateColors);
            check(sys::ghostty_render_state_get(self.render, D::GHOSTTY_RENDER_STATE_DATA_COLORS, &mut colors as *mut _ as *mut c_void))?;

            let mut cur = sized!(sys::GhosttyRenderStateCursor);
            check(sys::ghostty_render_state_get(self.render, D::GHOSTTY_RENDER_STATE_DATA_CURSOR, &mut cur as *mut _ as *mut c_void))?;
            let cursor = cur.viewport_has_value.then(|| Cursor {
                x: cur.viewport_x,
                y: cur.viewport_y,
                visible: cur.visible,
                blinking: cur.blinking,
                style: match cur.visual_style {
                    sys::GhosttyRenderStateCursorVisualStyle::GHOSTTY_RENDER_STATE_CURSOR_VISUAL_STYLE_BAR => CursorStyle::Bar,
                    sys::GhosttyRenderStateCursorVisualStyle::GHOSTTY_RENDER_STATE_CURSOR_VISUAL_STYLE_UNDERLINE => CursorStyle::Underline,
                    sys::GhosttyRenderStateCursorVisualStyle::GHOSTTY_RENDER_STATE_CURSOR_VISUAL_STYLE_BLOCK_HOLLOW => CursorStyle::BlockHollow,
                    _ => CursorStyle::Block,
                },
            });

            check(sys::ghostty_render_state_get(self.render, D::GHOSTTY_RENDER_STATE_DATA_ROW_ITERATOR, &mut self.rows_it as *mut _ as *mut c_void))?;

            let mut rows = Vec::new();
            let partial = only_dirty && dirty != Dirty::Full;
            loop {
                let mut dirty_row = 0u16;
                let more = if partial {
                    sys::ghostty_render_state_row_iterator_next_dirty(self.rows_it, &mut dirty_row)
                } else {
                    sys::ghostty_render_state_row_iterator_next(self.rows_it)
                };
                if !more {
                    break;
                }
                let mut y = 0u16;
                sys::ghostty_render_state_row_get(
                    self.rows_it,
                    sys::GhosttyRenderStateRowData::GHOSTTY_RENDER_STATE_ROW_DATA_VIEWPORT_Y,
                    &mut y as *mut _ as *mut c_void,
                );
                rows.push(self.read_row(y, &colors)?);
                // Mark the row clean so the next update reports only new changes.
                let clean = false;
                sys::ghostty_render_state_row_set(
                    self.rows_it,
                    sys::GhosttyRenderStateRowOption::GHOSTTY_RENDER_STATE_ROW_OPTION_DIRTY,
                    &clean as *const bool as *const c_void,
                );
            }
            // Reset the global dirty flag.
            let clean = sys::GhosttyRenderStateDirty::GHOSTTY_RENDER_STATE_DIRTY_FALSE;
            sys::ghostty_render_state_set(
                self.render,
                sys::GhosttyRenderStateOption::GHOSTTY_RENDER_STATE_OPTION_DIRTY,
                &clean as *const _ as *const c_void,
            );

            Ok(Snapshot {
                cols: self.cols,
                rows_total: self.rows,
                dirty,
                fg: colors.foreground.into(),
                bg: colors.background.into(),
                cursor,
                cursor_color: colors.cursor_has_value.then(|| colors.cursor.into()),
                scrollbar: self.scrollbar(),
                mouse_tracking: self.mouse_tracking(),
                alt_screen: self.alt_screen(),
                rows,
            })
        }
    }

    unsafe fn read_row(&mut self, y: u16, colors: &sys::GhosttyRenderStateColors) -> Result<Row, Error> {
        check(sys::ghostty_render_state_row_get(
            self.rows_it,
            sys::GhosttyRenderStateRowData::GHOSTTY_RENDER_STATE_ROW_DATA_CELLS,
            &mut self.cells as *mut _ as *mut c_void,
        ))?;

        let mut runs: Vec<Run> = Vec::new();
        let mut run_cells: Vec<Vec<String>> = Vec::new();
        let mut x: u16 = 0;
        while sys::ghostty_render_state_row_cells_next(self.cells) {
            let cx = x;
            x += 1;

            // Wide characters: the second cell is a spacer that carries no text.
            let mut raw: u64 = 0;
            sys::ghostty_render_state_row_cells_get(
                self.cells,
                sys::GhosttyRenderStateRowCellsData::GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_RAW,
                &mut raw as *mut _ as *mut c_void,
            );
            let mut wide: i32 = 0;
            sys::ghostty_cell_get(raw, sys::GhosttyCellData::GHOSTTY_CELL_DATA_WIDE, &mut wide as *mut _ as *mut c_void);
            if wide == sys::GhosttyCellWide::GHOSTTY_CELL_WIDE_SPACER_TAIL || wide == sys::GhosttyCellWide::GHOSTTY_CELL_WIDE_SPACER_HEAD {
                continue;
            }
            let width: u16 = if wide == sys::GhosttyCellWide::GHOSTTY_CELL_WIDE_WIDE { 2 } else { 1 };

            let mut len: u32 = 0;
            sys::ghostty_render_state_row_cells_get(
                self.cells,
                sys::GhosttyRenderStateRowCellsData::GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_GRAPHEMES_LEN,
                &mut len as *mut _ as *mut c_void,
            );
            let mut text = String::new();
            if len > 0 {
                let mut cps = vec![0u32; len as usize];
                sys::ghostty_render_state_row_cells_get(
                    self.cells,
                    sys::GhosttyRenderStateRowCellsData::GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_GRAPHEMES_BUF,
                    cps.as_mut_ptr() as *mut c_void,
                );
                text.extend(cps.into_iter().filter_map(char::from_u32));
            } else {
                text.push(' ');
            }

            let mut st = sized!(sys::GhosttyStyle);
            sys::ghostty_render_state_row_cells_get(
                self.cells,
                sys::GhosttyRenderStateRowCellsData::GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_STYLE,
                &mut st as *mut _ as *mut c_void,
            );
            let resolve = |c: sys::GhosttyStyleColor| match c.tag {
                sys::GhosttyStyleColorTag::GHOSTTY_STYLE_COLOR_RGB => Some(Rgb::from(c.value.rgb)),
                sys::GhosttyStyleColorTag::GHOSTTY_STYLE_COLOR_PALETTE => Some(Rgb::from(colors.palette[c.value.palette as usize])),
                _ => None,
            };
            let (fg, bg) = (resolve(st.fg_color), resolve(st.bg_color));
            let flags = Flags {
                bold: st.bold,
                italic: st.italic,
                faint: st.faint,
                blink: st.blink,
                inverse: st.inverse,
                invisible: st.invisible,
                strike: st.strikethrough,
                overline: st.overline,
                underline: st.underline as u8,
            };

            let mut cells = vec![text.clone()];
            if width == 2 {
                cells.push(String::new());
            }
            match runs.last_mut() {
                Some(r) if r.fg == fg && r.bg == bg && r.flags == flags && r.x + r.width == cx => {
                    r.text.push_str(&text);
                    r.width += width;
                    run_cells.last_mut().unwrap().extend(cells);
                }
                _ => {
                    runs.push(Run { x: cx, text, width, cells: None, fg, bg, flags });
                    run_cells.push(cells);
                }
            }
        }

        // Trailing blank, unstyled cells aren't worth sending.
        if let Some(r) = runs.last_mut() {
            if r.bg.is_none() && r.flags == Flags::default() {
                let trimmed = r.text.trim_end_matches(' ').len();
                let cells = run_cells.last_mut().unwrap();
                while cells.last().is_some_and(|c| c == " ") {
                    cells.pop();
                }
                r.width -= (r.text.len() - trimmed) as u16;
                r.text.truncate(trimmed);
                if r.text.is_empty() {
                    runs.pop();
                    run_cells.pop();
                }
            }
        }
        for (r, cells) in runs.iter_mut().zip(run_cells) {
            if !r.text.is_ascii() {
                r.cells = Some(cells);
            }
        }
        Ok(Row { y, runs })
    }

    /// Encode a key press into the bytes to send to the PTY, honouring the modes the
    /// running program enabled (application cursor keys, Kitty keyboard protocol...).
    /// `code` is a W3C `KeyboardEvent.code` such as "KeyA" or "ArrowUp"; `text` is the
    /// layout-produced text (`KeyboardEvent.key` for printable keys), if any.
    pub fn encode_key(&mut self, code: &str, text: Option<&str>, mods: Mods, action: KeyAction) -> Vec<u8> {
        let Some(&(_, key)) = sys::KEY_CODES.iter().find(|(c, _)| *c == code) else {
            return Vec::new();
        };
        unsafe {
            sys::ghostty_key_encoder_setopt_from_terminal(self.encoder, self.term);
            sys::ghostty_key_event_set_action(
                self.key_event,
                match action {
                    KeyAction::Press => sys::GhosttyKeyAction::GHOSTTY_KEY_ACTION_PRESS,
                    KeyAction::Release => sys::GhosttyKeyAction::GHOSTTY_KEY_ACTION_RELEASE,
                    KeyAction::Repeat => sys::GhosttyKeyAction::GHOSTTY_KEY_ACTION_REPEAT,
                },
            );
            sys::ghostty_key_event_set_key(self.key_event, key);
            sys::ghostty_key_event_set_mods(self.key_event, mods.bits());
            // Shift is already reflected in `text`, so it isn't a consumed modifier to the encoder.
            sys::ghostty_key_event_set_consumed_mods(self.key_event, if text.is_some() { mods.bits() & sys::GHOSTTY_MODS_SHIFT as u16 } else { 0 });
            match text {
                Some(t) => sys::ghostty_key_event_set_utf8(self.key_event, t.as_ptr() as *const _, t.len()),
                None => sys::ghostty_key_event_set_utf8(self.key_event, ptr::null(), 0),
            }
            let mut buf = [0u8; 128];
            let mut n = 0usize;
            let r = sys::ghostty_key_encoder_encode(self.encoder, self.key_event, buf.as_mut_ptr() as *mut _, buf.len(), &mut n);
            if r == sys::GhosttyResult::GHOSTTY_SUCCESS { buf[..n].to_vec() } else { Vec::new() }
        }
    }
}

impl Terminal {
    fn mode(&self, value: u16) -> bool {
        let mut m = sys::GhosttyTerminalModeConfig { mode: value & 0x7FFF, value: false };
        unsafe {
            sys::ghostty_terminal_get(self.term, sys::GhosttyTerminalData::GHOSTTY_TERMINAL_DATA_MODE, &mut m as *mut _ as *mut c_void);
        }
        m.value
    }

    pub fn scrollbar(&self) -> Scrollbar {
        let mut sb = sys::GhosttyTerminalScrollbar::default();
        unsafe {
            sys::ghostty_terminal_get(self.term, sys::GhosttyTerminalData::GHOSTTY_TERMINAL_DATA_SCROLLBAR, &mut sb as *mut _ as *mut c_void);
        }
        Scrollbar { total: sb.total, offset: sb.offset, len: sb.len }
    }

    /// True when the program asked for mouse reports (so clicks go to it, not to selection).
    pub fn mouse_tracking(&self) -> bool {
        let mut on = false;
        unsafe {
            sys::ghostty_terminal_get(self.term, sys::GhosttyTerminalData::GHOSTTY_TERMINAL_DATA_MOUSE_TRACKING, &mut on as *mut _ as *mut c_void);
        }
        on
    }

    /// True while the program uses the alternate screen (full-screen TUIs).
    pub fn alt_screen(&self) -> bool {
        let mut screen = 0;
        unsafe {
            sys::ghostty_terminal_get(self.term, sys::GhosttyTerminalData::GHOSTTY_TERMINAL_DATA_ACTIVE_SCREEN, &mut screen as *mut _ as *mut c_void);
        }
        screen == sys::GhosttyTerminalScreen::GHOSTTY_TERMINAL_SCREEN_ALTERNATE
    }

    /// Whether the program wants focus-in/out reports (mode 1004).
    pub fn wants_focus_events(&self) -> bool {
        self.mode(1004)
    }

    /// Set the default colours and the first 16 palette entries (the theme).
    pub fn set_colors(&mut self, fg: Rgb, bg: Rgb, cursor: Rgb, ansi: &[Rgb; 16]) {
        unsafe {
            let mut palette = [sys::GhosttyColorRgb::default(); 256];
            sys::ghostty_terminal_get(
                self.term,
                sys::GhosttyTerminalData::GHOSTTY_TERMINAL_DATA_COLOR_PALETTE_DEFAULT,
                palette.as_mut_ptr() as *mut c_void,
            );
            for (i, c) in ansi.iter().enumerate() {
                palette[i] = sys::GhosttyColorRgb { r: c.0, g: c.1, b: c.2 };
            }
            use sys::GhosttyTerminalOption as O;
            let rgb = |c: Rgb| sys::GhosttyColorRgb { r: c.0, g: c.1, b: c.2 };
            sys::ghostty_terminal_set(self.term, O::GHOSTTY_TERMINAL_OPT_COLOR_PALETTE, palette.as_ptr() as *const c_void);
            sys::ghostty_terminal_set(self.term, O::GHOSTTY_TERMINAL_OPT_COLOR_FOREGROUND, &rgb(fg) as *const _ as *const c_void);
            sys::ghostty_terminal_set(self.term, O::GHOSTTY_TERMINAL_OPT_COLOR_BACKGROUND, &rgb(bg) as *const _ as *const c_void);
            sys::ghostty_terminal_set(self.term, O::GHOSTTY_TERMINAL_OPT_COLOR_CURSOR, &rgb(cursor) as *const _ as *const c_void);
        }
    }

    /// Bytes to send for pasted text: control bytes stripped, bracketed when the program asked.
    pub fn encode_paste(&self, text: &str) -> Vec<u8> {
        let mut data = text.as_bytes().to_vec();
        let bracketed = self.mode(2004);
        let mut out = vec![0u8; data.len() + 32];
        let mut n = 0usize;
        unsafe {
            let r = sys::ghostty_paste_encode(data.as_mut_ptr() as *mut _, data.len(), bracketed, out.as_mut_ptr() as *mut _, out.len(), &mut n);
            if r == sys::GhosttyResult::GHOSTTY_SUCCESS { out.truncate(n); out } else { Vec::new() }
        }
    }

    /// Focus gained/lost report; empty unless the program enabled focus reporting.
    pub fn encode_focus(&self, gained: bool) -> Vec<u8> {
        if !self.wants_focus_events() {
            return Vec::new();
        }
        let ev = if gained { sys::GhosttyFocusEvent::GHOSTTY_FOCUS_GAINED } else { sys::GhosttyFocusEvent::GHOSTTY_FOCUS_LOST };
        let mut buf = [0u8; 16];
        let mut n = 0usize;
        let r = unsafe { sys::ghostty_focus_encode(ev, buf.as_mut_ptr() as *mut _, buf.len(), &mut n) };
        if r == sys::GhosttyResult::GHOSTTY_SUCCESS { buf[..n].to_vec() } else { Vec::new() }
    }

    /// Encode a mouse event for a program that enabled mouse reporting. Positions are in
    /// pixels relative to the terminal surface; `cell_px` and the surface size map them to cells.
    /// Returns nothing when the program isn't tracking the mouse or the event isn't reportable.
    pub fn encode_mouse(&mut self, ev: MouseEvent, cell_px: (u32, u32)) -> Vec<u8> {
        if !self.mouse_tracking() {
            return Vec::new();
        }
        unsafe {
            sys::ghostty_mouse_encoder_setopt_from_terminal(self.mouse_encoder, self.term);
            let mut size = sized!(sys::GhosttyMouseEncoderSize);
            size.screen_width = self.cols as u32 * cell_px.0;
            size.screen_height = self.rows as u32 * cell_px.1;
            size.cell_width = cell_px.0;
            size.cell_height = cell_px.1;
            sys::ghostty_mouse_encoder_setopt(
                self.mouse_encoder,
                sys::GhosttyMouseEncoderOption::GHOSTTY_MOUSE_ENCODER_OPT_SIZE,
                &size as *const _ as *const c_void,
            );
            let any = ev.any_button_pressed;
            sys::ghostty_mouse_encoder_setopt(
                self.mouse_encoder,
                sys::GhosttyMouseEncoderOption::GHOSTTY_MOUSE_ENCODER_OPT_ANY_BUTTON_PRESSED,
                &any as *const bool as *const c_void,
            );
            use sys::GhosttyMouseAction as A;
            sys::ghostty_mouse_event_set_action(
                self.mouse_event,
                match ev.action {
                    MouseAction::Press => A::GHOSTTY_MOUSE_ACTION_PRESS,
                    MouseAction::Release => A::GHOSTTY_MOUSE_ACTION_RELEASE,
                    MouseAction::Motion => A::GHOSTTY_MOUSE_ACTION_MOTION,
                },
            );
            match ev.button {
                Some(b) => sys::ghostty_mouse_event_set_button(self.mouse_event, b as _),
                None => sys::ghostty_mouse_event_clear_button(self.mouse_event),
            }
            sys::ghostty_mouse_event_set_mods(self.mouse_event, ev.mods.bits());
            sys::ghostty_mouse_event_set_position(self.mouse_event, sys::GhosttyMousePosition { x: ev.x, y: ev.y });
            let mut buf = [0u8; 64];
            let mut n = 0usize;
            let r = sys::ghostty_mouse_encoder_encode(self.mouse_encoder, self.mouse_event, buf.as_mut_ptr() as *mut _, buf.len(), &mut n);
            if r == sys::GhosttyResult::GHOSTTY_SUCCESS { buf[..n].to_vec() } else { Vec::new() }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseAction {
    Press,
    Release,
    Motion,
}

/// Buttons as GhosttyMouseButton: 1 left, 2 right, 3 middle, 4/5 wheel up/down.
#[derive(Debug, Clone, Copy)]
pub struct MouseEvent {
    pub action: MouseAction,
    pub button: Option<u8>,
    pub mods: Mods,
    pub x: f32,
    pub y: f32,
    pub any_button_pressed: bool,
}

impl Drop for Terminal {
    fn drop(&mut self) {
        unsafe {
            sys::ghostty_mouse_event_free(self.mouse_event);
            sys::ghostty_mouse_encoder_free(self.mouse_encoder);
            sys::ghostty_key_event_free(self.key_event);
            sys::ghostty_key_encoder_free(self.encoder);
            sys::ghostty_render_state_row_cells_free(self.cells);
            sys::ghostty_render_state_row_iterator_free(self.rows_it);
            sys::ghostty_render_state_free(self.render);
            sys::ghostty_terminal_free(self.term);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyAction {
    Press,
    Release,
    Repeat,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Mods {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub super_: bool,
}

impl Mods {
    fn bits(self) -> u16 {
        (self.shift as u16) * sys::GHOSTTY_MODS_SHIFT as u16
            | (self.ctrl as u16) * sys::GHOSTTY_MODS_CTRL as u16
            | (self.alt as u16) * sys::GHOSTTY_MODS_ALT as u16
            | (self.super_ as u16) * sys::GHOSTTY_MODS_SUPER as u16
    }
}
