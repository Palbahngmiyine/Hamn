use super::*;

#[tokio::test]
async fn input_queue_rejects_whole_overflow_and_preserves_accepted_order() {
    let invocation = crate::native::parse("version", &crate::tui_state::State::new(Default::default())).unwrap();
    let mut command = tokio::process::Command::new("/bin/cat");
    command.arg("-");
    let mut session = Session::spawn(command, invocation, 80, 24).unwrap();
    session.input(b"first").unwrap();
    let rest = vec![b'x'; INPUT_LIMIT - 5];
    session.input(&rest).unwrap();
    assert!(session.input(b"rejected").is_err());
    assert_eq!(session.input.len(), INPUT_LIMIT);
    assert_eq!(session.input.iter().take(5).copied().collect::<Vec<_>>(), b"first");
    assert!(session.input_error.as_ref().unwrap().contains("8 new bytes were not sent"));
    session.close_input("test closure");
    assert!(session.input(b"after close").is_err());
    assert!(session.input_error.as_ref().unwrap().contains("PTY input is closed"));
    session.signal(libc::SIGTERM).unwrap();
    session.child.wait().await.unwrap();
}

#[tokio::test]
async fn failed_write_reports_all_pending_bytes_without_replaying_input() {
    use std::os::unix::net::UnixStream;
    let (stream, _peer) = UnixStream::pair().unwrap();
    stream.set_nonblocking(true).unwrap();
    let fd: OwnedFd = stream.into();
    let fd = AsyncFd::new(fd).unwrap();
    // Cache writability before closing the endpoint; kqueue need not emit a new
    // write event for a shut-down socket. Exercise the write error itself.
    drop(tokio::time::timeout(std::time::Duration::from_secs(5), fd.writable()).await.unwrap().unwrap());
    // Closing the local write side is independent of reader descriptors briefly
    // inherited by concurrent forks, unlike relying on the last peer close.
    assert_eq!(unsafe { libc::shutdown(fd.get_ref().as_raw_fd(), libc::SHUT_WR) }, 0);
    let mut input = VecDeque::from(Vec::from(&b"not-delivered"[..]));
    assert!(tokio::time::timeout(std::time::Duration::from_secs(5), flush_input(&fd, &mut input)).await.unwrap().is_err());
    assert_eq!(input.iter().copied().collect::<Vec<_>>(), b"not-delivered");
}

/// A raw-mode PTY program that never reads its input. It blocks SIGTERM,
/// SIGINT and SIGWINCH before announcing READY, so no signal can interrupt that
/// write; sigwait then reports each resize and exits 128+N on SIGTERM/SIGINT.
/// No-op handlers keep the default-ignored SIGWINCH pending while blocked.
const BLOCKED_INPUT_SOURCE: &str = r#"
#include <signal.h>
#include <string.h>
#include <termios.h>
#include <unistd.h>

static void put(const char *text) {
    size_t length = strlen(text);
    while (length) {
        ssize_t written = write(1, text, length);
        if (written <= 0) _exit(90);
        text += written; length -= (size_t)written;
    }
}
static void ignore(int number) { (void)number; }

int main(void) {
    struct termios mode;
    if (tcgetattr(0, &mode) != 0) return 91;
    cfmakeraw(&mode);
    if (tcsetattr(0, TCSAFLUSH, &mode) != 0) return 92;
    sigset_t signals;
    sigemptyset(&signals);
    const int numbers[] = { SIGTERM, SIGINT, SIGWINCH };
    for (size_t index = 0; index < sizeof(numbers) / sizeof(numbers[0]); index++) {
        struct sigaction action = {0};
        action.sa_handler = ignore;
        if (sigaction(numbers[index], &action, NULL) != 0) return 93;
        sigaddset(&signals, numbers[index]);
    }
    if (sigprocmask(SIG_BLOCK, &signals, NULL) != 0) return 94;
    put("READY\n");
    for (;;) {
        int number;
        if (sigwait(&signals, &number) != 0) return 95;
        if (number == SIGWINCH) {
            put("RESIZED\n");
        } else {
            put("STOPPED\n");
            _exit(128 + number);
        }
    }
}
"#;

#[tokio::test]
async fn blocked_input_still_allows_output_resize_termination_and_explicit_interrupt() {
    let fixture = crate::test_fixture::CFixture::compile("blocked-input", BLOCKED_INPUT_SOURCE, &[]);
    for explicit_interrupt in [false, true] {
        let invocation = crate::native::parse("version", &crate::tui_state::State::new(Default::default())).unwrap();
        // The PTY input remains deliberately unread.
        let command = tokio::process::Command::new(fixture.program());
        let mut session = Session::spawn(command, invocation, 80, 24).unwrap();
        let mut phase = "ready";
        let completed = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            while !session.parser.screen().contents().contains("READY") {
                if let Event::Output(bytes) = session.next().await.unwrap() { session.parser.process(&bytes); }
            }
            session.input(&vec![b'x'; 512 * 1024]).unwrap();
            session.input(&[3]).unwrap(); // Ordinary Ctrl-C remains a raw byte behind the paste.
            assert_eq!(session.input.back(), Some(&3));
            session.resize(100, 33).unwrap();
            phase = "resized";
            while !session.parser.screen().contents().contains("RESIZED") {
                if let Event::Output(bytes) = session.next().await.unwrap() { session.parser.process(&bytes); }
            }
            assert!(!session.input.is_empty(), "fixture must still be backpressured");
            let pending = session.input.len();
            phase = "stopped";
            if explicit_interrupt {
                session.interrupt_input().unwrap();
                assert!(session.input.is_empty());
                assert!(session.input_error.as_ref().unwrap().contains(&format!("discarded {pending} queued bytes")));
            } else { session.signal(libc::SIGTERM).unwrap(); }
            loop {
                match session.next().await.unwrap() {
                    Event::Output(bytes) => session.parser.process(&bytes),
                    Event::Exited(code) => {
                        assert_eq!(code, if explicit_interrupt { 130 } else { 143 });
                        if session.ended { break; }
                    },
                    Event::Ended if session.exit.is_some() => break,
                    Event::Ended | Event::InputProgress => {},
                }
            }
            assert!(session.parser.screen().contents().contains("STOPPED"));
            assert!(session.input_closed && session.input.is_empty());
            assert!(session.input_error.as_ref().is_some_and(|e| if explicit_interrupt { e.contains("discarded") } else { e.contains("not delivered") }));
            let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(160, 33)).unwrap();
            terminal.draw(|frame| session.draw(frame)).unwrap();
            let drawn: String = terminal.backend().buffer().content.iter().map(|c| c.symbol()).collect();
            assert!(drawn.contains("Exit code") && drawn.contains(if explicit_interrupt { "discarded" } else { "not delivered" }));
        }).await;
        assert!(completed.is_ok(), "phase={phase} explicit={explicit_interrupt} exit={:?} ended={} pending={} screen={:?}",
            session.exit, session.ended, session.input.len(), session.parser.screen().contents());
    }
}
