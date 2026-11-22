use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    os::unix::fs::MetadataExt,
    path::Path,
};

use anyhow::{Context, Result};
use lz4_flex::frame::{FrameDecoder, FrameEncoder, FrameInfo};

#[derive(Debug, Default, Clone, Copy)]
pub struct Usage {
    pub bytes: u64,
    pub allocated: u64,
}

pub fn usage(path: &Path) -> Result<Usage> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.is_symlink() {
        return Ok(Usage::default());
    }
    let mut total = Usage {
        bytes: if metadata.is_file() {
            metadata.len()
        } else {
            0
        },
        allocated: metadata.blocks() * 512,
    };
    if metadata.is_dir() {
        for entry in fs::read_dir(path)? {
            let child = usage(&entry?.path())?;
            total.bytes += child.bytes;
            total.allocated += child.allocated;
        }
    }
    Ok(total)
}

pub fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{size:.1} {}", UNITS[unit])
    }
}

pub fn compress_scrollback(source: &Path, destination: &Path) -> Result<u64> {
    let mut input = File::open(source).context("could not open scrollback")?;
    let output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)?;
    let frame = FrameInfo::new()
        .content_size(Some(input.metadata()?.len()))
        .content_checksum(true);
    let mut encoder = FrameEncoder::with_frame_info(frame, output);
    let bytes = io::copy(&mut input, &mut encoder).context("could not compress scrollback")?;
    encoder
        .finish()
        .context("could not finish LZ4 scrollback")?;
    Ok(bytes)
}

pub fn replay_scrollback(path: &Path, output: &mut impl Write) -> Result<u64> {
    let input = File::open(path).context("could not open saved scrollback")?;
    let mut reader: Box<dyn Read> = if path.extension().is_some_and(|extension| extension == "lz4")
    {
        Box::new(FrameDecoder::new(input))
    } else {
        Box::new(input)
    };
    io::copy(&mut reader, output).context("could not replay saved scrollback")
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::symlink;

    use tempfile::TempDir;

    use super::*;

    #[test]
    fn lz4_round_trip_preserves_terminal_bytes_and_plain_snapshots() {
        let temp = TempDir::new().unwrap();
        let original = "π session\n\u{1b}[32mterminal output\u{1b}[0m\n"
            .repeat(20_000)
            .into_bytes();
        let plain = temp.path().join("history.txt");
        let compressed = temp.path().join("history.txt.lz4");
        fs::write(&plain, &original).unwrap();
        assert_eq!(
            compress_scrollback(&plain, &compressed).unwrap(),
            original.len() as u64
        );
        assert!(compressed.metadata().unwrap().len() < original.len() as u64 / 10);
        for path in [&plain, &compressed] {
            let mut restored = Vec::new();
            replay_scrollback(path, &mut restored).unwrap();
            assert_eq!(restored, original);
        }
        let mut corrupted = fs::read(&compressed).unwrap();
        let last = corrupted.len() - 1;
        corrupted[last] ^= 0xff;
        fs::write(&compressed, corrupted).unwrap();
        assert!(replay_scrollback(&compressed, &mut Vec::new()).is_err());
    }

    #[test]
    fn storage_sizes_do_not_follow_symlinks() {
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("snapshot");
        fs::create_dir(&root).unwrap();
        fs::write(root.join("state.json"), b"metadata").unwrap();
        let outside = temp.path().join("outside");
        fs::write(&outside, vec![0u8; 100_000]).unwrap();
        symlink(&outside, root.join("link")).unwrap();
        let measured = usage(&root).unwrap();
        assert_eq!(measured.bytes, 8);
        assert!(measured.allocated < 100_000);
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(1024), "1.0 KiB");
        assert_eq!(format_bytes(1024 * 1024), "1.0 MiB");
    }
}
