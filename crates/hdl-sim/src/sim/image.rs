//! Rendering `t'image(x)` values as strings.
//!
//! Extracted from the `Expr::ToString` evaluation arm in `sim.rs` so the
//! spelling rules live in one testable place: real values print decimal,
//! booleans spell `true`/`false` (never the `-1`/`1`/`0` of the underlying
//! bit encoding), signed integers sign-extend from their own width, and
//! unknown stays visible as `x`.

use hdl_ir::value::Value;

/// Render a value the way `t'image` spells it. `real` selects the float
/// rendering; `boolean` marks a `boolean'image` operand (1-bit values spell
/// `true`/`false`, wider operands keep the decimal rendering — loose typing
/// can put a natural under a boolean prefix); `signed` sign-extends.
pub(super) fn format_image(v: &Value, real: bool, signed: bool, boolean: bool) -> String {
    if real {
        match v.to_f64() {
            Some(f) => format!("{}", f),
            None => "x".to_string(),
        }
    } else if boolean && v.width() == 1 {
        match v.to_u64() {
            Some(b) if b & 1 == 1 => "true".to_string(),
            Some(_) => "false".to_string(),
            None => "x".to_string(),
        }
    } else if signed {
        // Signed integer image: sign-extend from the value's own
        // width (`integer'image(-1)` is "-1", not 4294967295).
        match v.to_i64() {
            Some(n) => n.to_string(),
            None => "x".to_string(),
        }
    } else {
        match v.to_u64() {
            Some(n) => n.to_string(),
            None => "x".to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boolean_spelling_only_for_one_bit_values() {
        let t = Value::from_u64(1, 1);
        let f = Value::from_u64(1, 0);
        assert_eq!(format_image(&t, false, false, true), "true");
        assert_eq!(format_image(&f, false, false, true), "false");
        // Wider operand under a boolean prefix: decimal fallback.
        let wide = Value::from_u64(32, 0xFFFF_FFFF);
        assert_eq!(format_image(&wide, false, false, true), "4294967295");
    }

    #[test]
    fn signed_and_unknown_renderings() {
        let minus1 = Value::from_u64(4, 0xF);
        assert_eq!(format_image(&minus1, false, true, false), "-1");
        assert_eq!(format_image(&minus1, false, false, false), "15");
        assert_eq!(format_image(&Value::x(1), false, false, true), "x");
        let f = Value::from_f64(1.5);
        assert_eq!(format_image(&f, true, false, false), "1.5");
    }
}
