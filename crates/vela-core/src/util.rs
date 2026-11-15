//! Small number-formatting helpers that reproduce JavaScript's output, for
//! text the original builds with `toFixed` and `String(n)`.

/// `Number.prototype.toFixed`: a fixed number of decimals, ties rounded away
/// from zero (Rust's own formatting rounds ties to even).
pub fn to_fixed(value: f64, digits: usize) -> String {
    if !value.is_finite() {
        return js_number(value);
    }
    // The exact decimal expansion of the double, then a manual half-up round.
    let exact = format!("{:.1100}", value.abs());
    let (int_part, frac_part) = exact.split_once('.').unwrap_or((&exact, ""));
    let mut kept: Vec<u8> = int_part
        .bytes()
        .chain(frac_part.bytes().take(digits))
        .map(|b| b - b'0')
        .collect();
    let round_up = frac_part.as_bytes().get(digits).is_some_and(|b| *b >= b'5');
    if round_up {
        let mut i = kept.len();
        loop {
            if i == 0 {
                kept.insert(0, 1);
                break;
            }
            i -= 1;
            if kept[i] == 9 {
                kept[i] = 0;
            } else {
                kept[i] += 1;
                break;
            }
        }
    }
    let int_len = kept.len() - digits;
    let mut out = String::with_capacity(kept.len() + 2);
    // JavaScript keeps the sign of a negative value even when it rounds to zero.
    if value < 0.0 {
        out.push('-');
    }
    out.extend(kept[..int_len].iter().map(|d| (b'0' + d) as char));
    if digits > 0 {
        out.push('.');
        out.extend(kept[int_len..].iter().map(|d| (b'0' + d) as char));
    }
    out
}

/// `String(n)` for the values charts meet: integers print without a
/// fraction, other finite values in their shortest round-trip form.
/// (JavaScript switches to exponent notation below 1e-6 and from 1e21; this
/// does not.)
pub fn js_number(value: f64) -> String {
    crate::i18n::number_text(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn to_fixed_matches_javascript() {
        assert_eq!(to_fixed(1.005, 2), "1.00"); // 1.005 is just under the tie in binary
        assert_eq!(to_fixed(0.125, 2), "0.13"); // an exact tie rounds away from zero
        assert_eq!(to_fixed(2.5, 0), "3");
        assert_eq!(to_fixed(-2.5, 0), "-3");
        assert_eq!(to_fixed(-0.001, 2), "-0.00");
        assert_eq!(to_fixed(99.995, 2), "100.00");
        assert_eq!(to_fixed(1234.5678, 2), "1234.57");
        assert_eq!(to_fixed(0.0, 2), "0.00");
        assert_eq!(to_fixed(9.999, 2), "10.00");
        assert_eq!(to_fixed(f64::NAN, 2), "NaN");
    }

    #[test]
    fn js_number_prints_integers_bare() {
        assert_eq!(js_number(0.618), "0.618");
        assert_eq!(js_number(2.0), "2");
        assert_eq!(js_number(-1.5), "-1.5");
    }
}
