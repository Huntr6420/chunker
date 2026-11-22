use clap::{Parser, Subcommand};
use std::path::PathBuf;

pub fn parse_chunk_size(s: &str) -> Result<u64, String> {
    let mib = s.parse::<u64>().map_err(|e| e.to_string())?;
    if mib == 0 {
        return Err("chunk size must be at least 1 MiB".to_string());
    }
    mib.checked_mul(1024 * 1024)
        .ok_or_else(|| "chunk size is too large".to_string())
}

#[derive(Parser, Debug)]
#[command(name = "chunker", about = "Large file chunker and merger")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand, Debug, PartialEq, Eq)]
pub enum Commands {
    Split {
        input: PathBuf,
        #[arg(long)]
        output: Option<PathBuf>,
        #[arg(long, default_value = "500", value_parser = parse_chunk_size)]
        chunk_size: u64,
    },
    Merge {
        bundle: PathBuf,
        #[arg(long)]
        output: Option<PathBuf>,
        #[arg(long)]
        force: bool,
    },
    Verify {
        bundle: PathBuf,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_chunk_size_cases() {
        assert_eq!(
            parse_chunk_size("0").unwrap_err(),
            "chunk size must be at least 1 MiB"
        );
        assert_eq!(parse_chunk_size("1").unwrap(), 1024 * 1024);
        assert_eq!(
            parse_chunk_size("000000000000000000001").unwrap(),
            1024 * 1024
        );
        assert_eq!(
            parse_chunk_size("17592186044415").unwrap(),
            17592186044415 * 1024 * 1024
        );
        assert_eq!(
            parse_chunk_size("17592186044416").unwrap_err(),
            "chunk size is too large"
        );

        let expected_parse_err = "18446744073709551616"
            .parse::<u64>()
            .unwrap_err()
            .to_string();
        let err_2_64 = parse_chunk_size("18446744073709551616").unwrap_err();
        assert_eq!(err_2_64, expected_parse_err);
        assert!(!err_2_64.contains("chunk size is too large"));

        let non_num_err = parse_chunk_size("not-a-number").unwrap_err();
        assert_eq!(
            non_num_err,
            "not-a-number".parse::<u64>().unwrap_err().to_string()
        );
    }
}
