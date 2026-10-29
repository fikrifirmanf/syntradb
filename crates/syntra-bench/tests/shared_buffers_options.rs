use syntra_bench::Options;

fn shared_buffers(value: Option<&str>) -> Result<u64, String> {
    let mut args = vec!["--pg-bin".to_owned(), "/pg/bin".to_owned()];
    if let Some(value) = value {
        args.extend(["--shared-buffers-mb".to_owned(), value.to_owned()]);
    }
    Options::parse(args)
        .map(|options| options.shared_buffers_mb)
        .map_err(|error| error.to_string())
}

#[test]
fn shared_buffers_keeps_the_server_default_unless_set() {
    assert_eq!(shared_buffers(None), Ok(0));
    assert_eq!(shared_buffers(Some("0")), Ok(0));
    assert_eq!(shared_buffers(Some("16")), Ok(16));
    assert_eq!(shared_buffers(Some("1024")), Ok(1024));
    assert_eq!(shared_buffers(Some("16384")), Ok(16384));
}

#[test]
fn shared_buffers_rejects_sizes_outside_the_range_and_non_numbers() {
    for value in ["15", "16385", "-1", "1GB", ""] {
        assert!(shared_buffers(Some(value)).is_err(), "accepted {value:?}");
    }
    assert_eq!(
        shared_buffers(Some("8")).unwrap_err(),
        "shared buffers must be zero or between 16 and 16384 MB"
    );
}
