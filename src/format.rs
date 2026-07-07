// Faithful port of XRPN's Numeric#to_num (xlib/numeric). Produces the same
// display string the desktop shows for a given value and mode.
//
// Params:
//   i      decimals after the separator (FIX n)
//   thresh exponent threshold s (FIX 9, SCI/ENG 6); |exp| >= thresh -> exponent
//   g      exponent grouping (ENG = 3, else 1)
//   comma  true = comma decimal separator (European, XRPN default); false = dot
//   sep    true = group thousands (space when comma, comma when dot)

pub fn to_num(value: f64, i: i32, thresh: i32, g: i32, comma: bool, sep: bool) -> String {
    if value.is_nan() {
        return "Not a number".to_string();
    }
    if value.is_infinite() {
        return if value < 0.0 { "-Inf".to_string() } else { "Inf".to_string() };
    }
    // n = g if g > n  (threshold floored to grouping)
    let thresh = if g > thresh { g } else { thresh };
    let g = g.max(1);

    let e: i32 = if value != 0.0 {
        value.abs().log10().floor() as i32
    } else {
        0
    };
    // ge = g * (e / g) with Ruby's floor division.
    let ge = g * e.div_euclid(g);
    let mut x = value.abs();
    let exponent_mode = e.abs() >= thresh;
    if exponent_mode {
        x = x / 10f64.powi(ge);
    }

    let sign = if value < 0.0 { "-" } else { "" };

    // Integer part + fractional part rounded to i digits, with overflow carry.
    let idigits = i.max(0) as usize;
    let int_part = x.trunc();
    let frac = x - int_part;
    let scale = 10f64.powi(i.max(0));
    let f_val = (frac * scale).round() as i128;
    let mut int_str;
    let frac_str;
    if f_val >= scale as i128 {
        int_str = ((int_part as i128) + 1).to_string();
        frac_str = "0".repeat(idigits);
    } else {
        int_str = (int_part as i128).to_string();
        let mut s = f_val.to_string();
        if idigits > s.len() {
            s = format!("{}{}", "0".repeat(idigits - s.len()), s);
        }
        frac_str = s;
    }

    let mut out = String::new();
    if exponent_mode {
        out.push_str(&int_str);
        if i > 0 {
            out.push('.');
            out.push_str(&pad_right(&frac_str, i as usize));
        }
        out.push_str(" e");
        let mut gabs = ge;
        if ge < 0 {
            out.push('-');
            gabs = ge.abs();
        }
        out.push_str(&format!("{:02}", gabs));
    } else {
        if sep {
            let o = if comma { ' ' } else { ',' };
            int_str = group_thousands(&int_str, o);
        }
        out.push_str(&int_str);
        if i > 0 {
            out.push('.');
            out.push_str(&pad_right(&frac_str, i as usize));
        }
    }
    if comma {
        out = out.replacen('.', ",", 1);
    }
    format!("{}{}", sign, out)
}

fn pad_right(s: &str, width: usize) -> String {
    if s.len() >= width {
        s.to_string()
    } else {
        format!("{}{}", s, "0".repeat(width - s.len()))
    }
}

// Insert separator every 3 digits from the right of an integer-digit string.
fn group_thousands(digits: &str, sep: char) -> String {
    let bytes = digits.as_bytes();
    let n = bytes.len();
    let mut out = String::with_capacity(n + n / 3);
    for (idx, &b) in bytes.iter().enumerate() {
        if idx > 0 && (n - idx) % 3 == 0 {
            out.push(sep);
        }
        out.push(b as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // XRPN default mode: FIX 4, threshold 9, grouping 1, comma, separator on.
    fn def(v: f64) -> String {
        to_num(v, 4, 9, 1, true, true)
    }

    #[test]
    fn basic_fixed() {
        assert_eq!(def(8.0), "8,0000");
        assert_eq!(def(0.0), "0,0000");
        assert_eq!(def(-3.5), "-3,5000");
    }

    #[test]
    fn thousands_grouped_with_space_for_comma() {
        // comma decimal -> space thousands separator.
        assert_eq!(def(1234567.0), "1 234 567,0000");
    }

    #[test]
    fn dot_mode_groups_with_comma() {
        // dot decimal -> comma thousands separator.
        assert_eq!(to_num(1234567.0, 2, 9, 1, false, true), "1,234,567.00");
    }

    #[test]
    fn rounding_carry() {
        // 9.99996 at FIX 4 rounds to 10,0000 (carry into integer part).
        assert_eq!(def(9.99996), "10,0000");
    }

    #[test]
    fn exponent_mode_kicks_in() {
        // |exp| >= 9 -> exponent notation. 1e9 -> "1,0000 e09".
        let s = def(1e9);
        assert!(s.starts_with("1,0000 e"), "got {}", s);
        assert!(s.ends_with("09"), "got {}", s);
    }

    #[test]
    fn sci_mode_threshold_6() {
        // SCI: threshold 6. 1e6 -> exponent.
        let s = to_num(1e6, 4, 6, 1, true, true);
        assert!(s.contains(" e"), "got {}", s);
    }

    #[test]
    fn eng_mode_groups_exponent_by_3() {
        // ENG: g=3. 1e7 -> ge = 3*floor(7/3)=6, mantissa 10 -> "10,000 e06".
        let s = to_num(1e7, 3, 6, 3, true, true);
        assert!(s.ends_with(" e06"), "got {}", s);
        assert!(s.starts_with("10"), "got {}", s);
    }

    #[test]
    fn fix0_no_decimals() {
        assert_eq!(to_num(42.7, 0, 9, 1, true, true), "43");
    }
}
