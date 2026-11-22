use std::io::Write;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignalAction {
    FirstPress,
    SecondPress,
}

pub fn process_signal_press<W: Write>(
    seen_first_press: &AtomicBool,
    cancel_token: &AtomicBool,
    writer: &mut W,
) -> SignalAction {
    if !seen_first_press.swap(true, Ordering::SeqCst) {
        cancel_token.store(true, Ordering::SeqCst);
        let _ = writeln!(
            writer,
            "\n[!] Cancellation requested. Cleaning up active temporary files... (Press Ctrl+C again to force exit)"
        );
        // Cancellation must still work if stderr is closed.
        SignalAction::FirstPress
    } else {
        let _ = writeln!(writer, "\n[!] Force exiting immediately.");
        // Cancellation must still work if stderr is closed.
        SignalAction::SecondPress
    }
}

pub fn setup_signal_handler(cancel_token: Arc<AtomicBool>) -> anyhow::Result<()> {
    static SEEN_FIRST_PRESS: AtomicBool = AtomicBool::new(false);
    ctrlc::set_handler(move || {
        let action = process_signal_press(&SEEN_FIRST_PRESS, &cancel_token, &mut std::io::stderr());
        if action == SignalAction::SecondPress {
            std::process::exit(130);
        }
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TokenCheckingWriter<'a> {
        token: &'a AtomicBool,
        first_write_token: Option<bool>,
        output: Vec<u8>,
    }

    impl Write for TokenCheckingWriter<'_> {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            if self.first_write_token.is_none() {
                self.first_write_token = Some(self.token.load(Ordering::SeqCst));
            }
            self.output.extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    struct FailingWriter {
        call_limit: usize,
        calls: usize,
    }

    impl Write for FailingWriter {
        fn write(&mut self, _buf: &[u8]) -> std::io::Result<usize> {
            self.calls += 1;
            if self.calls > self.call_limit {
                panic!(
                    "FailingWriter call limit exceeded: {} > {}",
                    self.calls, self.call_limit
                );
            }
            Err(std::io::Error::other("stderr closed"))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn test_signal_press_and_notice_writing() {
        let seen = AtomicBool::new(false);
        let token = AtomicBool::new(false);

        // First press
        let mut writer1 = TokenCheckingWriter {
            token: &token,
            first_write_token: None,
            output: Vec::new(),
        };
        let action1 = process_signal_press(&seen, &token, &mut writer1);
        assert_eq!(action1, SignalAction::FirstPress);
        assert!(token.load(Ordering::SeqCst));
        assert_eq!(
            writer1.first_write_token,
            Some(true),
            "Token must be set BEFORE notice write"
        );
        let expected1 = "\n[!] Cancellation requested. Cleaning up active temporary files... (Press Ctrl+C again to force exit)\n";
        assert_eq!(writer1.output, expected1.as_bytes());

        // Second press
        let mut writer2 = Vec::new();
        let action2 = process_signal_press(&seen, &token, &mut writer2);
        assert_eq!(action2, SignalAction::SecondPress);
        let expected2 = "\n[!] Force exiting immediately.\n";
        assert_eq!(writer2, expected2.as_bytes());
    }

    #[test]
    fn test_signal_press_with_failing_writer() {
        let seen = AtomicBool::new(false);
        let token = AtomicBool::new(false);
        let mut failing = FailingWriter {
            call_limit: 5,
            calls: 0,
        };

        let action1 = process_signal_press(&seen, &token, &mut failing);
        assert_eq!(action1, SignalAction::FirstPress);
        assert!(token.load(Ordering::SeqCst));

        let action2 = process_signal_press(&seen, &token, &mut failing);
        assert_eq!(action2, SignalAction::SecondPress);
        assert!(failing.calls > 0);
    }
}
