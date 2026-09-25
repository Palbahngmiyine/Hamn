//! A small screen recorder for Ratatui's cursor-addressed PTY output. It
//! places characters where the cursor puts them rather than stripping ANSI,
//! so assertions see what a terminal would show.
use unicode_width::UnicodeWidthChar;

pub struct Screen {
    rows: usize,
    cols: usize,
    cells: Vec<Vec<char>>,
    row: usize,
    col: usize,
    /// An escape sequence split across reads.
    pending: String,
    /// A UTF-8 sequence split across reads.
    undecoded: Vec<u8>,
}

impl Screen {
    pub fn new(rows: usize, cols: usize) -> Self {
        Self { rows, cols, cells: vec![vec![' '; cols]; rows], row: 0, col: 0, pending: String::new(), undecoded: Vec::new() }
    }

    pub fn feed(&mut self, data: &[u8]) {
        let mut text = std::mem::take(&mut self.pending);
        text.push_str(&self.decode(data));
        let chars: Vec<char> = text.chars().collect();
        let mut i = 0;
        while i < chars.len() {
            if chars[i] == '\x1b' {
                if i + 1 == chars.len() {
                    self.pending = chars[i..].iter().collect();
                    break;
                }
                if chars[i + 1] == '[' {
                    let Some((parameters, last, length)) = control_sequence(&chars[i..]) else {
                        self.pending = chars[i..].iter().collect();
                        break;
                    };
                    self.apply(&parameters, last);
                    i += length;
                    continue;
                }
                i += 2;
                continue;
            }
            let c = chars[i];
            if c == '\r' {
                self.col = 0;
            } else if c == '\n' {
                self.row += 1;
            } else if c >= ' ' {
                if self.row < self.rows && self.col < self.cols {
                    self.cells[self.row][self.col] = c;
                }
                self.col += if c.width() == Some(2) { 2 } else { 1 };
            }
            i += 1;
        }
    }

    /// Decodes UTF-8 incrementally, replacing invalid bytes with U+FFFD and
    /// keeping an incomplete trailing sequence for the next read.
    fn decode(&mut self, data: &[u8]) -> String {
        self.undecoded.extend_from_slice(data);
        let mut text = String::new();
        let mut rest: &[u8] = &self.undecoded;
        loop {
            match std::str::from_utf8(rest) {
                Ok(valid) => {
                    text.push_str(valid);
                    rest = &[];
                    break;
                }
                Err(error) => {
                    let (valid, after) = rest.split_at(error.valid_up_to());
                    text.push_str(std::str::from_utf8(valid).unwrap());
                    match error.error_len() {
                        Some(length) => {
                            text.push('\u{fffd}');
                            rest = &after[length..];
                        }
                        None => {
                            rest = after;
                            break;
                        }
                    }
                }
            }
        }
        self.undecoded = rest.to_vec();
        text
    }

    fn apply(&mut self, parameters: &str, last: char) {
        let values: Vec<usize> = if parameters.starts_with('?') {
            Vec::new()
        } else {
            parameters.split(';').map(|value| value.parse().unwrap_or(0)).collect()
        };
        let a = values.first().copied().unwrap_or(0);
        match last {
            'H' | 'f' => {
                self.row = a.saturating_sub(1);
                self.col = values.get(1).copied().unwrap_or(1).saturating_sub(1);
            }
            'G' => self.col = a.saturating_sub(1),
            'A' => self.row = self.row.saturating_sub(if a == 0 { 1 } else { a }),
            'B' => self.row += if a == 0 { 1 } else { a },
            'C' => self.col += if a == 0 { 1 } else { a },
            'D' => self.col = self.col.saturating_sub(if a == 0 { 1 } else { a }),
            'J' if a == 2 || a == 3 => self.cells = vec![vec![' '; self.cols]; self.rows],
            'K' if self.row < self.rows => {
                let (start, end) = match a {
                    2 => (0, self.cols),
                    1 => (0, self.col + 1),
                    _ => (self.col, self.cols),
                };
                for col in start..end.min(self.cols) {
                    self.cells[self.row][col] = ' ';
                }
            }
            _ => {}
        }
    }

    /// Rows joined by newlines, each without trailing whitespace.
    pub fn text(&self) -> String {
        self.cells.iter().map(|row| row.iter().collect::<String>().trim_end().to_owned()).collect::<Vec<_>>().join("\n")
    }
}

/// Parses `ESC [ parameters intermediates final` at the start of `chars`:
/// parameters in 0x30-0x3f, intermediates in 0x20-0x2f, a final byte in
/// 0x40-0x7e. Returns the parameters, the final character and the length, or
/// `None` while the sequence is incomplete or malformed.
fn control_sequence(chars: &[char]) -> Option<(String, char, usize)> {
    let mut i = 2;
    let start = i;
    while i < chars.len() && ('0'..='?').contains(&chars[i]) {
        i += 1;
    }
    let parameters: String = chars[start..i].iter().collect();
    while i < chars.len() && (' '..='/').contains(&chars[i]) {
        i += 1;
    }
    let last = *chars.get(i)?;
    ('@'..='~').contains(&last).then(|| (parameters, last, i + 1))
}

/// Exposes completed draws, not transient mixtures of old and new rows:
/// Ratatui writes the cursor visibility sequence after a frame's cells.
pub struct RatatuiScreen {
    screen: Screen,
    frame_pending: Vec<u8>,
}

impl RatatuiScreen {
    pub fn new(rows: usize, cols: usize) -> Self {
        Self { screen: Screen::new(rows, cols), frame_pending: Vec::new() }
    }

    pub fn feed(&mut self, data: &[u8]) {
        self.frame_pending.extend_from_slice(data);
        while let Some(end) = cursor_visibility_end(&self.frame_pending) {
            let frame: Vec<u8> = self.frame_pending.drain(..end).collect();
            self.screen.feed(&frame);
        }
    }

    pub fn text(&self) -> String {
        self.screen.text()
    }
}

/// The end of the first `ESC [ ? 2 5 h` or `ESC [ ? 2 5 l`.
fn cursor_visibility_end(data: &[u8]) -> Option<usize> {
    data.windows(6)
        .position(|window| window[..5] == *b"\x1b[?25" && (window[5] == b'h' || window[5] == b'l'))
        .map(|start| start + 6)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_addressing_places_text_and_erases_lines() {
        let mut screen = Screen::new(3, 10);
        screen.feed(b"\x1b[2;3Hab\x1b[1;1Hxyz\x1b[1;2H\x1b[K");
        assert_eq!(screen.text(), "x\n  ab\n");
    }

    #[test]
    fn split_escape_and_utf8_sequences_complete_on_the_next_read() {
        let mut screen = Screen::new(1, 10);
        let bytes = "\x1b[1;2H가b".as_bytes();
        for byte in bytes {
            screen.feed(std::slice::from_ref(byte));
        }
        assert_eq!(screen.text(), " 가 b");
    }

    #[test]
    fn ratatui_screen_waits_for_the_cursor_visibility_marker() {
        let mut screen = RatatuiScreen::new(1, 10);
        screen.feed(b"\x1b[1;1Hnew");
        assert_eq!(screen.text(), "");
        screen.feed(b"\x1b[?25l");
        assert_eq!(screen.text(), "new");
    }
}
