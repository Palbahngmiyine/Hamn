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

#[tokio::test]
async fn blocked_input_still_allows_output_resize_termination_and_explicit_interrupt() {
    let interpreter = std::process::Command::new("python3").args(["-c", "import sys; print(sys.executable)"]).output().unwrap();
    assert!(interpreter.status.success());
    let interpreter = String::from_utf8(interpreter.stdout).unwrap();
    for explicit_interrupt in [false, true] {
        let invocation = crate::native::parse("version", &crate::tui_state::State::new(Default::default())).unwrap();
        let mut command = tokio::process::Command::new(interpreter.trim());
        command.args(["-c", "import os,signal,sys,tty\ntty.setraw(0)\ndef done(signum,*_):\n print('STOPPED',flush=True);sys.exit(128+signum)\nsignal.signal(signal.SIGTERM,done)\nsignal.signal(signal.SIGINT,done)\nsignal.signal(signal.SIGWINCH,lambda *_:print('RESIZED',flush=True))\nprint('READY',flush=True)\nwhile True: signal.pause()"]);
        let mut session = Session::spawn(command, invocation, 80, 24).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            while !session.parser.screen().contents().contains("READY") {
                if let Event::Output(bytes) = session.next().await.unwrap() { session.parser.process(&bytes); }
            }
            session.input(&vec![b'x'; 512 * 1024]).unwrap();
            session.input(&[3]).unwrap(); // Ordinary Ctrl-C remains a raw byte behind the paste.
            assert_eq!(session.input.back(), Some(&3));
            session.resize(100, 33).unwrap();
            while !session.parser.screen().contents().contains("RESIZED") {
                if let Event::Output(bytes) = session.next().await.unwrap() { session.parser.process(&bytes); }
            }
            assert!(!session.input.is_empty(), "fixture must still be backpressured");
            let pending = session.input.len();
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
        }).await.unwrap();
    }
}
