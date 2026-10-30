#![no_main]

use libfuzzer_sys::fuzz_target;
use std::collections::BTreeMap;
use syntra_exec::{group_int4, group_int8, GroupingError};

type SummaryRow = (
    Option<i64>,
    i64,
    i64,
    Option<i128>,
    Option<i64>,
    Option<i64>,
);

#[derive(Default)]
struct Expected {
    rows: i64,
    non_nulls: i64,
    sum: Option<i128>,
    min: Option<i64>,
    max: Option<i64>,
}

fuzz_target!(|data: &[u8]| {
    if data.len() < 2 {
        return;
    }
    let wide = data[0] & 1 != 0;
    let limit = usize::from(data[1]);
    let mut input = Vec::new();
    let mut expected: BTreeMap<Option<i64>, Expected> = BTreeMap::new();
    for row in data[2..].as_chunks::<17>().0 {
        let flags = row[0];
        let mut key = i64::from_le_bytes(row[1..9].try_into().unwrap());
        let mut value = i64::from_le_bytes(row[9..17].try_into().unwrap());
        // Exercise both repeated small keys and distinct full-width keys.
        if flags & 4 == 0 {
            key %= 8;
        }
        if !wide {
            key = i64::from(key as i32);
            value = i64::from(value as i32);
        }
        let key = (flags & 1 == 0).then_some(key);
        let value = (flags & 2 == 0).then_some(value);
        input.push((key, value));
        // Independent ordered-map oracle with direct arithmetic. It never
        // calls the aggregate kernel or the execution implementation.
        let state = expected.entry(key).or_default();
        state.rows += 1;
        if let Some(value) = value {
            state.non_nulls += 1;
            state.sum = Some(state.sum.unwrap_or(0) + i128::from(value));
            state.min = Some(state.min.map_or(value, |old| old.min(value)));
            state.max = Some(state.max.map_or(value, |old| old.max(value)));
        }
    }
    let result: Result<Vec<SummaryRow>, _> = if wide {
        group_int8(input.iter().copied(), limit, || true).map(|groups| {
            groups
                .into_iter()
                .map(|group| {
                    let s = group.summary;
                    (
                        group.key,
                        s.rows(),
                        s.non_nulls(),
                        s.sum(),
                        s.min(),
                        s.max(),
                    )
                })
                .collect()
        })
    } else {
        group_int4(
            input
                .iter()
                .map(|(key, value)| (key.map(|k| k as i32), value.map(|v| v as i32))),
            limit,
            || true,
        )
        .map(|groups| {
            groups
                .into_iter()
                .map(|group| {
                    let s = group.summary;
                    (
                        group.key.map(i64::from),
                        s.rows(),
                        s.non_nulls(),
                        s.sum().map(i128::from),
                        s.min().map(i64::from),
                        s.max().map(i64::from),
                    )
                })
                .collect()
        })
    };
    if limit == 0 {
        assert_eq!(result, Err(GroupingError::InvalidLimit));
    } else if expected.len() > limit {
        assert_eq!(result, Err(GroupingError::GroupLimitExceeded { limit }));
    } else {
        let oracle = expected
            .into_iter()
            .map(|(key, s)| (key, s.rows, s.non_nulls, s.sum, s.min, s.max))
            .collect::<Vec<_>>();
        assert_eq!(result, Ok(oracle));
    }
});
