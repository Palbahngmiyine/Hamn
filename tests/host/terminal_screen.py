"""Small screen recorder for Ratatui's cursor-addressed PTY output, not ANSI stripping."""
import codecs
import re
import unicodedata

class Screen:
    def __init__(self, rows=40, cols=160):
        self.rows, self.cols = rows, cols
        self.cells = [[' '] * cols for _ in range(rows)]
        self.row = self.col = 0
        self.pending = ''
        self.decoder = codecs.getincrementaldecoder('utf-8')('replace')

    def feed(self, data):
        text = self.pending + self.decoder.decode(data)
        self.pending = ''
        i = 0
        while i < len(text):
            if text[i] == '\x1b':
                if i + 1 == len(text): self.pending = text[i:]; break
                if text[i + 1] == '[':
                    match = re.match(r'\x1b\[([0-?]*)([ -/]*)([@-~])', text[i:])
                    if not match: self.pending = text[i:]; break
                    values = [int(v or 0) for v in match[1].split(';')] if not match[1].startswith('?') else []
                    c = match[3]
                    a = values[0] if values else 0
                    if c in ('H', 'f'):
                        self.row, self.col = max(0, a - 1), max(0, (values[1] if len(values) > 1 else 1) - 1)
                    elif c == 'G': self.col = max(0, a - 1)
                    elif c == 'A': self.row = max(0, self.row - (a or 1))
                    elif c == 'B': self.row += a or 1
                    elif c == 'C': self.col += a or 1
                    elif c == 'D': self.col = max(0, self.col - (a or 1))
                    elif c == 'J' and a in (2, 3): self.cells = [[' '] * self.cols for _ in range(self.rows)]
                    elif c == 'K' and self.row < self.rows:
                        start, end = (0, self.cols) if a == 2 else ((0, self.col + 1) if a == 1 else (self.col, self.cols))
                        for col in range(start, min(end, self.cols)): self.cells[self.row][col] = ' '
                    i += match.end(); continue
                i += 2; continue
            c = text[i]
            if c == '\r': self.col = 0
            elif c == '\n': self.row += 1
            elif c >= ' ':
                if self.row < self.rows and self.col < self.cols: self.cells[self.row][self.col] = c
                self.col += 2 if unicodedata.east_asian_width(c) in ('W', 'F') else 1
            i += 1

    def text(self):
        return '\n'.join(''.join(row).rstrip() for row in self.cells)
