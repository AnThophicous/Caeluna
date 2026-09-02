//! A small, bounded terminal core for the graphical shell.
//!
//! Linux sessions use a real POSIX PTY and launch the user's shell with an
//! argument vector. The screen/parser state is platform independent, while
//! Windows deliberately returns [`TerminalError::UnsupportedPlatform`] for
//! process sessions. Test-only input/output injection exists so the parser and
//! tab lifecycle can be tested without pretending that Windows has a PTY.

use std::collections::VecDeque;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
#[cfg(target_os = "linux")]
use std::time::{Duration, Instant};

pub type SessionId = u64;

pub const DEFAULT_COLUMNS: u16 = 120;
pub const DEFAULT_ROWS: u16 = 40;
/// A conservative default for small machines. Larger histories remain
/// available through explicit configuration, but the default must not
/// multiply into hundreds of megabytes across several tabs.
pub const DEFAULT_SCROLLBACK_LINES: usize = 4_000;
pub const MAX_SCROLLBACK_LINES: usize = 100_000;
pub const MAX_TABS: usize = 32;
pub const MAX_COLUMNS: u16 = 512;
pub const MAX_ROWS: u16 = 256;
const MAX_INPUT_BYTES: usize = 1024 * 1024;
const MAX_CONTROL_SEQUENCE_BYTES: usize = 4096;
#[cfg(target_os = "linux")]
/// Bounded PTY backpressure keeps a busy command from growing memory without
/// limit; the reader thread may wait, but the compositor never does.
const OUTPUT_QUEUE_CAPACITY: usize = 32;
#[cfg(target_os = "linux")]
const OUTPUT_CHUNK_SIZE: usize = 8 * 1024;

#[derive(Debug)]
pub enum TerminalError {
    InvalidSize,
    InvalidScrollback,
    InvalidTitle,
    InvalidPath(String),
    ShellNotFound,
    InputTooLarge { max: usize },
    TooManyTabs { max: usize },
    Closed,
    UnsupportedPlatform,
    Io(io::Error),
}

impl fmt::Display for TerminalError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidSize => formatter.write_str("terminal size must be non-zero"),
            Self::InvalidScrollback => formatter.write_str("scrollback limit is invalid"),
            Self::InvalidTitle => {
                formatter.write_str("terminal title is empty or contains a control character")
            }
            Self::InvalidPath(path) => write!(formatter, "terminal path is invalid: {path}"),
            Self::ShellNotFound => formatter.write_str("no executable shell was found"),
            Self::InputTooLarge { max } => write!(formatter, "terminal input exceeds the {max}-byte limit"),
            Self::TooManyTabs { max } => write!(formatter, "terminal tab limit reached ({max})"),
            Self::Closed => formatter.write_str("terminal session is closed"),
            Self::UnsupportedPlatform => formatter.write_str("real PTY sessions require Linux"),
            Self::Io(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for TerminalError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}

impl From<io::Error> for TerminalError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TerminalConfig {
    pub columns: u16,
    pub rows: u16,
    pub scrollback_lines: usize,
    pub cwd: Option<PathBuf>,
    pub shell: Option<PathBuf>,
    pub title: Option<String>,
}

impl Default for TerminalConfig {
    fn default() -> Self {
        Self {
            columns: DEFAULT_COLUMNS,
            rows: DEFAULT_ROWS,
            scrollback_lines: DEFAULT_SCROLLBACK_LINES,
            cwd: None,
            shell: None,
            title: None,
        }
    }
}

impl TerminalConfig {
    pub fn new(columns: u16, rows: u16) -> Result<Self, TerminalError> {
        let config = Self {
            columns,
            rows,
            ..Self::default()
        };
        config.validate()?;
        Ok(config)
    }

    pub fn with_scrollback_lines(mut self, lines: usize) -> Result<Self, TerminalError> {
        if lines > MAX_SCROLLBACK_LINES {
            return Err(TerminalError::InvalidScrollback);
        }
        self.scrollback_lines = lines;
        Ok(self)
    }

    pub fn with_cwd(mut self, cwd: impl Into<PathBuf>) -> Self {
        self.cwd = Some(cwd.into());
        self
    }

    pub fn with_shell(mut self, shell: impl Into<PathBuf>) -> Self {
        self.shell = Some(shell.into());
        self
    }

    pub fn with_title(mut self, title: impl Into<String>) -> Result<Self, TerminalError> {
        let title = sanitize_title(&title.into()).ok_or(TerminalError::InvalidTitle)?;
        self.title = Some(title);
        Ok(self)
    }

    fn validate(&self) -> Result<(), TerminalError> {
        if self.columns == 0 || self.rows == 0 || self.columns > MAX_COLUMNS || self.rows > MAX_ROWS {
            return Err(TerminalError::InvalidSize);
        }
        if self.scrollback_lines > MAX_SCROLLBACK_LINES {
            return Err(TerminalError::InvalidScrollback);
        }
        if let Some(title) = &self.title {
            if sanitize_title(title).is_none() {
                return Err(TerminalError::InvalidTitle);
            }
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TerminalColor {
    Default,
    Indexed(u8),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CellAttributes {
    pub foreground: TerminalColor,
    pub background: TerminalColor,
    pub bold: bool,
    pub dim: bool,
    pub italic: bool,
    pub underline: bool,
    pub inverse: bool,
}

impl Default for CellAttributes {
    fn default() -> Self {
        Self {
            foreground: TerminalColor::Default,
            background: TerminalColor::Default,
            bold: false,
            dim: false,
            italic: false,
            underline: false,
            inverse: false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cell {
    pub character: char,
    pub attributes: CellAttributes,
}

impl Cell {
    fn blank(attributes: CellAttributes) -> Self {
        Self {
            character: ' ',
            attributes,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScreenLine {
    cells: Vec<Cell>,
}

impl ScreenLine {
    fn blank(columns: usize, attributes: CellAttributes) -> Self {
        Self {
            cells: vec![Cell::blank(attributes); columns],
        }
    }

    pub fn cells(&self) -> &[Cell] {
        &self.cells
    }

    pub fn text(&self) -> String {
        self.cells.iter().map(|cell| cell.character).collect()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CursorPosition {
    pub row: usize,
    pub column: usize,
    pub visible: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TerminalScreen {
    columns: usize,
    rows: usize,
    visible: Vec<ScreenLine>,
    scrollback: VecDeque<ScreenLine>,
    scrollback_limit: usize,
    cursor: CursorPosition,
    saved_cursor: Option<CursorPosition>,
    attributes: CellAttributes,
    tab_stops: Vec<bool>,
    wrap_pending: bool,
}

impl TerminalScreen {
    pub fn new(columns: u16, rows: u16, scrollback_limit: usize) -> Result<Self, TerminalError> {
        if columns == 0 || rows == 0 || columns > MAX_COLUMNS || rows > MAX_ROWS {
            return Err(TerminalError::InvalidSize);
        }
        if scrollback_limit > MAX_SCROLLBACK_LINES {
            return Err(TerminalError::InvalidScrollback);
        }

        let columns = columns as usize;
        let rows = rows as usize;
        let attributes = CellAttributes::default();
        Ok(Self {
            columns,
            rows,
            visible: vec![ScreenLine::blank(columns, attributes); rows],
            scrollback: VecDeque::with_capacity(scrollback_limit.min(1024)),
            scrollback_limit,
            cursor: CursorPosition {
                row: 0,
                column: 0,
                visible: true,
            },
            saved_cursor: None,
            attributes,
            tab_stops: (0..columns).map(|column| column % 8 == 0).collect(),
            wrap_pending: false,
        })
    }

    pub fn columns(&self) -> usize {
        self.columns
    }

    pub fn rows(&self) -> usize {
        self.rows
    }

    pub fn cursor(&self) -> CursorPosition {
        self.cursor
    }

    pub fn visible_lines(&self) -> &[ScreenLine] {
        &self.visible
    }

    pub fn scrollback_lines(&self) -> std::collections::vec_deque::Iter<'_, ScreenLine> {
        self.scrollback.iter()
    }

    pub fn scrollback_len(&self) -> usize {
        self.scrollback.len()
    }

    pub fn line_texts(&self) -> Vec<String> {
        self.visible.iter().map(ScreenLine::text).collect()
    }

    pub fn resize(&mut self, columns: u16, rows: u16) -> Result<(), TerminalError> {
        if columns == 0 || rows == 0 || columns > MAX_COLUMNS || rows > MAX_ROWS {
            return Err(TerminalError::InvalidSize);
        }

        let columns = columns as usize;
        let rows = rows as usize;
        let mut lines = VecDeque::from(std::mem::take(&mut self.visible));
        // A geometry change is not terminal output. Do not feed lines removed
        // by a resize into scrollback: repeated drag-resizes would otherwise
        // turn blank/reflowed rows into fake history and evict useful output.
        // Keeping the newest rows is the least surprising bounded fallback
        // until full terminal line reflow is implemented.
        while lines.len() > rows {
            lines.pop_front();
        }
        while lines.len() < rows {
            lines.push_back(ScreenLine::blank(columns, self.attributes));
        }
        for line in &mut lines {
            line.cells.resize(columns, Cell::blank(self.attributes));
            line.cells.truncate(columns);
        }
        self.visible = lines.into_iter().collect();
        self.columns = columns;
        self.rows = rows;
        self.cursor.row = self.cursor.row.min(rows - 1);
        self.cursor.column = self.cursor.column.min(columns - 1);
        self.tab_stops = (0..columns).map(|column| column % 8 == 0).collect();
        self.wrap_pending = false;
        Ok(())
    }

    fn push_scrollback(&mut self, line: ScreenLine) {
        if self.scrollback_limit == 0 {
            return;
        }
        self.scrollback.push_back(line);
        while self.scrollback.len() > self.scrollback_limit {
            self.scrollback.pop_front();
        }
    }

    fn reset(&mut self) {
        self.attributes = CellAttributes::default();
        self.visible = vec![ScreenLine::blank(self.columns, self.attributes); self.rows];
        self.scrollback.clear();
        self.cursor = CursorPosition {
            row: 0,
            column: 0,
            visible: true,
        };
        self.saved_cursor = None;
        self.tab_stops = (0..self.columns).map(|column| column % 8 == 0).collect();
        self.wrap_pending = false;
    }

    fn put_char(&mut self, character: char) {
        if self.wrap_pending {
            self.line_feed();
        }
        let row = self.cursor.row;
        let column = self.cursor.column;
        self.visible[row].cells[column] = Cell {
            character,
            attributes: self.attributes,
        };
        if column + 1 >= self.columns {
            self.cursor.column = self.columns - 1;
            self.wrap_pending = true;
        } else {
            self.cursor.column += 1;
        }
    }

    fn line_feed(&mut self) {
        self.wrap_pending = false;
        if self.cursor.row + 1 >= self.rows {
            let line = self.visible.remove(0);
            self.push_scrollback(line);
            self.visible
                .push(ScreenLine::blank(self.columns, self.attributes));
        } else {
            self.cursor.row += 1;
        }
    }

    fn carriage_return(&mut self) {
        self.cursor.column = 0;
        self.wrap_pending = false;
    }

    fn backspace(&mut self) {
        if self.wrap_pending {
            self.wrap_pending = false;
        } else {
            self.cursor.column = self.cursor.column.saturating_sub(1);
        }
    }

    fn tab(&mut self) {
        let next = self
            .tab_stops
            .iter()
            .enumerate()
            .skip(self.cursor.column.saturating_add(1))
            .find_map(|(column, is_stop)| (*is_stop).then_some(column))
            .unwrap_or(self.columns - 1);
        self.cursor.column = next;
        self.wrap_pending = false;
    }

    fn set_tab_stop(&mut self) {
        self.tab_stops[self.cursor.column] = true;
    }

    fn clear_tab_stop(&mut self, mode: usize) {
        match mode {
            0 => self.tab_stops[self.cursor.column] = false,
            3 => self.tab_stops.fill(false),
            _ => {}
        }
    }

    fn move_cursor(&mut self, row: usize, column: usize) {
        self.cursor.row = row.min(self.rows - 1);
        self.cursor.column = column.min(self.columns - 1);
        self.wrap_pending = false;
    }

    fn move_rows(&mut self, amount: isize) {
        let row = (self.cursor.row as isize + amount).clamp(0, self.rows as isize - 1) as usize;
        self.move_cursor(row, self.cursor.column);
    }

    fn move_columns(&mut self, amount: isize) {
        let column = (self.cursor.column as isize + amount).clamp(0, self.columns as isize - 1) as usize;
        self.move_cursor(self.cursor.row, column);
    }

    fn erase_display(&mut self, mode: usize) {
        match mode {
            0 => {
                self.erase_line_from(self.cursor.column, self.columns.saturating_sub(1));
                for row in self.cursor.row + 1..self.rows {
                    self.visible[row] = ScreenLine::blank(self.columns, self.attributes);
                }
            }
            1 => {
                for row in 0..self.cursor.row {
                    self.visible[row] = ScreenLine::blank(self.columns, self.attributes);
                }
                self.erase_line_from(0, self.cursor.column);
            }
            2 | 3 => {
                self.visible = vec![ScreenLine::blank(self.columns, self.attributes); self.rows];
                if mode == 3 {
                    self.scrollback.clear();
                }
            }
            _ => {}
        }
    }

    fn erase_line(&mut self, mode: usize) {
        match mode {
            0 => self.erase_line_from(self.cursor.column, self.columns.saturating_sub(1)),
            1 => self.erase_line_from(0, self.cursor.column),
            2 => self.visible[self.cursor.row] = ScreenLine::blank(self.columns, self.attributes),
            _ => {}
        }
    }

    fn erase_line_from(&mut self, start: usize, end: usize) {
        if start > end || start >= self.columns {
            return;
        }
        let end = end.min(self.columns - 1);
        for cell in &mut self.visible[self.cursor.row].cells[start..=end] {
            *cell = Cell::blank(self.attributes);
        }
    }

    fn insert_chars(&mut self, count: usize) {
        let count = count.min(self.columns - self.cursor.column);
        let line = &mut self.visible[self.cursor.row].cells;
        let end = self.columns;
        line.copy_within(self.cursor.column..end - count, self.cursor.column + count);
        for cell in &mut line[self.cursor.column..self.cursor.column + count] {
            *cell = Cell::blank(self.attributes);
        }
    }

    fn delete_chars(&mut self, count: usize) {
        let count = count.min(self.columns - self.cursor.column);
        let line = &mut self.visible[self.cursor.row].cells;
        let end = self.columns;
        line.copy_within(self.cursor.column + count..end, self.cursor.column);
        for cell in &mut line[end - count..] {
            *cell = Cell::blank(self.attributes);
        }
    }

    fn erase_chars(&mut self, count: usize) {
        let end = (self.cursor.column + count.max(1)).min(self.columns);
        for cell in &mut self.visible[self.cursor.row].cells[self.cursor.column..end] {
            *cell = Cell::blank(self.attributes);
        }
    }

    fn insert_lines(&mut self, count: usize) {
        let count = count.min(self.rows - self.cursor.row);
        for _ in 0..count {
            self.visible
                .insert(self.cursor.row, ScreenLine::blank(self.columns, self.attributes));
            self.visible.pop();
        }
    }

    fn delete_lines(&mut self, count: usize) {
        let count = count.min(self.rows - self.cursor.row);
        for _ in 0..count {
            let line = self.visible.remove(self.cursor.row);
            self.push_scrollback(line);
            self.visible
                .push(ScreenLine::blank(self.columns, self.attributes));
        }
    }

    fn set_attributes(&mut self, params: &[usize]) {
        if params.is_empty() {
            self.attributes = CellAttributes::default();
            return;
        }
        let mut index = 0;
        while index < params.len() {
            match params[index] {
                0 => self.attributes = CellAttributes::default(),
                1 => self.attributes.bold = true,
                2 => self.attributes.dim = true,
                3 => self.attributes.italic = true,
                4 => self.attributes.underline = true,
                7 => self.attributes.inverse = true,
                22 => {
                    self.attributes.bold = false;
                    self.attributes.dim = false;
                }
                23 => self.attributes.italic = false,
                24 => self.attributes.underline = false,
                27 => self.attributes.inverse = false,
                30..=37 => self.attributes.foreground = TerminalColor::Indexed((params[index] - 30) as u8),
                39 => self.attributes.foreground = TerminalColor::Default,
                40..=47 => self.attributes.background = TerminalColor::Indexed((params[index] - 40) as u8),
                49 => self.attributes.background = TerminalColor::Default,
                90..=97 => {
                    self.attributes.foreground = TerminalColor::Indexed((params[index] - 90 + 8) as u8)
                }
                100..=107 => {
                    self.attributes.background = TerminalColor::Indexed((params[index] - 100 + 8) as u8)
                }
                38 | 48 if params.get(index + 1) == Some(&5) => {
                    // Keep the parser synchronized for the common 256-color form.
                    if let Some(color) = params.get(index + 2) {
                        if params[index] == 38 {
                            self.attributes.foreground = TerminalColor::Indexed((*color).min(255) as u8);
                        } else {
                            self.attributes.background = TerminalColor::Indexed((*color).min(255) as u8);
                        }
                        index += 2;
                    }
                }
                _ => {}
            }
            index += 1;
        }
    }
}

#[derive(Debug, Default)]
enum ParserState {
    #[default]
    Ground,
    Escape,
    Csi {
        bytes: Vec<u8>,
        private: bool,
    },
    Osc {
        bytes: Vec<u8>,
    },
    OscEscape {
        bytes: Vec<u8>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminalEvent {
    Title(String),
    CurrentDirectory(PathBuf),
    Bell,
}

#[derive(Debug, Default)]
struct Utf8Decoder {
    codepoint: u32,
    expected: u8,
    seen: u8,
    minimum: u32,
}

impl Utf8Decoder {
    fn feed<F>(&mut self, byte: u8, mut emit: F)
    where
        F: FnMut(char),
    {
        let mut byte = Some(byte);
        while let Some(current) = byte.take() {
            if self.expected == 0 {
                match current {
                    0x00..=0x7f => emit(current as char),
                    0xc2..=0xdf => {
                        self.codepoint = (current & 0x1f) as u32;
                        self.expected = 1;
                        self.seen = 0;
                        self.minimum = 0x80;
                    }
                    0xe0..=0xef => {
                        self.codepoint = (current & 0x0f) as u32;
                        self.expected = 2;
                        self.seen = 0;
                        self.minimum = 0x800;
                    }
                    0xf0..=0xf4 => {
                        self.codepoint = (current & 0x07) as u32;
                        self.expected = 3;
                        self.seen = 0;
                        self.minimum = 0x10000;
                    }
                    _ => emit('\u{fffd}'),
                }
                continue;
            }

            if !(0x80..=0xbf).contains(&current) {
                emit('\u{fffd}');
                self.reset();
                byte = Some(current);
                continue;
            }

            self.codepoint = (self.codepoint << 6) | (current & 0x3f) as u32;
            self.seen += 1;
            if self.seen == self.expected {
                let codepoint = self.codepoint;
                if codepoint >= self.minimum
                    && codepoint <= 0x10ffff
                    && !(0xd800..=0xdfff).contains(&codepoint)
                {
                    emit(char::from_u32(codepoint).unwrap_or('\u{fffd}'));
                } else {
                    emit('\u{fffd}');
                }
                self.reset();
            }
        }
    }

    fn flush<F>(&mut self, mut emit: F)
    where
        F: FnMut(char),
    {
        if self.expected != 0 {
            emit('\u{fffd}');
            self.reset();
        }
    }

    fn reset(&mut self) {
        self.codepoint = 0;
        self.expected = 0;
        self.seen = 0;
        self.minimum = 0;
    }
}

#[derive(Debug, Default)]
pub struct VtParser {
    state: ParserState,
    utf8: Utf8Decoder,
}

impl VtParser {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn feed(&mut self, bytes: &[u8], screen: &mut TerminalScreen) -> Vec<TerminalEvent> {
        let mut events = Vec::new();
        self.feed_into(bytes, screen, &mut events);
        events
    }

    pub fn feed_into(&mut self, bytes: &[u8], screen: &mut TerminalScreen, events: &mut Vec<TerminalEvent>) {
        for &byte in bytes {
            self.advance(byte, screen, events);
        }
    }

    pub fn finish(&mut self, screen: &mut TerminalScreen) {
        self.utf8.flush(|character| screen.put_char(character));
        self.state = ParserState::Ground;
    }

    fn advance(&mut self, byte: u8, screen: &mut TerminalScreen, events: &mut Vec<TerminalEvent>) {
        match &mut self.state {
            ParserState::Ground => self.advance_ground(byte, screen),
            ParserState::Escape => self.advance_escape(byte, screen),
            ParserState::Csi { bytes, private } => {
                if byte == 0x1b {
                    self.utf8.flush(|character| screen.put_char(character));
                    self.state = ParserState::Escape;
                } else if (0x40..=0x7e).contains(&byte) {
                    let bytes = std::mem::take(bytes);
                    let private = *private;
                    self.state = ParserState::Ground;
                    self.apply_csi(&bytes, private, byte, screen);
                } else if bytes.len() < MAX_CONTROL_SEQUENCE_BYTES {
                    if bytes.is_empty() && byte == b'?' {
                        *private = true;
                    } else {
                        bytes.push(byte);
                    }
                } else {
                    self.state = ParserState::Ground;
                }
            }
            ParserState::Osc { bytes } => {
                if byte == 0x07 {
                    let bytes = std::mem::take(bytes);
                    self.state = ParserState::Ground;
                    Self::apply_osc(&bytes, events);
                } else if byte == 0x1b {
                    let bytes = std::mem::take(bytes);
                    self.state = ParserState::OscEscape { bytes };
                } else if bytes.len() < MAX_CONTROL_SEQUENCE_BYTES {
                    bytes.push(byte);
                } else {
                    self.state = ParserState::Ground;
                }
            }
            ParserState::OscEscape { bytes } => {
                if byte == b'\\' {
                    let bytes = std::mem::take(bytes);
                    self.state = ParserState::Ground;
                    Self::apply_osc(&bytes, events);
                } else if byte == 0x1b {
                    // Keep the most recent ESC as the possible string terminator.
                    bytes.clear();
                    self.state = ParserState::OscEscape { bytes: Vec::new() };
                } else if bytes.len() + 2 < MAX_CONTROL_SEQUENCE_BYTES {
                    bytes.push(0x1b);
                    bytes.push(byte);
                    let bytes = std::mem::take(bytes);
                    self.state = ParserState::Osc { bytes };
                } else {
                    self.state = ParserState::Ground;
                }
            }
        }
    }

    fn advance_ground(&mut self, byte: u8, screen: &mut TerminalScreen) {
        match byte {
            0x1b => {
                self.utf8.flush(|character| screen.put_char(character));
                self.state = ParserState::Escape;
            }
            0x00 | 0x0e | 0x0f => {}
            0x07 => {}
            0x08 => {
                self.utf8.flush(|character| screen.put_char(character));
                screen.backspace();
            }
            0x09 => {
                self.utf8.flush(|character| screen.put_char(character));
                screen.tab();
            }
            0x0a..=0x0c => {
                self.utf8.flush(|character| screen.put_char(character));
                screen.line_feed();
            }
            0x0d => {
                self.utf8.flush(|character| screen.put_char(character));
                screen.carriage_return();
            }
            0x80..=0x9f => {
                self.utf8.flush(|character| screen.put_char(character));
                match byte {
                    0x9b => {
                        self.state = ParserState::Csi {
                            bytes: Vec::new(),
                            private: false,
                        }
                    }
                    0x9d => self.state = ParserState::Osc { bytes: Vec::new() },
                    _ => {}
                }
            }
            _ => self.utf8.feed(byte, |character| screen.put_char(character)),
        }
    }

    fn advance_escape(&mut self, byte: u8, screen: &mut TerminalScreen) {
        match byte {
            b'[' => {
                self.state = ParserState::Csi {
                    bytes: Vec::new(),
                    private: false,
                };
            }
            b']' => self.state = ParserState::Osc { bytes: Vec::new() },
            b'7' => {
                screen.saved_cursor = Some(screen.cursor);
                self.state = ParserState::Ground;
            }
            b'8' => {
                if let Some(cursor) = screen.saved_cursor {
                    screen.cursor = cursor;
                }
                self.state = ParserState::Ground;
            }
            b'D' | b'E' => {
                screen.line_feed();
                if byte == b'E' {
                    screen.carriage_return();
                }
                self.state = ParserState::Ground;
            }
            b'M' => {
                if screen.cursor.row == 0 {
                    if let Some(last) = screen.visible.pop() {
                        screen
                            .visible
                            .insert(0, ScreenLine::blank(screen.columns, screen.attributes));
                        screen.push_scrollback(last);
                    }
                } else {
                    screen.cursor.row -= 1;
                }
                self.state = ParserState::Ground;
            }
            b'c' => {
                screen.reset();
                self.state = ParserState::Ground;
            }
            b'H' => {
                screen.set_tab_stop();
                self.state = ParserState::Ground;
            }
            0x1b => {}
            _ => self.state = ParserState::Ground,
        }
    }

    fn apply_csi(&self, raw: &[u8], private: bool, final_byte: u8, screen: &mut TerminalScreen) {
        let params = parse_parameters(raw);
        let first = |default| params.first().copied().unwrap_or(default).max(1);
        match final_byte {
            b'A' => screen.move_rows(-(first(1) as isize)),
            b'B' | b'e' => screen.move_rows(first(1) as isize),
            b'C' | b'a' => screen.move_columns(first(1) as isize),
            b'D' => screen.move_columns(-(first(1) as isize)),
            b'E' => {
                screen.move_rows(first(1) as isize);
                screen.carriage_return();
            }
            b'F' => {
                screen.move_rows(-(first(1) as isize));
                screen.carriage_return();
            }
            b'G' | b'`' => screen.move_cursor(screen.cursor.row, first(1) - 1),
            b'd' => screen.move_cursor(first(1) - 1, screen.cursor.column),
            b'H' | b'f' => {
                let row = params.first().copied().unwrap_or(1).max(1) - 1;
                let column = params.get(1).copied().unwrap_or(1).max(1) - 1;
                screen.move_cursor(row, column);
            }
            b'J' => screen.erase_display(params.first().copied().unwrap_or(0)),
            b'K' => screen.erase_line(params.first().copied().unwrap_or(0)),
            b'@' => screen.insert_chars(first(1)),
            b'P' => screen.delete_chars(first(1)),
            b'X' => screen.erase_chars(first(1)),
            b'L' => screen.insert_lines(first(1)),
            b'M' => screen.delete_lines(first(1)),
            b'm' => screen.set_attributes(&params),
            b'g' => screen.clear_tab_stop(params.first().copied().unwrap_or(0)),
            b's' if !private => screen.saved_cursor = Some(screen.cursor),
            b'u' if !private => {
                if let Some(cursor) = screen.saved_cursor {
                    screen.cursor = cursor;
                }
            }
            b'h' | b'l' if params.contains(&25) => {
                screen.cursor.visible = final_byte == b'h';
            }
            _ => {}
        }
    }

    fn apply_osc(bytes: &[u8], events: &mut Vec<TerminalEvent>) {
        let Ok(text) = std::str::from_utf8(bytes) else {
            let text = String::from_utf8_lossy(bytes).into_owned();
            Self::apply_osc_text(&text, events);
            return;
        };
        Self::apply_osc_text(text, events);
    }

    fn apply_osc_text(text: &str, events: &mut Vec<TerminalEvent>) {
        let Some((kind, value)) = text.split_once(';') else {
            return;
        };
        match kind {
            "0" | "1" | "2" => {
                if let Some(title) = sanitize_title(value) {
                    events.push(TerminalEvent::Title(title));
                }
            }
            "7" => {
                if let Some(path) = parse_file_uri(value) {
                    events.push(TerminalEvent::CurrentDirectory(path));
                }
            }
            _ => {}
        }
    }
}

fn parse_parameters(raw: &[u8]) -> Vec<usize> {
    if raw.is_empty() {
        return Vec::new();
    }
    let mut params = Vec::new();
    let mut value = 0usize;
    let mut has_digits = false;
    for byte in raw.iter().copied().chain(std::iter::once(b';')) {
        match byte {
            b'0'..=b'9' => {
                has_digits = true;
                value = value.saturating_mul(10).saturating_add((byte - b'0') as usize);
            }
            b';' | b':' => {
                params.push(if has_digits { value } else { 0 });
                value = 0;
                has_digits = false;
                if params.len() >= 32 {
                    break;
                }
            }
            _ => {}
        }
    }
    params
}

fn sanitize_title(title: &str) -> Option<String> {
    let title = title.trim();
    if title.is_empty() || title.chars().any(char::is_control) {
        return None;
    }
    let mut result = title.chars().take(256).collect::<String>();
    if result.is_empty() {
        return None;
    }
    if result.len() > 1024 {
        result.truncate(1024);
    }
    Some(std::mem::take(&mut result))
}

fn parse_file_uri(value: &str) -> Option<PathBuf> {
    let path = value.strip_prefix("file://")?;
    let path = path.strip_prefix("localhost").unwrap_or(path);
    let decoded = percent_decode(path)?;
    if decoded.as_bytes().contains(&0) {
        return None;
    }
    Some(PathBuf::from(decoded))
}

fn percent_decode(input: &str) -> Option<String> {
    let mut output = Vec::with_capacity(input.len());
    let bytes = input.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            if index + 2 >= bytes.len() {
                return None;
            }
            let high = hex_value(bytes[index + 1])?;
            let low = hex_value(bytes[index + 2])?;
            output.push(high << 4 | low);
            index += 3;
        } else {
            output.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(output).ok()
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionStatus {
    Running,
    Exited(Option<i32>),
    ShutDown,
}

#[allow(dead_code)]
enum BackendEvent {
    Bytes(Vec<u8>),
    #[cfg(target_os = "linux")]
    Exited(Option<i32>),
}

enum Backend {
    #[cfg(target_os = "linux")]
    Pty(LinuxPty),
    #[cfg(test)]
    Test(TestBackend),
    #[cfg(not(target_os = "linux"))]
    #[allow(dead_code)]
    Unsupported,
}

impl Backend {
    #[cfg(target_os = "linux")]
    fn spawn(config: &TerminalConfig, cwd: &Path) -> Result<Self, TerminalError> {
        Ok(Self::Pty(LinuxPty::spawn(config, cwd)?))
    }

    #[cfg(not(target_os = "linux"))]
    fn spawn(_config: &TerminalConfig, _cwd: &Path) -> Result<Self, TerminalError> {
        Err(TerminalError::UnsupportedPlatform)
    }

    fn send_bytes(&mut self, bytes: &[u8]) -> Result<usize, TerminalError> {
        if bytes.len() > MAX_INPUT_BYTES {
            return Err(TerminalError::InputTooLarge { max: MAX_INPUT_BYTES });
        }
        match self {
            #[cfg(target_os = "linux")]
            Self::Pty(pty) => pty.send_bytes(bytes),
            #[cfg(test)]
            Self::Test(test) => test.send_bytes(bytes),
            #[cfg(not(target_os = "linux"))]
            Self::Unsupported => Err(TerminalError::UnsupportedPlatform),
        }
    }

    fn poll(&mut self, events: &mut Vec<BackendEvent>) {
        let _ = events;
        match self {
            #[cfg(target_os = "linux")]
            Self::Pty(pty) => pty.poll(events),
            #[cfg(test)]
            Self::Test(test) => test.poll(events),
            #[cfg(not(target_os = "linux"))]
            Self::Unsupported => {}
        }
    }

    fn resize(&mut self, columns: u16, rows: u16) -> Result<(), TerminalError> {
        let _ = (columns, rows);
        match self {
            #[cfg(target_os = "linux")]
            Self::Pty(pty) => pty.resize(columns, rows),
            #[cfg(test)]
            Self::Test(test) => test.resize(columns, rows),
            #[cfg(not(target_os = "linux"))]
            Self::Unsupported => Err(TerminalError::UnsupportedPlatform),
        }
    }

    fn shutdown(&mut self) {
        match self {
            #[cfg(target_os = "linux")]
            Self::Pty(pty) => pty.shutdown(),
            #[cfg(test)]
            Self::Test(test) => test.shutdown(),
            #[cfg(not(target_os = "linux"))]
            Self::Unsupported => {}
        }
    }
}

pub struct TerminalSession {
    id: SessionId,
    title: String,
    custom_title: bool,
    cwd: PathBuf,
    screen: TerminalScreen,
    parser: VtParser,
    backend: Backend,
    status: SessionStatus,
}

impl TerminalSession {
    pub fn spawn(id: SessionId, config: TerminalConfig) -> Result<Self, TerminalError> {
        config.validate()?;
        let cwd = config
            .cwd
            .clone()
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/")));
        if !cwd.is_dir() {
            return Err(TerminalError::InvalidPath(cwd.display().to_string()));
        }
        let title = config.title.clone().unwrap_or_else(|| String::from("Terminal"));
        let screen = TerminalScreen::new(config.columns, config.rows, config.scrollback_lines)?;
        let backend = Backend::spawn(&config, &cwd)?;
        Ok(Self {
            id,
            title,
            custom_title: config.title.is_some(),
            cwd,
            screen,
            parser: VtParser::new(),
            backend,
            status: SessionStatus::Running,
        })
    }

    pub fn id(&self) -> SessionId {
        self.id
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    pub fn cwd(&self) -> &Path {
        &self.cwd
    }

    pub fn screen(&self) -> &TerminalScreen {
        &self.screen
    }

    pub fn status(&self) -> SessionStatus {
        self.status
    }

    pub fn is_running(&self) -> bool {
        self.status == SessionStatus::Running
    }

    pub fn set_title(&mut self, title: impl AsRef<str>) -> Result<(), TerminalError> {
        self.title = sanitize_title(title.as_ref()).ok_or(TerminalError::InvalidTitle)?;
        self.custom_title = true;
        Ok(())
    }

    pub fn use_shell_title(&mut self) {
        self.custom_title = false;
    }

    pub fn send_bytes(&mut self, bytes: &[u8]) -> Result<usize, TerminalError> {
        if !self.is_running() {
            return Err(TerminalError::Closed);
        }
        self.backend.send_bytes(bytes)
    }

    pub fn send_text(&mut self, text: &str) -> Result<usize, TerminalError> {
        self.send_bytes(text.as_bytes())
    }

    pub fn send_command(&mut self, command: &str) -> Result<usize, TerminalError> {
        if command.as_bytes().contains(&0) {
            return Err(TerminalError::InvalidPath("command contains NUL".into()));
        }
        let bytes = command.len().saturating_add(1);
        if bytes > MAX_INPUT_BYTES {
            return Err(TerminalError::InputTooLarge { max: MAX_INPUT_BYTES });
        }
        let mut sent = self.send_bytes(command.as_bytes())?;
        sent += self.send_bytes(b"\n")?;
        Ok(sent)
    }

    pub fn poll(&mut self) -> usize {
        let mut events = Vec::new();
        self.backend.poll(&mut events);
        let mut byte_count = 0;
        for event in events {
            match event {
                BackendEvent::Bytes(bytes) => {
                    byte_count += bytes.len();
                    let mut parser_events = Vec::new();
                    self.parser
                        .feed_into(&bytes, &mut self.screen, &mut parser_events);
                    self.apply_parser_events(parser_events);
                }
                #[cfg(target_os = "linux")]
                BackendEvent::Exited(code) => {
                    self.status = SessionStatus::Exited(code);
                }
            }
        }
        byte_count
    }

    pub fn resize(&mut self, columns: u16, rows: u16) -> Result<(), TerminalError> {
        self.screen.resize(columns, rows)?;
        self.backend.resize(columns, rows)
    }

    pub fn shutdown(&mut self) {
        if self.status != SessionStatus::ShutDown {
            self.backend.shutdown();
            self.status = SessionStatus::ShutDown;
        }
    }

    fn apply_parser_events(&mut self, events: Vec<TerminalEvent>) {
        for event in events {
            match event {
                TerminalEvent::Title(title) if !self.custom_title => self.title = title,
                TerminalEvent::CurrentDirectory(path) => self.cwd = path,
                TerminalEvent::Bell | TerminalEvent::Title(_) => {}
            }
        }
    }

    #[cfg(test)]
    fn for_test(id: SessionId, config: TerminalConfig, output: &[u8]) -> Self {
        let cwd = config.cwd.clone().unwrap_or_else(|| PathBuf::from("/"));
        Self {
            id,
            title: config.title.clone().unwrap_or_else(|| String::from("Terminal")),
            custom_title: config.title.is_some(),
            cwd,
            screen: TerminalScreen::new(config.columns, config.rows, config.scrollback_lines)
                .expect("test config is valid"),
            parser: VtParser::new(),
            backend: Backend::Test(TestBackend::new(output)),
            status: SessionStatus::Running,
        }
    }

    #[cfg(test)]
    fn queue_test_output(&mut self, bytes: &[u8]) {
        #[cfg(target_os = "linux")]
        if let Backend::Test(test) = &mut self.backend {
            test.queue_output(bytes);
        }
        #[cfg(not(target_os = "linux"))]
        let Backend::Test(test) = &mut self.backend else {
            return;
        };
        #[cfg(not(target_os = "linux"))]
        test.queue_output(bytes);
    }

    #[cfg(test)]
    fn take_test_input(&mut self) -> Vec<u8> {
        match &mut self.backend {
            Backend::Test(test) => test.take_input(),
            #[allow(unreachable_patterns)]
            _ => Vec::new(),
        }
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        self.shutdown();
    }
}

pub struct TerminalTabs {
    next_id: SessionId,
    active: Option<SessionId>,
    max_tabs: usize,
    default_config: TerminalConfig,
    sessions: Vec<TerminalSession>,
}

impl Default for TerminalTabs {
    fn default() -> Self {
        Self::new(TerminalConfig::default())
    }
}

impl TerminalTabs {
    pub fn new(default_config: TerminalConfig) -> Self {
        Self {
            next_id: 1,
            active: None,
            max_tabs: MAX_TABS,
            default_config,
            sessions: Vec::new(),
        }
    }

    pub fn with_max_tabs(mut self, max_tabs: usize) -> Self {
        self.max_tabs = max_tabs.clamp(1, MAX_TABS);
        self
    }

    pub fn len(&self) -> usize {
        self.sessions.len()
    }

    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }

    pub fn active_id(&self) -> Option<SessionId> {
        self.active
    }

    pub fn ids(&self) -> impl Iterator<Item = SessionId> + '_ {
        self.sessions.iter().map(TerminalSession::id)
    }

    pub fn active(&self) -> Option<&TerminalSession> {
        self.active.and_then(|id| self.session(id))
    }

    pub fn active_mut(&mut self) -> Option<&mut TerminalSession> {
        let id = self.active?;
        self.session_mut(id)
    }

    pub fn session(&self, id: SessionId) -> Option<&TerminalSession> {
        self.sessions.iter().find(|session| session.id == id)
    }

    pub fn session_mut(&mut self, id: SessionId) -> Option<&mut TerminalSession> {
        self.sessions.iter_mut().find(|session| session.id == id)
    }

    pub fn open(&mut self) -> Result<SessionId, TerminalError> {
        self.open_with(self.default_config.clone())
    }

    pub fn open_with(&mut self, config: TerminalConfig) -> Result<SessionId, TerminalError> {
        if self.sessions.len() >= self.max_tabs {
            return Err(TerminalError::TooManyTabs { max: self.max_tabs });
        }
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1).max(1);
        let session = TerminalSession::spawn(id, config)?;
        self.sessions.push(session);
        self.active = Some(id);
        Ok(id)
    }

    pub fn select(&mut self, id: SessionId) -> bool {
        if self.session(id).is_some() {
            self.active = Some(id);
            true
        } else {
            false
        }
    }

    pub fn close(&mut self, id: SessionId) -> bool {
        let Some(index) = self.sessions.iter().position(|session| session.id == id) else {
            return false;
        };
        self.sessions.remove(index);
        if self.active == Some(id) {
            self.active = self
                .sessions
                .get(index.min(self.sessions.len().saturating_sub(1)))
                .map(TerminalSession::id)
                .or_else(|| self.sessions.last().map(TerminalSession::id));
        }
        if self.sessions.is_empty() {
            self.active = None;
        }
        true
    }

    pub fn poll(&mut self) -> usize {
        self.sessions.iter_mut().map(TerminalSession::poll).sum()
    }

    pub fn shutdown(&mut self) {
        for session in &mut self.sessions {
            session.shutdown();
        }
        self.active = None;
    }

    #[cfg(test)]
    fn open_for_test(&mut self, output: &[u8]) -> SessionId {
        let id = self.next_id;
        self.next_id += 1;
        let session = TerminalSession::for_test(id, self.default_config.clone(), output);
        self.sessions.push(session);
        self.active = Some(id);
        id
    }
}

#[cfg(target_os = "linux")]
mod linux_pty {
    use super::*;
    use std::ffi::{CStr, CString, OsStr};
    use std::fs::File;
    use std::io::{ErrorKind, Read, Write};
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::ffi::OsStrExt;
    use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError};
    use std::sync::{Arc, Mutex};
    use std::thread;

    pub(super) struct LinuxPty {
        writer: Arc<Mutex<File>>,
        events: Receiver<BackendEvent>,
        pid: libc::pid_t,
        shutdown: bool,
    }

    impl LinuxPty {
        pub(super) fn spawn(config: &TerminalConfig, cwd: &Path) -> Result<Self, TerminalError> {
            let shell = resolve_shell(config.shell.as_deref())?;
            let shell_c = CString::new(shell.as_os_str().as_bytes())
                .map_err(|_| TerminalError::InvalidPath(shell.display().to_string()))?;
            let cwd_c = CString::new(cwd.as_os_str().as_bytes())
                .map_err(|_| TerminalError::InvalidPath(cwd.display().to_string()))?;
            let interactive_c = CString::new("-i").expect("literal contains no NUL");
            let term_key = CString::new("TERM").expect("literal contains no NUL");
            let term_value = CString::new("xterm-256color").expect("literal contains no NUL");
            let lang_key = CString::new("LC_CTYPE").expect("literal contains no NUL");
            let lang_value = CString::new("C.UTF-8").expect("literal contains no NUL");
            let color_key = CString::new("COLORTERM").expect("literal contains no NUL");
            let color_value = CString::new("truecolor").expect("literal contains no NUL");
            let mut master_fd = -1;
            let window = libc::winsize {
                ws_row: config.rows,
                ws_col: config.columns,
                ws_xpixel: 0,
                ws_ypixel: 0,
            };

            // SAFETY: all pointers refer to stack-owned, NUL-terminated values
            // that remain alive through the fork call. `forkpty` initializes the
            // master descriptor and returns once in each process.
            let pid =
                unsafe { libc::forkpty(&mut master_fd, std::ptr::null_mut(), std::ptr::null(), &window) };
            if pid < 0 {
                return Err(io::Error::last_os_error().into());
            }
            if pid == 0 {
                // SAFETY: this is the child side of fork. The calls either
                // configure the child and exec, or terminate it without
                // running Rust destructors across fork.
                unsafe {
                    let _ = libc::chdir(cwd_c.as_ptr());
                    let _ = libc::setenv(term_key.as_ptr(), term_value.as_ptr(), 1);
                    let _ = libc::setenv(lang_key.as_ptr(), lang_value.as_ptr(), 1);
                    let _ = libc::setenv(color_key.as_ptr(), color_value.as_ptr(), 1);
                    let mut argv = [shell_c.as_ptr(), interactive_c.as_ptr(), std::ptr::null()];
                    libc::execv(shell_c.as_ptr(), argv.as_mut_ptr());
                    libc::_exit(127);
                }
            }

            if master_fd < 0 {
                // The parent should not get here, but avoid leaking a child if
                // a non-standard libc implementation returned no descriptor.
                unsafe {
                    libc::kill(pid, libc::SIGKILL);
                    libc::waitpid(pid, std::ptr::null_mut(), 0);
                }
                return Err(io::Error::other("forkpty did not return a master descriptor").into());
            }

            // SAFETY: the parent exclusively owns the descriptor returned by
            // forkpty and transfers it to this File.
            let master = unsafe { File::from_raw_fd(master_fd) };
            let reader = master.try_clone()?;
            let writer = Arc::new(Mutex::new(master));
            let (sender, events) = mpsc::sync_channel(OUTPUT_QUEUE_CAPACITY);
            spawn_reader(reader, sender);
            Ok(Self {
                writer,
                events,
                pid,
                shutdown: false,
            })
        }

        pub(super) fn send_bytes(&mut self, bytes: &[u8]) -> Result<usize, TerminalError> {
            if self.shutdown {
                return Err(TerminalError::Closed);
            }
            let mut writer = self.writer.lock().map_err(|_| TerminalError::Closed)?;
            writer.write_all(bytes)?;
            writer.flush()?;
            Ok(bytes.len())
        }

        pub(super) fn poll(&mut self, events: &mut Vec<BackendEvent>) {
            loop {
                match self.events.try_recv() {
                    Ok(event) => events.push(event),
                    Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
                }
            }
            if self.shutdown {
                return;
            }
            let mut status = 0;
            // SAFETY: `pid` is the child returned by forkpty and `status` is a
            // valid writable status slot. WNOHANG makes polling non-blocking.
            let result = unsafe { libc::waitpid(self.pid, &mut status, libc::WNOHANG) };
            if result == self.pid {
                self.shutdown = true;
                events.push(BackendEvent::Exited(exit_code(status)));
            }
        }

        pub(super) fn resize(&mut self, columns: u16, rows: u16) -> Result<(), TerminalError> {
            if self.shutdown {
                return Err(TerminalError::Closed);
            }
            let window = libc::winsize {
                ws_row: rows,
                ws_col: columns,
                ws_xpixel: 0,
                ws_ypixel: 0,
            };
            let fd = self.writer.lock().map_err(|_| TerminalError::Closed)?.as_raw_fd();
            // SAFETY: fd is the live PTY master and `window` is a valid winsize.
            let result = unsafe { libc::ioctl(fd, libc::TIOCSWINSZ, &window) };
            if result == -1 {
                Err(io::Error::last_os_error().into())
            } else {
                Ok(())
            }
        }

        fn signal_process_group(&self, signal: libc::c_int) {
            if self.pid <= 0 {
                return;
            }
            // forkpty makes the child a session/process-group leader. Signal
            // the group first so a foreground editor or compiler can clean up
            // too; fall back to the shell pid for unusual libc setups.
            let group_result = unsafe { libc::kill(-self.pid, signal) };
            if group_result == -1 {
                unsafe {
                    let _ = libc::kill(self.pid, signal);
                }
            }
        }

        fn exited_within(&self, timeout: Duration) -> bool {
            let deadline = Instant::now() + timeout;
            loop {
                let mut status = 0;
                // SAFETY: pid belongs to this PTY and status is writable;
                // WNOHANG keeps the graceful close bounded.
                let result = unsafe { libc::waitpid(self.pid, &mut status, libc::WNOHANG) };
                if result == self.pid {
                    return true;
                }
                if result == -1 {
                    // Another non-blocking poll may already have reaped it.
                    return io::Error::last_os_error().raw_os_error() == Some(libc::ECHILD);
                }
                if Instant::now() >= deadline {
                    return false;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }

        pub(super) fn shutdown(&mut self) {
            if self.shutdown {
                return;
            }
            self.shutdown = true;
            // Give interactive programs a chance to save/close, then use
            // stronger signals only when the process group refuses to exit.
            self.signal_process_group(libc::SIGHUP);
            if self.exited_within(Duration::from_millis(60)) {
                return;
            }
            self.signal_process_group(libc::SIGTERM);
            if self.exited_within(Duration::from_millis(120)) {
                return;
            }
            self.signal_process_group(libc::SIGKILL);
            // SAFETY: the final wait is bounded by the kernel after SIGKILL;
            // it also prevents leaving a zombie child behind.
            unsafe {
                let _ = libc::waitpid(self.pid, std::ptr::null_mut(), 0);
            }
        }
    }

    impl Drop for LinuxPty {
        fn drop(&mut self) {
            self.shutdown();
        }
    }

    fn spawn_reader(mut reader: File, sender: SyncSender<BackendEvent>) {
        let _ = thread::Builder::new()
            .name(String::from("rouch-terminal-pty-reader"))
            .spawn(move || {
                let mut buffer = vec![0u8; OUTPUT_CHUNK_SIZE];
                loop {
                    match reader.read(&mut buffer) {
                        Ok(0) => break,
                        Ok(size) => {
                            if sender.send(BackendEvent::Bytes(buffer[..size].to_vec())).is_err() {
                                break;
                            }
                        }
                        Err(error) if error.kind() == ErrorKind::Interrupted => continue,
                        Err(error) if error.raw_os_error() == Some(libc::EIO) => break,
                        Err(_) => break,
                    }
                }
                let _ = sender.send(BackendEvent::Exited(None));
            });
    }

    fn resolve_shell(explicit: Option<&Path>) -> Result<PathBuf, TerminalError> {
        let candidates = explicit
            .map(Path::to_path_buf)
            .into_iter()
            .chain(std::env::var_os("SHELL").map(PathBuf::from))
            .chain(passwd_shell())
            .chain([PathBuf::from("/bin/sh"), PathBuf::from("/usr/bin/sh")]);
        for candidate in candidates {
            if is_executable_absolute(&candidate) {
                return Ok(candidate);
            }
        }
        Err(TerminalError::ShellNotFound)
    }

    fn is_executable_absolute(path: &Path) -> bool {
        if !path.is_absolute() || path.as_os_str().as_bytes().contains(&0) {
            return false;
        }
        let Ok(metadata) = std::fs::metadata(path) else {
            return false;
        };
        use std::os::unix::fs::PermissionsExt;
        metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
    }

    fn passwd_shell() -> Option<PathBuf> {
        // SAFETY: libc returns a pointer owned by libc for the current user;
        // it is read immediately and copied into an owned PathBuf.
        let entry = unsafe { libc::getpwuid(libc::getuid()) };
        if entry.is_null() {
            return None;
        }
        // SAFETY: a non-null passwd entry has a valid pw_shell field on Linux.
        let shell = unsafe { (*entry).pw_shell };
        if shell.is_null() {
            return None;
        }
        // SAFETY: pw_shell is NUL-terminated according to the passwd ABI.
        let shell = unsafe { CStr::from_ptr(shell) };
        Some(PathBuf::from(OsStr::from_bytes(shell.to_bytes())))
    }

    fn exit_code(status: libc::c_int) -> Option<i32> {
        if libc::WIFEXITED(status) {
            Some(libc::WEXITSTATUS(status))
        } else if libc::WIFSIGNALED(status) {
            Some(128 + libc::WTERMSIG(status))
        } else {
            None
        }
    }
}

#[cfg(target_os = "linux")]
use linux_pty::LinuxPty;

#[cfg(test)]
struct TestBackend {
    output: VecDeque<BackendEvent>,
    input: Vec<u8>,
    columns: u16,
    rows: u16,
    alive: bool,
}

#[cfg(test)]
impl TestBackend {
    fn new(output: &[u8]) -> Self {
        let mut backend = Self {
            output: VecDeque::new(),
            input: Vec::new(),
            columns: DEFAULT_COLUMNS,
            rows: DEFAULT_ROWS,
            alive: true,
        };
        backend.queue_output(output);
        backend
    }

    fn queue_output(&mut self, bytes: &[u8]) {
        self.output.push_back(BackendEvent::Bytes(bytes.to_vec()));
    }

    fn send_bytes(&mut self, bytes: &[u8]) -> Result<usize, TerminalError> {
        if !self.alive {
            return Err(TerminalError::Closed);
        }
        self.input.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn take_input(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.input)
    }

    fn poll(&mut self, events: &mut Vec<BackendEvent>) {
        events.extend(self.output.drain(..));
    }

    fn resize(&mut self, columns: u16, rows: u16) -> Result<(), TerminalError> {
        self.columns = columns;
        self.rows = rows;
        Ok(())
    }

    fn shutdown(&mut self) {
        self.alive = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> TerminalConfig {
        TerminalConfig::new(20, 4)
            .expect("valid dimensions")
            .with_scrollback_lines(2)
            .expect("valid scrollback")
    }

    fn first_line(session: &TerminalSession) -> String {
        session.screen().visible_lines()[0].text()
    }

    #[test]
    fn invalid_utf8_is_replaced_without_stalling() {
        let mut session = TerminalSession::for_test(1, test_config(), &[0xff, b'A', 0xc3]);
        session.poll();
        session.queue_test_output(&[0x28, b'B']);
        session.poll();
        assert_eq!(&first_line(&session)[..], "�A�(B               ");
    }

    #[test]
    fn partial_escape_is_kept_between_chunks() {
        let mut session = TerminalSession::for_test(1, test_config(), b"\x1b[");
        session.poll();
        assert_eq!(first_line(&session), "                    ");
        session.queue_test_output(b"31mR");
        session.poll();
        let cell = session.screen().visible_lines()[0].cells()[0];
        assert_eq!(cell.character, 'R');
        assert_eq!(cell.attributes.foreground, TerminalColor::Indexed(1));
    }

    #[test]
    fn bytes_and_commands_are_written_verbatim_to_backend() {
        let mut session = TerminalSession::for_test(1, test_config(), &[]);
        assert_eq!(session.send_bytes(&[0x1b, b'[']).unwrap(), 2);
        assert_eq!(
            session.send_command("printf 'oi'").unwrap(),
            "printf 'oi'\n".len()
        );
        assert_eq!(session.take_test_input(), b"\x1b[printf 'oi'\n");
    }

    #[test]
    fn screen_scrollback_is_bounded() {
        let mut session = TerminalSession::for_test(1, test_config(), b"1\r\n2\r\n3\r\n4\r\n5\r\n6\r\n7\r\n");
        session.poll();
        assert_eq!(session.screen().scrollback_len(), 2);
        let lines = session
            .screen()
            .scrollback_lines()
            .map(ScreenLine::text)
            .collect::<Vec<_>>();
        assert_eq!(lines[0].trim_end(), "3");
        assert_eq!(lines[1].trim_end(), "4");
    }

    #[test]
    fn resize_does_not_pollute_scrollback_with_layout_rows() {
        let mut session = TerminalSession::for_test(1, test_config(), b"1\r\n2\r\n3\r\n4\r\n5\r\n6\r\n7\r\n");
        session.poll();
        let before = session.screen().scrollback_len();
        session.resize(20, 2).unwrap();
        session.resize(20, 4).unwrap();
        session.resize(20, 2).unwrap();
        assert_eq!(session.screen().scrollback_len(), before);
    }

    #[test]
    fn tabs_open_select_close_and_shutdown_cleanly() {
        let mut tabs = TerminalTabs::new(test_config()).with_max_tabs(3);
        let first = tabs.open_for_test(b"one");
        let second = tabs.open_for_test(b"two");
        assert_eq!(tabs.active_id(), Some(second));
        assert!(tabs.select(first));
        assert_eq!(tabs.active().unwrap().title(), "Terminal");
        assert!(tabs.session_mut(first).unwrap().set_title("Build").is_ok());
        assert_eq!(tabs.session(first).unwrap().title(), "Build");
        assert!(tabs.close(first));
        assert_eq!(tabs.active_id(), Some(second));
        tabs.shutdown();
        assert_eq!(tabs.active_id(), None);
        assert_eq!(tabs.session(second).unwrap().status(), SessionStatus::ShutDown);
    }

    #[test]
    fn osc_title_and_working_directory_are_incremental() {
        let mut session = TerminalSession::for_test(
            1,
            test_config(),
            b"\x1b]2;Build\x07\x1b]7;file:///tmp/project\x1b\\",
        );
        session.poll();
        assert_eq!(session.title(), "Build");
        assert_eq!(session.cwd(), Path::new("/tmp/project"));
    }

    #[test]
    fn windows_does_not_claim_to_spawn_a_pty() {
        #[cfg(not(target_os = "linux"))]
        assert!(matches!(
            TerminalSession::spawn(1, test_config()),
            Err(TerminalError::UnsupportedPlatform)
        ));
    }
}
