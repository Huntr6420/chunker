pub mod args;
pub mod signals;
pub mod terminal_progress;

use clap::Parser;
use std::io::Write;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use chunker_core::error::ChunkerError;
use chunker_core::merge::merge_bundle;
use chunker_core::progress::ProgressEvent;
use chunker_core::security::safe_parent;
use chunker_core::split::{SplitSummary, split_file};
use chunker_core::stream::is_cancelled;
use chunker_core::verify::verify_bundle;

use args::{Cli, Commands};
use signals::setup_signal_handler;
use terminal_progress::TerminalProgress;

fn exit_code(result: &Result<(), ChunkerError>) -> i32 {
    match result {
        Ok(()) => 0,
        Err(ChunkerError::InvalidArgument(_)) => 2,
        Err(ChunkerError::Cancelled) => 130,
        Err(_) => 1,
    }
}

pub fn report_result<E: Write>(stderr: &mut E, result: &Result<(), ChunkerError>) -> i32 {
    if let Err(err) = result {
        if !matches!(err, ChunkerError::Cancelled) {
            let _ = writeln!(stderr, "error: {err}");
            // Cancellation must still work if stderr is closed.
        }
    }
    exit_code(result)
}

pub fn print_handler_setup_error<E: Write>(stderr: &mut E, reason: &str) -> i32 {
    let _ = writeln!(
        stderr,
        "error: Could not set up Ctrl+C handling, so nothing was started. Reason: {reason}"
    );
    1
}

pub fn print_split_success<W: Write, E: Write>(
    stdout: &mut W,
    stderr: &mut E,
    summary: &SplitSummary,
) -> i32 {
    if let Err(e) = writeln!(stdout, "Created bundle: {}", summary.bundle_path.display()) {
        let wrapped = ChunkerError::Io(e);
        return report_result(stderr, &Err(wrapped));
    }
    if summary.reused_chunks > 0 {
        if let Err(e) = writeln!(stdout, "Reused chunks: {}", summary.reused_chunks) {
            let wrapped = ChunkerError::Io(e);
            return report_result(stderr, &Err(wrapped));
        }
    }
    report_result(stderr, &Ok(()))
}

pub fn print_merge_success<W: Write, E: Write>(stdout: &mut W, stderr: &mut E, dest: &Path) -> i32 {
    if let Err(e) = writeln!(stdout, "Merged file: {}", dest.display()) {
        let wrapped = ChunkerError::Io(e);
        return report_result(stderr, &Err(wrapped));
    }
    report_result(stderr, &Ok(()))
}

pub fn print_verify_success<W: Write, E: Write>(
    stdout: &mut W,
    stderr: &mut E,
    bundle: &Path,
) -> i32 {
    if let Err(e) = writeln!(stdout, "Verified bundle: {}", bundle.display()) {
        let wrapped = ChunkerError::Io(e);
        return report_result(stderr, &Err(wrapped));
    }
    report_result(stderr, &Ok(()))
}

pub fn run_verify<F>(
    bundle: &Path,
    cancel_token: Option<&Arc<AtomicBool>>,
    on_progress: F,
) -> Result<(), ChunkerError>
where
    F: FnMut(ProgressEvent),
{
    if is_cancelled(cancel_token) {
        return Err(ChunkerError::Cancelled);
    }

    match std::fs::metadata(bundle) {
        Ok(meta) => {
            if !meta.is_dir() {
                return Err(ChunkerError::InvalidArgument(format!(
                    "Bundle path must be a directory: {}",
                    bundle.display()
                )));
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // Let NotFound fall through to verify_bundle
        }
        Err(e) => return Err(ChunkerError::Io(e)),
    }

    verify_bundle(bundle, cancel_token, on_progress)
}

fn run() -> i32 {
    let cli = match Cli::try_parse() {
        Ok(c) => c,
        Err(e) => {
            let _ = e.print();
            return e.exit_code();
        }
    };

    let cancel_token = Arc::new(AtomicBool::new(false));
    if let Err(e) = setup_signal_handler(Arc::clone(&cancel_token)) {
        return print_handler_setup_error(&mut std::io::stderr(), &e.to_string());
    }

    let mut progress = TerminalProgress::new();
    let on_progress = |event: ProgressEvent| {
        progress.on_event(&event);
    };

    let mut stdout = std::io::stdout();
    let mut stderr = std::io::stderr();

    match cli.command {
        Commands::Split {
            input,
            output,
            chunk_size,
        } => {
            let out_dir = output.unwrap_or_else(|| safe_parent(&input).to_path_buf());
            let res = split_file(
                &input,
                &out_dir,
                chunk_size,
                Some(&cancel_token),
                on_progress,
            );
            progress.finish_and_clear();
            match res {
                Ok(summary) => print_split_success(&mut stdout, &mut stderr, &summary),
                Err(err) => report_result(&mut stderr, &Err(err)),
            }
        }
        Commands::Merge {
            bundle,
            output,
            force,
        } => {
            let res = merge_bundle(
                &bundle,
                output.as_deref(),
                force,
                Some(&cancel_token),
                on_progress,
            );
            progress.finish_and_clear();
            match res {
                Ok(dest) => print_merge_success(&mut stdout, &mut stderr, &dest),
                Err(err) => report_result(&mut stderr, &Err(err)),
            }
        }
        Commands::Verify { bundle } => {
            let res = run_verify(&bundle, Some(&cancel_token), on_progress);
            progress.finish_and_clear();
            match res {
                Ok(()) => print_verify_success(&mut stdout, &mut stderr, &bundle),
                Err(err) => report_result(&mut stderr, &Err(err)),
            }
        }
    }
}

fn main() {
    let code = run();
    std::process::exit(code);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    struct BoundedWriter {
        capacity_bytes: usize,
        call_limit: usize,
        calls: usize,
        accepted: Vec<u8>,
    }

    impl BoundedWriter {
        fn new(capacity_bytes: usize, call_limit: usize) -> Self {
            Self {
                capacity_bytes,
                call_limit,
                calls: 0,
                accepted: Vec::new(),
            }
        }
    }

    impl Write for BoundedWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.calls += 1;
            if self.calls > self.call_limit {
                panic!(
                    "BoundedWriter call limit exceeded: {} > {}",
                    self.calls, self.call_limit
                );
            }
            let remaining = self.capacity_bytes.saturating_sub(self.accepted.len());
            if remaining == 0 {
                return Err(std::io::Error::other("stdout closed"));
            }
            let to_take = std::cmp::min(remaining, buf.len());
            self.accepted.extend_from_slice(&buf[..to_take]);
            Ok(to_take)
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    struct FailingDiagnosticWriter {
        call_limit: usize,
        calls: usize,
    }

    impl FailingDiagnosticWriter {
        fn new(call_limit: usize) -> Self {
            Self {
                call_limit,
                calls: 0,
            }
        }
    }

    impl Write for FailingDiagnosticWriter {
        fn write(&mut self, _buf: &[u8]) -> std::io::Result<usize> {
            self.calls += 1;
            if self.calls > self.call_limit {
                panic!("FailingDiagnosticWriter call limit exceeded");
            }
            Err(std::io::Error::other("stderr closed"))
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn test_exit_code_ok() {
        let mut stderr = Vec::new();
        let code = report_result(&mut stderr, &Ok(()));
        assert_eq!(code, 0);
        assert!(stderr.is_empty());
    }

    #[test]
    fn test_exit_code_invalid_argument() {
        let mut stderr = Vec::new();
        let code = report_result(
            &mut stderr,
            &Err(ChunkerError::InvalidArgument("bad arg".into())),
        );
        assert_eq!(code, 2);
        let expected = "error: Invalid argument: bad arg\n";
        assert_eq!(String::from_utf8_lossy(&stderr), expected);
    }

    #[test]
    fn test_exit_code_cancelled() {
        let mut stderr = Vec::new();
        let code = report_result(&mut stderr, &Err(ChunkerError::Cancelled));
        assert_eq!(code, 130);
        assert_eq!(
            stderr.len(),
            0,
            "Cancelled must write exactly zero bytes to stderr"
        );
    }

    #[test]
    fn test_exit_code_other() {
        let mut stderr = Vec::new();
        let err = ChunkerError::Io(std::io::Error::other("disk error"));
        let code = report_result(&mut stderr, &Err(err));
        assert_eq!(code, 1);
        let expected = "error: I/O error: disk error\n";
        assert_eq!(String::from_utf8_lossy(&stderr), expected);

        let mut stderr2 = Vec::new();
        let err2 = ChunkerError::HashMismatch {
            file: "chunk.chk".into(),
            expected: "e".into(),
            actual: "a".into(),
        };
        let code2 = report_result(&mut stderr2, &Err(err2));
        assert_eq!(code2, 1);
        let expected2 = "error: Hash mismatch for chunk.chk: expected e, got a\n";
        assert_eq!(String::from_utf8_lossy(&stderr2), expected2);
    }

    #[test]
    fn test_verify_precedence_token_cancelled_over_invalid_directory() {
        let temp = tempfile::tempdir().unwrap();
        let regular_file = temp.path().join("not_a_dir.bin");
        std::fs::write(&regular_file, b"content").unwrap();

        let token = Arc::new(AtomicBool::new(true));
        let res = run_verify(&regular_file, Some(&token), |_| {});
        assert!(
            matches!(res, Err(ChunkerError::Cancelled)),
            "Expected Err(Cancelled), got: {res:?}"
        );
    }

    #[test]
    fn test_split_output_failure_first_line() {
        let summary = SplitSummary {
            bundle_path: PathBuf::from("target.chunks"),
            reused_chunks: 0,
        };
        let mut stdout = BoundedWriter::new(0, 5);
        let mut stderr = Vec::new();

        let code = print_split_success(&mut stdout, &mut stderr, &summary);
        assert_eq!(code, 1);
        assert!(stdout.accepted.is_empty());
        let expected_diag = "error: I/O error: stdout closed\n";
        assert_eq!(String::from_utf8_lossy(&stderr), expected_diag);
    }

    #[test]
    fn test_split_output_failure_second_line() {
        let summary = SplitSummary {
            bundle_path: PathBuf::from("target.chunks"),
            reused_chunks: 3,
        };
        let first_line = "Created bundle: target.chunks\n";
        let mut stdout = BoundedWriter::new(first_line.len(), 5);
        let mut stderr = Vec::new();

        let code = print_split_success(&mut stdout, &mut stderr, &summary);
        assert_eq!(code, 1);
        assert_eq!(String::from_utf8_lossy(&stdout.accepted), first_line);
        let expected_diag = "error: I/O error: stdout closed\n";
        assert_eq!(String::from_utf8_lossy(&stderr), expected_diag);
    }

    #[test]
    fn test_merge_output_failure() {
        let mut stdout = BoundedWriter::new(0, 5);
        let mut stderr = Vec::new();

        let code = print_merge_success(&mut stdout, &mut stderr, Path::new("merged.bin"));
        assert_eq!(code, 1);
        assert!(stdout.accepted.is_empty());
        let expected_diag = "error: I/O error: stdout closed\n";
        assert_eq!(String::from_utf8_lossy(&stderr), expected_diag);
    }

    #[test]
    fn test_verify_output_failure() {
        let mut stdout = BoundedWriter::new(0, 5);
        let mut stderr = Vec::new();

        let code = print_verify_success(&mut stdout, &mut stderr, Path::new("bundle.chunks"));
        assert_eq!(code, 1);
        assert!(stdout.accepted.is_empty());
        let expected_diag = "error: I/O error: stdout closed\n";
        assert_eq!(String::from_utf8_lossy(&stderr), expected_diag);
    }

    #[test]
    fn test_diagnostic_writer_also_fails() {
        let summary = SplitSummary {
            bundle_path: PathBuf::from("target.chunks"),
            reused_chunks: 0,
        };
        let mut stdout = BoundedWriter::new(0, 5);
        let mut stderr = FailingDiagnosticWriter::new(1);

        let code = print_split_success(&mut stdout, &mut stderr, &summary);
        assert_eq!(code, 1);
        assert_eq!(stderr.calls, 1);
    }

    #[test]
    fn test_success_output_on_both_writers() {
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();

        // Split without reuse
        let s1 = SplitSummary {
            bundle_path: PathBuf::from("b1.chunks"),
            reused_chunks: 0,
        };
        assert_eq!(print_split_success(&mut stdout, &mut stderr, &s1), 0);
        assert_eq!(
            String::from_utf8_lossy(&stdout),
            "Created bundle: b1.chunks\n"
        );
        assert!(stderr.is_empty());

        stdout.clear();
        stderr.clear();

        // Split with reuse
        let s2 = SplitSummary {
            bundle_path: PathBuf::from("b2.chunks"),
            reused_chunks: 4,
        };
        assert_eq!(print_split_success(&mut stdout, &mut stderr, &s2), 0);
        assert_eq!(
            String::from_utf8_lossy(&stdout),
            "Created bundle: b2.chunks\nReused chunks: 4\n"
        );
        assert!(stderr.is_empty());

        stdout.clear();
        stderr.clear();

        // Merge
        assert_eq!(
            print_merge_success(&mut stdout, &mut stderr, Path::new("out.bin")),
            0
        );
        assert_eq!(String::from_utf8_lossy(&stdout), "Merged file: out.bin\n");
        assert!(stderr.is_empty());

        stdout.clear();
        stderr.clear();

        // Verify
        assert_eq!(
            print_verify_success(&mut stdout, &mut stderr, Path::new("b.chunks")),
            0
        );
        assert_eq!(
            String::from_utf8_lossy(&stdout),
            "Verified bundle: b.chunks\n"
        );
        assert!(stderr.is_empty());
    }

    #[test]
    fn test_handler_setup_error_format() {
        let mut stderr = Vec::new();
        let code = print_handler_setup_error(&mut stderr, "access denied");
        assert_eq!(code, 1);
        assert_eq!(
            String::from_utf8_lossy(&stderr),
            "error: Could not set up Ctrl+C handling, so nothing was started. Reason: access denied\n"
        );
    }
}
