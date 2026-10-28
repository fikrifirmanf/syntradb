#[path = "../../../build-support/source_fingerprint.rs"]
mod source_fingerprint;

use source_fingerprint::fingerprint_files;
use std::fs;
use std::path::PathBuf;

#[test]
fn fingerprint_matches_independent_sha256_fixture() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("a.rs"), b"fn main() {}\n").unwrap();
    // Python hashlib with the versioned domain and little-endian length fields.
    assert_eq!(
        fingerprint_files(root.path(), &["a.rs".into()]).unwrap(),
        "617b60b6151508eae3c21555437187d6dfbe7d57151a49257fc73d5606195ff0"
    );
}

#[test]
fn fingerprint_changes_with_file_contents_and_names() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("a.rs"), b"one").unwrap();
    let first = fingerprint_files(root.path(), &["a.rs".into()]).unwrap();
    fs::write(root.path().join("a.rs"), b"two").unwrap();
    let second = fingerprint_files(root.path(), &["a.rs".into()]).unwrap();
    assert_ne!(first, second);
    fs::rename(root.path().join("a.rs"), root.path().join("b.rs")).unwrap();
    assert_ne!(
        second,
        fingerprint_files(root.path(), &["b.rs".into()]).unwrap()
    );
}

#[test]
fn fingerprint_is_independent_of_input_order_duplicates_and_root_location() {
    let root = tempfile::tempdir().unwrap();
    let other = tempfile::tempdir().unwrap();
    for location in [root.path(), other.path()] {
        fs::write(location.join("a.rs"), b"one").unwrap();
        fs::write(location.join("b.sql"), b"two").unwrap();
    }
    let first =
        fingerprint_files(root.path(), &["b.sql".into(), "a.rs".into(), "a.rs".into()]).unwrap();
    assert_eq!(
        first,
        fingerprint_files(other.path(), &["a.rs".into(), "b.sql".into()]).unwrap()
    );
}

#[test]
fn fingerprint_rejects_absolute_traversal_and_empty_file_names() {
    let root = tempfile::tempdir().unwrap();
    for path in [root.path().join("a.rs"), "../a.rs".into(), PathBuf::new()] {
        assert!(fingerprint_files(root.path(), &[path]).is_err());
    }
}

#[cfg(unix)]
#[test]
fn fingerprint_rejects_symlink_files_and_directories() {
    use std::os::unix::fs::symlink;
    let root = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join("real")).unwrap();
    fs::write(root.path().join("real/a.rs"), b"one").unwrap();
    symlink("real/a.rs", root.path().join("alias.rs")).unwrap();
    symlink("real", root.path().join("alias")).unwrap();
    assert!(fingerprint_files(root.path(), &["alias.rs".into()]).is_err());
    assert!(fingerprint_files(root.path(), &["alias/a.rs".into()]).is_err());
}

#[test]
fn benchmark_rejects_mismatched_or_malformed_source_fingerprints() {
    let first = "a".repeat(64);
    let second = "b".repeat(64);
    assert!(syntra_bench::verify_source_fingerprints(&first, &first).is_ok());
    for other in [
        second,
        "a".repeat(63),
        "A".repeat(64),
        "g".repeat(64),
        String::new(),
    ] {
        assert!(syntra_bench::verify_source_fingerprints(&first, &other).is_err());
    }
    assert!(syntra_bench::verify_source_fingerprints(&"g".repeat(64), &"g".repeat(64)).is_err());
}
