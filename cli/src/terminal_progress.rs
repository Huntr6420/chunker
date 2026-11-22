use chunker_core::ProgressEvent;
use indicatif::{ProgressBar, ProgressStyle};
use std::io::IsTerminal;

pub fn should_display_progress(is_terminal: bool, total_bytes: u64) -> bool {
    is_terminal && total_bytes > 0
}

pub fn clamp_display_position(bytes_processed: u64, total_bytes: u64) -> u64 {
    std::cmp::min(bytes_processed, total_bytes)
}

pub struct TerminalProgress {
    is_terminal: bool,
    bar: Option<ProgressBar>,
}

impl Default for TerminalProgress {
    fn default() -> Self {
        Self::new()
    }
}

impl TerminalProgress {
    pub fn new() -> Self {
        Self {
            is_terminal: std::io::stderr().is_terminal(),
            bar: None,
        }
    }

    pub fn with_terminal(is_terminal: bool) -> Self {
        Self {
            is_terminal,
            bar: None,
        }
    }

    pub fn on_event(&mut self, event: &ProgressEvent) {
        if !should_display_progress(self.is_terminal, event.total_bytes) {
            return;
        }

        let bar = self.bar.get_or_insert_with(|| {
            let pb = ProgressBar::new(event.total_bytes);
            let style = ProgressStyle::with_template(
                "{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {bytes}/{total_bytes} ({eta})",
            )
            .unwrap_or_else(|_| ProgressStyle::default_bar());
            pb.set_style(style);
            pb
        });

        bar.set_position(clamp_display_position(
            event.bytes_processed,
            event.total_bytes,
        ));
    }

    pub fn finish_and_clear(&mut self) {
        if let Some(bar) = self.bar.take() {
            bar.finish_and_clear();
        }
    }
}

impl Drop for TerminalProgress {
    fn drop(&mut self) {
        self.finish_and_clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chunker_core::OperationKind;

    #[test]
    fn test_progress_helpers() {
        // Display decisions
        assert!(!should_display_progress(false, 1000));
        assert!(!should_display_progress(false, 0));
        assert!(!should_display_progress(true, 0)); // Synthetic zero-total event
        assert!(should_display_progress(true, 1000));

        // Clamping
        assert_eq!(clamp_display_position(500, 1000), 500);
        assert_eq!(clamp_display_position(1000, 1000), 1000);
        assert_eq!(clamp_display_position(1500, 1000), 1000); // Clamped over-total
    }

    #[test]
    fn test_terminal_progress_lifecycle() {
        let mut non_term = TerminalProgress::with_terminal(false);
        let event = ProgressEvent {
            operation: OperationKind::Split,
            current_chunk: 0,
            total_chunks: 1,
            bytes_processed: 500,
            total_bytes: 1000,
        };
        non_term.on_event(&event);
        assert!(non_term.bar.is_none());

        let mut term_zero = TerminalProgress::with_terminal(true);
        let zero_event = ProgressEvent {
            operation: OperationKind::Split,
            current_chunk: 0,
            total_chunks: 0,
            bytes_processed: 0,
            total_bytes: 0,
        };
        term_zero.on_event(&zero_event);
        assert!(term_zero.bar.is_none());

        let mut term = TerminalProgress::with_terminal(true);
        term.on_event(&event);
        assert!(term.bar.is_some());
        term.finish_and_clear();
        assert!(term.bar.is_none());
    }
}
