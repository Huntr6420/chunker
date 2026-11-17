#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationKind {
    Split,
    Merge,
    Verify,
}

#[derive(Debug, Clone)]
pub struct ProgressEvent {
    pub operation: OperationKind,
    pub current_chunk: usize,
    pub total_chunks: usize,
    pub bytes_processed: u64,
    pub total_bytes: u64,
}
