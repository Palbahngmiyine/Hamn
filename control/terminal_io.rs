use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{layout::Rect, style::{Color, Modifier, Style}, widgets::Widget};

pub fn key_bytes(key: KeyEvent, application: bool) -> Vec<u8> {
    let modifier = 1 + u8::from(key.modifiers.contains(KeyModifiers::SHIFT)) +
        2 * u8::from(key.modifiers.contains(KeyModifiers::ALT)) + 4 * u8::from(key.modifiers.contains(KeyModifiers::CONTROL));
    let sequence = |last: char| if modifier > 1 { format!("\x1b[1;{modifier}{last}") }
        else { format!("\x1b{}{last}", if application { 'O' } else { '[' }) };
    let numbered = |number: u8| if modifier > 1 { format!("\x1b[{number};{modifier}~") } else { format!("\x1b[{number}~") };
    match key.code {
        KeyCode::Up => sequence('A').into_bytes(), KeyCode::Down => sequence('B').into_bytes(),
        KeyCode::Right => sequence('C').into_bytes(), KeyCode::Left => sequence('D').into_bytes(),
        KeyCode::Home => sequence('H').into_bytes(), KeyCode::End => sequence('F').into_bytes(),
        KeyCode::Insert => numbered(2).into_bytes(), KeyCode::Delete => numbered(3).into_bytes(),
        KeyCode::PageUp => numbered(5).into_bytes(), KeyCode::PageDown => numbered(6).into_bytes(),
        KeyCode::F(n @ 1..=4) => if modifier > 1 { format!("\x1b[1;{modifier}{}", char::from(b'P' + n - 1)).into_bytes() }
            else { vec![27, b'O', b'P' + n - 1] },
        KeyCode::F(n @ 5..=12) => numbered([15, 17, 18, 19, 20, 21, 23, 24][usize::from(n - 5)]).into_bytes(),
        KeyCode::BackTab => b"\x1b[Z".to_vec(),
        code => {
            let mut bytes = if key.modifiers.contains(KeyModifiers::ALT) { vec![27] } else { vec![] };
            match code {
                KeyCode::Char(c) if key.modifiers.contains(KeyModifiers::CONTROL) && c.is_ascii() => {
                    bytes.push(match c { ' ' | '@' | '2' => 0, '?' => 127, c => (c.to_ascii_uppercase() as u8) & 31 });
                },
                KeyCode::Char(c) => bytes.extend_from_slice(c.encode_utf8(&mut [0; 4]).as_bytes()),
                KeyCode::Enter => bytes.push(13), KeyCode::Backspace => bytes.push(127),
                KeyCode::Tab => bytes.push(9), KeyCode::Esc => bytes.push(27), KeyCode::Null => bytes.push(0),
                _ => {},
            }
            bytes
        }
    }
}
#[derive(Default)]
pub struct Replies(pub Vec<u8>);
impl vt100::Callbacks for Replies {
    fn unhandled_csi(&mut self, screen: &mut vt100::Screen, first: Option<u8>, _: Option<u8>, params: &[&[u16]], c: char) {
        if first.is_some() { return; }
        match (c, params.first().and_then(|v| v.first()).copied().unwrap_or(0)) {
            ('n', 5) => self.0.extend_from_slice(b"\x1b[0n"),
            ('n', 6) => { let (r, c) = screen.cursor_position(); self.0.extend_from_slice(format!("\x1b[{};{}R", r + 1, c + 1).as_bytes()); },
            ('c', 0) => self.0.extend_from_slice(b"\x1b[?1;2c"),
            _ => {},
        }
    }
}
fn color(color: vt100::Color) -> Color {
    match color { vt100::Color::Default => Color::Reset, vt100::Color::Idx(i) => Color::Indexed(i), vt100::Color::Rgb(r,g,b) => Color::Rgb(r,g,b) }
}
pub struct Screen<'a>(pub &'a vt100::Screen);
impl Widget for Screen<'_> {
    fn render(self, area: Rect, buffer: &mut ratatui::buffer::Buffer) {
        for row in 0..area.height {
            for col in 0..area.width {
                if let Some(cell) = self.0.cell(row, col) {
                    if cell.is_wide_continuation() { continue; }
                    let mut style = Style::default().fg(color(cell.fgcolor())).bg(color(cell.bgcolor()));
                    for (enabled, modifier) in [(cell.bold(), Modifier::BOLD), (cell.dim(), Modifier::DIM), (cell.italic(), Modifier::ITALIC),
                        (cell.underline(), Modifier::UNDERLINED), (cell.inverse(), Modifier::REVERSED)] {
                        if enabled { style = style.add_modifier(modifier); }
                    }
                    buffer[(area.x + col, area.y + row)].set_symbol(if cell.has_contents() { cell.contents() } else { " " }).set_style(style);
                }
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn forwards_control_detach_unicode_and_terminal_modes() {
        for (key, expected) in [('c', 3), ('p', 16), ('q', 17), ('d', 4), ('z', 26)] {
            assert_eq!(key_bytes(KeyEvent::new(KeyCode::Char(key), KeyModifiers::CONTROL), false), [expected]);
        }
        assert_eq!(key_bytes(KeyEvent::new(KeyCode::Char('한'), KeyModifiers::NONE), false), "한".as_bytes());
        assert_eq!(key_bytes(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE), true), b"\x1bOA");
        let mut parser = vt100::Parser::new_with_callbacks(10, 30, 100, Replies::default());
        parser.process(b"\x1b[3;5H\x1b[6n");
        assert_eq!(parser.callbacks().0, b"\x1b[3;5R");
        parser.process("한글".as_bytes());
        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(30, 10)).unwrap();
        terminal.draw(|f| f.render_widget(Screen(parser.screen()), f.area())).unwrap();
        assert_eq!(terminal.backend().buffer()[(4, 2)].symbol(), "한");
    }
}
