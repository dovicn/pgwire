use crate::error::PgWireError;
use smol_str::{SmolStr, ToSmolStr, format_smolstr};

/// extra_float_digits sets the number of digits displayed for floating-point
/// values.
///
/// Valid values [-15, 3], default value: 1
#[derive(Debug)]
pub struct ExtraFloatDigits(pub i8);

impl TryFrom<&str> for ExtraFloatDigits {
    type Error = PgWireError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        let value = value
            .parse::<i8>()
            .map_err(|_| PgWireError::InvalidOptionValue(value.to_string()))?;

        if !(-15..=3).contains(&value) {
            Err(PgWireError::InvalidOptionValue(value.to_string()))
        } else {
            Ok(Self(value))
        }
    }
}

impl ExtraFloatDigits {
    const FLT_DIG: i8 = 6;
    const DBL_DIG: i8 = 15;

    // Tessera patch: format as PostgreSQL's float8out / float4out do (see
    // third_party/pgwire/TESSERA.md). extra_float_digits > 0 (what JDBC, Npgsql
    // and tokio-postgres ask for): the shortest text that reads back as the same
    // value, `5` not ryu's `5.0`, `1e+20` not `1e20`. <= 0: `%.{DIG+extra}g`, so
    // large and small values use an exponent too. `Infinity`, `-Infinity`, `NaN`,
    // `-0`, as Postgres writes them.

    /// Format an f64 value according to the configured precision.
    pub fn format_f64(&self, value: f64) -> SmolStr {
        let extra = self.0.clamp(-15, 3);
        if let Some(s) = special(value) {
            return s;
        }
        if extra >= 1 {
            let mut buffer = ryu::Buffer::new();
            shortest(buffer.format_finite(value), Self::DBL_DIG as i32)
        } else {
            format_g(value, (Self::DBL_DIG + extra).max(1) as usize)
        }
    }

    /// Format an f32 value according to the configured precision.
    pub fn format_f32(&self, value: f32) -> SmolStr {
        let extra = self.0.clamp(-15, 3);
        if let Some(s) = special(value as f64) {
            return s;
        }
        if extra >= 1 {
            let mut buffer = ryu::Buffer::new();
            shortest(buffer.format_finite(value), Self::FLT_DIG as i32)
        } else {
            format_g(value as f64, (Self::FLT_DIG + extra).max(1) as usize)
        }
    }
}

/// NaN, infinities and zeros, as Postgres writes them.
fn special(value: f64) -> Option<SmolStr> {
    if value.is_nan() {
        Some("NaN".to_smolstr())
    } else if value.is_infinite() {
        Some(if value > 0.0 { "Infinity" } else { "-Infinity" }.to_smolstr())
    } else if value == 0.0 {
        Some(if value.is_sign_negative() { "-0" } else { "0" }.to_smolstr())
    } else {
        None
    }
}

/// A number's significant digits (no leading or trailing zeros) and the decimal
/// exponent of its first digit, from ryu's or `{:e}`'s text.
fn digits_and_exponent(text: &str) -> (bool, String, i32) {
    let (negative, text) = match text.strip_prefix('-') {
        Some(t) => (true, t),
        None => (false, text),
    };
    let (mantissa, exp) = match text.split_once(['e', 'E']) {
        Some((m, e)) => (m, e.parse::<i32>().unwrap_or(0)),
        None => (text, 0),
    };
    let (int, frac) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    let all: String = format!("{int}{frac}");
    let leading = all.len() - all.trim_start_matches('0').len();
    let digits = all.trim_start_matches('0').trim_end_matches('0').to_string();
    // the first digit's position: int has int.len() digits before the point
    let exponent = exp + int.len() as i32 - 1 - leading as i32;
    (negative, if digits.is_empty() { "0".into() } else { digits }, exponent)
}

/// `digits` x 10^exponent (first digit) in Postgres's layout: plain when
/// -4 <= exponent < `max`, else `d.ddde+XX` (exponent at least two digits).
fn layout(negative: bool, digits: &str, exponent: i32, max: i32) -> SmolStr {
    let sign = if negative { "-" } else { "" };
    if exponent < -4 || exponent >= max {
        let (first, rest) = digits.split_at(1);
        let rest = if rest.is_empty() { String::new() } else { format!(".{rest}") };
        let esign = if exponent < 0 { '-' } else { '+' };
        return format_smolstr!("{sign}{first}{rest}e{esign}{:02}", exponent.abs());
    }
    if exponent < 0 {
        return format_smolstr!("{sign}0.{}{digits}", "0".repeat((-exponent - 1) as usize));
    }
    let int_len = exponent as usize + 1;
    if digits.len() <= int_len {
        format_smolstr!("{sign}{digits}{}", "0".repeat(int_len - digits.len()))
    } else {
        format_smolstr!("{sign}{}.{}", &digits[..int_len], &digits[int_len..])
    }
}

/// The shortest round-trip text (ryu's digits), in Postgres's layout: plain up to
/// `dig` integer digits, else an exponent (Postgres 12+ float8out with
/// extra_float_digits > 0).
fn shortest(ryu_text: &str, dig: i32) -> SmolStr {
    let (negative, digits, exponent) = digits_and_exponent(ryu_text);
    layout(negative, &digits, exponent, dig)
}

/// C's `%.{precision}g`.
fn format_g(value: f64, precision: usize) -> SmolStr {
    let text = format!("{:.*e}", precision - 1, value);
    let (negative, digits, exponent) = digits_and_exponent(&text);
    layout(negative, &digits, exponent, precision as i32)
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format() {
        let v = std::f64::consts::PI; //3.1415926535897931

        let formatted = &[
            "3",                 // -15
            "3",                 // -14
            "3.1",               // -13
            "3.14",              // -12
            "3.142",             // -11
            "3.1416",            // -10
            "3.14159",           // -9
            "3.141593",          // -8
            "3.1415927",         // -7
            "3.14159265",        // -6
            "3.141592654",       // -5
            "3.1415926536",      // -4
            "3.14159265359",     // -3
            "3.14159265359",     // -2
            "3.1415926535898",   // -1
            "3.14159265358979",  // 0
            "3.141592653589793", // 1
            "3.141592653589793", // 2
            "3.141592653589793", // 3
        ];

        for (i, e) in (-15..=3).enumerate() {
            let extra_float_digit = ExtraFloatDigits(e);
            assert_eq!(extra_float_digit.format_f64(v), formatted[i]);
        }

        assert_eq!(ExtraFloatDigits(0).format_f64(f64::NAN), "NaN");
        assert_eq!(ExtraFloatDigits(0).format_f64(f64::INFINITY), "Infinity");
        assert_eq!(ExtraFloatDigits(0).format_f64(-f64::INFINITY), "-Infinity");

        let v = std::f32::consts::PI; //3.1415926535897931

        let formatted = &[
            "3",         // -15
            "3",         // -14
            "3",         // -13
            "3",         // -12
            "3",         // -11
            "3",         // -10
            "3",         // -9
            "3",         // -8
            "3",         // -7
            "3",         // -6
            "3",         // -5
            "3.1",       // -4
            "3.14",      // -3
            "3.142",     // -2
            "3.1416",    // -1
            "3.14159",   // 0
            "3.1415927", // 1
            "3.1415927", // 2
            "3.1415927", // 3
        ];

        for (i, e) in (-15..=3).enumerate() {
            let extra_float_digit = ExtraFloatDigits(e);
            assert_eq!(extra_float_digit.format_f32(v), formatted[i]);
        }
    }

    /// Tessera patch: PostgreSQL's float8out / float4out text.
    #[test]
    fn postgres_text() {
        let shortest = ExtraFloatDigits(1);
        for (v, want) in [
            (5.0, "5"),
            (0.1, "0.1"),
            (1.5, "1.5"),
            (-2.5, "-2.5"),
            (1e20, "1e+20"),
            (1e15, "1e+15"),
            (1e14, "100000000000000"),
            (123456789012345.0, "123456789012345"),
            (1234567890123456.0, "1.234567890123456e+15"),
            (0.0001, "0.0001"),
            (0.00001, "1e-05"),
            (1.23e-7, "1.23e-07"),
            (1e300, "1e+300"),
            (5e-324, "5e-324"),
            (f64::MAX, "1.7976931348623157e+308"),
            (0.0, "0"),
            (-0.0, "-0"),
            (f64::NEG_INFINITY, "-Infinity"),
            (100.0, "100"),
            (1234.5678, "1234.5678"),
        ] {
            assert_eq!(shortest.format_f64(v), want, "{v:e}");
            assert_eq!(ExtraFloatDigits(3).format_f64(v), want, "{v:e}");
        }
        for (v, want) in [(5.0f32, "5"), (0.1, "0.1"), (100000.0, "100000"), (1e6, "1e+06"), (1.5e-5, "1.5e-05")] {
            assert_eq!(shortest.format_f32(v), want, "{v:e}");
        }
        // %.15g
        let g = ExtraFloatDigits(0);
        for (v, want) in [(5.0, "5"), (0.1, "0.1"), (1e20, "1e+20"), (123456789012345678.0, "1.23456789012346e+17"), (0.00001, "1e-05"), (2.0 / 3.0, "0.666666666666667")] {
            assert_eq!(g.format_f64(v), want, "{v:e}");
        }
    }
}
