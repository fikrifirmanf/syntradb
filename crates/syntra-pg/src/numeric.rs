//! `numeric(p,s)` capture values with 1 <= p <= 18 and 0 <= s <= p, stored as
//! the integer value * 10^s. Every such value is below 10^18 in magnitude, so it
//! fits the 8-byte layout. See docs/numeric-design.md.
use pgrx::AnyNumeric;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Scale {
    pub(crate) precision: u8,
    pub(crate) scale: u8,
}

/// The precision and scale a `numeric` typmod declares, if capture supports them.
pub(crate) fn from_typmod(typmod: i32) -> Option<Scale> {
    // PostgreSQL packs ((p << 16) | (s & 0x7ff)) + VARHDRSZ; s is 11-bit signed.
    let packed = typmod.checked_sub(4).filter(|_| typmod >= 4)?;
    let precision = (packed >> 16) & 0xffff;
    let scale = ((packed & 0x7ff) ^ 1024) - 1024;
    ((1..=18).contains(&precision) && (0..=precision).contains(&scale)).then_some(Scale {
        precision: precision as u8,
        scale: scale as u8,
    })
}

/// True when a stored value lies within the column's precision.
pub(crate) fn fits(value: i64, scale: Scale) -> bool {
    value.unsigned_abs() < 10_u64.pow(u32::from(scale.precision))
}

/// The exact stored integer for a decimal text such as `-12.30`, or None when
/// the text has more fractional digits than `scale`, an exponent, `NaN`, or a
/// magnitude outside the precision.
pub(crate) fn parse(text: &str, scale: Scale) -> Option<i64> {
    let (negative, digits) = match text.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, text),
    };
    let (whole, fraction) = digits.split_once('.').unwrap_or((digits, ""));
    if whole.is_empty()
        || fraction.len() > usize::from(scale.scale)
        || !whole
            .bytes()
            .chain(fraction.bytes())
            .all(|b| b.is_ascii_digit())
    {
        return None;
    }
    let mut value: i64 = 0;
    let padded = fraction.bytes().chain(std::iter::repeat_n(
        b'0',
        usize::from(scale.scale) - fraction.len(),
    ));
    for b in whole.bytes().chain(padded) {
        value = value.checked_mul(10)?.checked_add(i64::from(b - b'0'))?;
    }
    let value = if negative { -value } else { value };
    fits(value, scale).then_some(value)
}

/// The decimal text of a stored integer, or of a sum of them, at `scale`.
pub(crate) fn format(value: i128, scale: Scale) -> String {
    let digits = value.unsigned_abs().to_string();
    let s = usize::from(scale.scale);
    let sign = if value < 0 { "-" } else { "" };
    if s == 0 {
        return format!("{sign}{digits}");
    }
    let padded = format!("{digits:0>width$}", width = s + 1);
    let (whole, fraction) = padded.split_at(padded.len() - s);
    format!("{sign}{whole}.{fraction}")
}

/// A captured value as its stored integer; errors on `NaN` or a value the
/// column's type could not hold.
pub(crate) fn to_stored(value: &AnyNumeric, scale: Scale) -> Result<i64, &'static str> {
    if value.is_nan() {
        return Err("capture cannot store NaN in a numeric column");
    }
    parse(&value.to_string(), scale).ok_or("capture value does not match its numeric column")
}

pub(crate) fn to_numeric(value: i128, scale: Scale) -> AnyNumeric {
    AnyNumeric::try_from(format(value, scale).as_str())
        .unwrap_or_else(|_| pgrx::error!("invalid_checkpoint"))
}

#[cfg(test)]
mod tests {
    use super::*;
    const S2: Scale = Scale {
        precision: 10,
        scale: 2,
    };
    const S0: Scale = Scale {
        precision: 18,
        scale: 0,
    };

    #[test]
    fn typmods_decode_precision_and_scale() {
        let typmod = |p: i32, s: i32| ((p << 16) | (s & 0x7ff)) + 4;
        assert_eq!(from_typmod(typmod(10, 2)), Some(S2));
        assert_eq!(from_typmod(typmod(18, 0)), Some(S0));
        assert_eq!(from_typmod(typmod(18, 18)).map(|s| s.scale), Some(18));
        for (p, s) in [(19, 0), (0, 0), (5, 6), (5, -1)] {
            assert_eq!(from_typmod(typmod(p, s)), None, "numeric({p},{s})");
        }
        assert_eq!(from_typmod(-1), None);
    }

    #[test]
    fn text_round_trips_exactly_at_the_limits() {
        for (text, stored) in [
            ("0.00", 0),
            ("12.30", 1230),
            ("-0.05", -5),
            ("99999999.99", 9_999_999_999),
            ("-99999999.99", -9_999_999_999),
        ] {
            assert_eq!(parse(text, S2), Some(stored), "{text}");
            assert_eq!(format(i128::from(stored), S2), text);
        }
        assert_eq!(
            parse("999999999999999999", S0),
            Some(999_999_999_999_999_999)
        );
        assert_eq!(format(-999_999_999_999_999_999, S0), "-999999999999999999");
        assert_eq!(parse("12.3", S2), Some(1230));
    }

    #[test]
    fn text_outside_the_column_is_refused() {
        for text in [
            "100000000.00",
            "1.001",
            "NaN",
            "1e5",
            "",
            "-",
            ".5",
            "1.2.3",
            "+1",
        ] {
            assert_eq!(parse(text, S2), None, "{text}");
        }
        assert_eq!(parse("1000000000000000000", S0), None);
        assert!(!fits(10_000_000_000, S2));
        assert!(fits(-9_999_999_999, S2));
    }

    #[test]
    fn sums_past_int8_format_exactly() {
        let sum = i128::from(i64::MAX) * 3 + 7;
        assert_eq!(format(sum, S2), "276701161105643274.28");
        assert_eq!(format(-sum, S0), "-27670116110564327428");
    }
}
