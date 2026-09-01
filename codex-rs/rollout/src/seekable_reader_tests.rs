use std::fs;
use std::io::Read;
use std::io::Seek;
use std::io::SeekFrom;
use std::io::Write;

use pretty_assertions::assert_eq;

use super::*;

#[test]
fn compressed_offsets_preserve_exact_bytes_and_open_snapshots() -> io::Result<()> {
    let home = tempfile::tempdir()?;
    let path = home.path().join("rollout.jsonl");
    let compressed = path.with_extension("jsonl.zst");
    let original = "first\r\nsecond 🦀\nthird\n".as_bytes();
    fs::write(
        &compressed,
        zstd::stream::encode_all(original, /*level*/ 3)?,
    )?;

    let mut reader = open_rollout_seekable_reader(&path)?;
    // The retained snapshot must remain valid after another caller materializes the logical path.
    fs::write(&path, b"replacement\n")?;
    fs::remove_file(&compressed)?;
    reader.seek(SeekFrom::Start(7))?;
    let mut suffix = Vec::new();
    reader.read_to_end(&mut suffix)?;
    assert_eq!(suffix, original[7..]);

    let mut current = Vec::new();
    open_rollout_seekable_reader(&compressed)?.read_to_end(&mut current)?;
    assert_eq!(current, b"replacement\n");

    let mut plain_snapshot = open_rollout_seekable_reader(&path)?;
    fs::write(
        &compressed,
        zstd::stream::encode_all(current.as_slice(), /*level*/ 3)?,
    )?;
    fs::remove_file(&path)?;
    let mut retained = Vec::new();
    plain_snapshot.read_to_end(&mut retained)?;
    assert_eq!(retained, current);
    Ok(())
}

#[test]
fn source_byte_limit_gates_compressed_decoding_and_reports_physical_bytes() -> io::Result<()> {
    let home = tempfile::tempdir()?;
    let path = home.path().join("rollout.jsonl");
    let compressed_path = path.with_extension("jsonl.zst");
    let original = b"first\nsecond\nthird\n";
    let compressed = zstd::stream::encode_all(original.as_slice(), /*level*/ 3)?;
    let compressed_bytes = u64::try_from(compressed.len()).map_err(io::Error::other)?;
    fs::write(&compressed_path, compressed)?;

    assert!(matches!(
        open_rollout_seekable_reader_with_source_byte_limit(&path, compressed_bytes - 1)?,
        SourceByteLimitedSeekableReader::LimitExceeded
    ));

    let SourceByteLimitedSeekableReader::Decoded {
        mut file,
        source_bytes_read,
    } = open_rollout_seekable_reader_with_source_byte_limit(&path, compressed_bytes)?
    else {
        panic!("compressed rollout should decode within its exact physical byte limit");
    };
    let mut decoded = Vec::new();
    file.read_to_end(&mut decoded)?;
    assert_eq!(decoded, original);
    assert_eq!(source_bytes_read, compressed_bytes);
    Ok(())
}

#[test]
fn prefix_bounds_support_sized_unsized_and_concatenated_frames() -> io::Result<()> {
    let home = tempfile::tempdir()?;
    let path = home.path().join("rollout.jsonl.zst");
    let first = "first 🦀\n".as_bytes();
    let second = b"second\n";
    let mut encoder = zstd::stream::write::Encoder::new(Vec::new(), /*level*/ 3)?;
    encoder.set_pledged_src_size(Some(first.len() as u64))?;
    encoder.write_all(first)?;
    let sized = encoder.finish()?;
    let without_size = zstd::stream::encode_all(first, /*level*/ 3)?;
    let mut concatenated = sized.clone();
    concatenated.extend(zstd::stream::encode_all(
        second.as_slice(),
        /*level*/ 3,
    )?);

    for (compressed, expected) in [
        (sized, first.to_vec()),
        (without_size, first.to_vec()),
        (concatenated, [first, second].concat()),
    ] {
        fs::write(&path, compressed)?;
        assert!(rollout_contains_prefix(&path, first.len() as u64)?);
        assert!(rollout_contains_prefix(&path, expected.len() as u64)?);
        assert!(!rollout_contains_prefix(&path, expected.len() as u64 + 1)?);
        let mut actual = Vec::new();
        open_rollout_seekable_reader(&path)?.read_to_end(&mut actual)?;
        assert_eq!(actual, expected);
        assert!(!path.with_extension("").exists());
    }
    Ok(())
}
