//! Human download progress for explicit install/upgrade transfers.
//!
//! Output goes only to the supplied writer (stderr in production); stdout keeps
//! machine results. A live display redraws one terminal line at most every
//! 200 ms. A non-live display prints one start line and one completion line,
//! so pipes and TUI logs stay line-oriented. Rendering never fails a transfer.
use std::{
    io::{IsTerminal, Write},
    time::{Duration, Instant},
};

const REDRAW_INTERVAL: Duration = Duration::from_millis(200);

/// Live redraw is chosen by the frontend when it relays stderr through a pipe
/// (`HAMN_UPDATE_PROGRESS=1|0`); otherwise it follows whether stderr is a TTY.
pub(super) fn live_terminal() -> bool {
    match std::env::var("HAMN_UPDATE_PROGRESS").as_deref() {
        Ok("1") => true,
        Ok("0") => false,
        _ => std::io::stderr().is_terminal(),
    }
}

/// Binary units with one decimal, e.g. `712.0 MiB`.
pub(super) fn bytes(value: u64) -> String {
    const UNITS: [&str; 4] = ["KiB", "MiB", "GiB", "TiB"];
    if value < 1024 {
        return format!("{value} B");
    }
    let mut scaled = value as f64 / 1024.0;
    let mut unit = 0;
    while scaled >= 1024.0 && unit + 1 < UNITS.len() {
        scaled /= 1024.0;
        unit += 1;
    }
    format!("{scaled:.1} {}", UNITS[unit])
}

pub(super) struct Progress<W: Write> {
    out: W,
    label: String,
    total: Option<u64>,
    live: bool,
    started: Option<Instant>,
    base: u64,
    drawn: Option<Instant>,
    open_line: bool,
}

impl<W: Write> Progress<W> {
    pub(super) fn new(out: W, label: impl Into<String>, total: Option<u64>, live: bool) -> Self {
        Self {
            out,
            label: label.into(),
            total,
            live,
            started: None,
            base: 0,
            drawn: None,
            open_line: false,
        }
    }

    /// Announce a network transfer that begins with `done` bytes already present.
    pub(super) fn start(&mut self, done: u64) {
        self.started = Some(Instant::now());
        self.base = done;
        self.drawn = None;
        if self.live {
            self.draw(done, Instant::now());
            return;
        }
        let size = self.total.map(bytes);
        let line = match (done, size) {
            (0, Some(size)) => format!("{} ({size})...", self.label),
            (0, None) => format!("{}...", self.label),
            (done, Some(size)) => {
                format!("{} (resuming at {} of {size})...", self.label, bytes(done))
            }
            (done, None) => format!("{} (resuming at {})...", self.label, bytes(done)),
        };
        let _ = writeln!(self.out, "{line}");
        let _ = self.out.flush();
    }

    /// Continue the same announced transfer from `done` bytes after the server
    /// rejected a Range request; prints nothing in line-oriented mode.
    pub(super) fn restart(&mut self, done: u64) {
        self.started = Some(Instant::now());
        self.base = done;
        self.drawn = None;
        if self.live {
            self.draw(done, Instant::now());
        }
    }

    pub(super) fn update(&mut self, done: u64) {
        if !self.live {
            return;
        }
        let now = Instant::now();
        if self
            .drawn
            .is_some_and(|last| now.duration_since(last) < REDRAW_INTERVAL)
        {
            return;
        }
        self.draw(done, now);
    }

    /// End the current line. Only a successful live transfer redraws 100%.
    pub(super) fn finish(&mut self, done: u64, succeeded: bool) {
        if self.live {
            if succeeded {
                self.draw(done, Instant::now());
            }
            if self.open_line {
                let _ = writeln!(self.out);
            }
        }
        self.open_line = false;
        let _ = self.out.flush();
    }

    /// Print a complete line, first ending any live progress line.
    pub(super) fn note(&mut self, message: &str) {
        if self.open_line {
            let _ = writeln!(self.out);
            self.open_line = false;
        }
        let _ = writeln!(self.out, "{message}");
        let _ = self.out.flush();
    }

    #[cfg(test)]
    pub(super) fn into_writer(self) -> W {
        self.out
    }

    fn draw(&mut self, done: u64, now: Instant) {
        let mut line = self.label.clone();
        match self.total {
            Some(total) if total > 0 => {
                let percent = (done.min(total) as u128 * 100 / total as u128) as u64;
                line += &format!("  {percent:>3}%  {} / {}", bytes(done), bytes(total));
            }
            _ => line += &format!("  {}", bytes(done)),
        }
        if let Some(started) = self.started {
            let elapsed = now.duration_since(started).as_secs_f64();
            if elapsed >= 1.0 {
                let rate = (done.saturating_sub(self.base) as f64 / elapsed) as u64;
                line += &format!("  {}/s", bytes(rate));
            }
        }
        // Carriage return + erase-to-end keeps one line in a terminal.
        let _ = write!(self.out, "\r\x1b[K{line}");
        let _ = self.out.flush();
        self.drawn = Some(now);
        self.open_line = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(progress: Progress<Vec<u8>>) -> String {
        String::from_utf8(progress.out).unwrap()
    }

    #[test]
    fn binary_units_round_to_one_decimal_and_keep_small_values_exact() {
        assert_eq!(bytes(0), "0 B");
        assert_eq!(bytes(1023), "1023 B");
        assert_eq!(bytes(1024), "1.0 KiB");
        assert_eq!(bytes(1536), "1.5 KiB");
        assert_eq!(bytes(746_586_112), "712.0 MiB");
        assert_eq!(bytes(3 * 1024 * 1024 * 1024), "3.0 GiB");
        assert_eq!(bytes(u64::MAX), "16777216.0 TiB");
    }

    #[test]
    fn line_oriented_output_has_one_start_line_and_no_control_bytes() {
        let mut progress = Progress::new(Vec::new(), "Downloading guest image", Some(2048), false);
        progress.start(0);
        progress.update(1024);
        progress.finish(2048, true);
        assert_eq!(text(progress), "Downloading guest image (2.0 KiB)...\n");

        let mut resumed = Progress::new(Vec::new(), "Downloading Hamn 1.2.3", Some(4096), false);
        resumed.start(1024);
        resumed.finish(1024, false);
        assert_eq!(
            text(resumed),
            "Downloading Hamn 1.2.3 (resuming at 1.0 KiB of 4.0 KiB)...\n"
        );

        let mut unknown = Progress::new(Vec::new(), "Downloading guest image", None, false);
        unknown.start(0);
        assert_eq!(text(unknown), "Downloading guest image...\n");
    }

    #[test]
    fn live_output_redraws_one_line_throttles_and_ends_with_a_newline() {
        let mut progress = Progress::new(Vec::new(), "Downloading guest image", Some(1000), true);
        progress.start(0);
        progress.update(10); // within the redraw interval: suppressed
        progress.finish(1000, true);
        let output = text(progress);
        assert_eq!(
            output,
            "\r\x1b[KDownloading guest image    0%  0 B / 1000 B\r\x1b[KDownloading guest image  100%  1000 B / 1000 B\n"
        );
    }

    #[test]
    fn failed_live_transfer_keeps_last_state_and_notes_start_on_a_new_line() {
        let mut progress = Progress::new(Vec::new(), "Downloading guest image", Some(1000), true);
        progress.start(250);
        progress.note("Connection interrupted; resuming (1 of 3)...");
        progress.finish(250, false);
        assert_eq!(
            text(progress),
            "\r\x1b[KDownloading guest image   25%  250 B / 1000 B\nConnection interrupted; resuming (1 of 3)...\n"
        );
    }

    #[test]
    fn percentage_never_exceeds_one_hundred_for_inconsistent_counts() {
        let mut progress = Progress::new(Vec::new(), "x", Some(10), true);
        progress.start(20);
        assert!(text(progress).contains("100%"));
    }
}
