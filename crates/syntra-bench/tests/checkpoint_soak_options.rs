use syntra_bench::{CheckpointColumn, Options};

fn parse(extra: &[&str]) -> Result<Options, String> {
    let mut args = vec!["--pg-bin".to_owned(), "/pg/bin".to_owned()];
    args.extend(extra.iter().map(|s| s.to_string()));
    Options::parse(args).map_err(|e| e.to_string())
}

#[test]
fn soak_is_disabled_by_default_and_keeps_stress_defaults() {
    let options = parse(&[]).unwrap();
    assert_eq!(
        (
            options.checkpoint_soak_seconds,
            options.checkpoint_soak_window_seconds,
            options.checkpoint_soak_disk_budget_mb,
            options.checkpoint_stress_seconds,
            options.checkpoint_worker_rate,
        ),
        (0, 60, 2048, 0, 0)
    );
}

#[test]
fn soak_accepts_a_paced_run_with_an_even_number_of_whole_windows() {
    let options = parse(&[
        "--checkpoint-soak-seconds",
        "240",
        "--checkpoint-soak-window-seconds",
        "60",
        "--checkpoint-worker-rate",
        "10",
        "--checkpoint-soak-disk-budget-mb",
        "256",
    ])
    .unwrap();
    assert_eq!(
        (
            options.checkpoint_soak_seconds,
            options.checkpoint_worker_rate,
            options.checkpoint_soak_disk_budget_mb,
        ),
        (240, 10, 256)
    );
    assert_eq!(
        parse(&[
            "--checkpoint-soak-seconds",
            "14400",
            "--checkpoint-soak-window-seconds",
            "600",
            "--checkpoint-worker-rate",
            "1000",
            "--checkpoint-soak-disk-budget-mb",
            "65536",
        ])
        .unwrap()
        .checkpoint_soak_seconds,
        14400
    );
}

#[test]
fn soak_rejects_unpaced_overlapping_or_misaligned_runs() {
    let rejected: [&[&str]; 9] = [
        // Saturated soaks are rejected because they can exhaust local disk.
        &["--checkpoint-soak-seconds", "240"],
        &[
            "--checkpoint-soak-seconds",
            "180",
            "--checkpoint-worker-rate",
            "10",
        ],
        &[
            "--checkpoint-soak-seconds",
            "14460",
            "--checkpoint-worker-rate",
            "10",
        ],
        &[
            "--checkpoint-soak-seconds",
            "240",
            "--checkpoint-worker-rate",
            "10",
            "--checkpoint-stress-seconds",
            "60",
        ],
        // 270 seconds is not a whole number of 60-second windows.
        &[
            "--checkpoint-soak-seconds",
            "270",
            "--checkpoint-worker-rate",
            "10",
        ],
        // Five windows put the snapshot release inside a window.
        &[
            "--checkpoint-soak-seconds",
            "300",
            "--checkpoint-worker-rate",
            "10",
        ],
        &[
            "--checkpoint-soak-seconds",
            "240",
            "--checkpoint-worker-rate",
            "10",
            "--checkpoint-soak-window-seconds",
            "9",
        ],
        &[
            "--checkpoint-soak-seconds",
            "240",
            "--checkpoint-worker-rate",
            "10",
            "--checkpoint-soak-disk-budget-mb",
            "255",
        ],
        &[
            "--checkpoint-soak-seconds",
            "240",
            "--checkpoint-worker-rate",
            "9",
        ],
    ];
    for args in rejected {
        assert!(parse(args).is_err(), "{args:?}");
    }
}

#[test]
fn soak_skip_accepts_optional_actors_only_inside_a_soak() {
    let base = [
        "--checkpoint-soak-seconds",
        "240",
        "--checkpoint-worker-rate",
        "10",
    ];
    let with = |names: &str| parse(&[&base[..], &["--checkpoint-soak-skip", names]].concat());
    assert_eq!(
        with("read_committed,keyed_monitor,vacuum,applier_1,repeatable_read")
            .unwrap()
            .checkpoint_soak_skip
            .len(),
        5
    );
    for mandatory in ["writer_0", "pruner", "applier_0", "nonsense"] {
        assert!(with(mandatory).is_err(), "{mandatory}");
    }
    assert!(parse(&["--checkpoint-soak-skip", "vacuum"]).is_err());
}

#[test]
fn column_type_selects_the_feed_layout_and_rejects_others() {
    let column = |args: &[&str]| parse(args).unwrap().checkpoint_column;
    assert_eq!(column(&[]), CheckpointColumn::Int4);
    for (name, layout) in [
        ("int4", CheckpointColumn::Int4),
        ("int8", CheckpointColumn::Int8),
        ("numeric", CheckpointColumn::Numeric),
    ] {
        assert_eq!(
            column(&["--checkpoint-column-type", name]),
            layout,
            "{name}"
        );
    }
    assert_eq!(
        parse(&["--checkpoint-column-type", "int2"]).unwrap_err(),
        "--checkpoint-column-type must be int4, int8 or numeric"
    );
}
