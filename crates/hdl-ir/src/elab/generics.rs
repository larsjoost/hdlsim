//! Top-generic overrides (`-g NAME=VALUE`): parse the CLI specs against the
//! top's declared generics and re-lower the top through the resolver's
//! variant machinery, so the values reach the architecture's declarations.

use super::*;

/// Apply `-g name=value` overrides to the top: parse each value against the
/// top's declared generics (the generic's own value width sizes the result),
/// then re-lower the top so the overrides reach the architecture's
/// declarations. Returns the (possibly variant-suffixed) top name to
/// elaborate.
pub(super) fn apply_top_generics(
    design: &mut Design,
    top: &str,
    resolver: &mut dyn UnitResolver,
    specs: &[(String, String)],
) -> Result<String, ElabError> {
    if specs.is_empty() {
        return Ok(top.to_string());
    }
    let Some(pos) = find_instance_target(&design.modules, top) else {
        return Err(ElabError::Message(format!(
            "generic overrides need an entity top, but '{top}' is not a compiled module"
        )));
    };
    let params = parse_top_generics(&design.modules[pos], top, specs)?;
    let entity = design.modules[pos].name.clone();
    match resolver.resolve_variant(design, &entity, &params, pos) {
        Ok(Some(vname)) => Ok(vname),
        Ok(None) => Err(ElabError::Message(format!(
            "cannot apply generic overrides to '{top}': its sources are not available for re-lowering"
        ))),
        Err(e) => Err(ElabError::Message(e)),
    }
}

/// Parse the `-g` specs against one module's declared generics: the value
/// width comes from the generic's default (an unbound generic's default is
/// X of the type's width), and the key is lowercased —
/// `lower_generic_decls` looks overrides up by the lowercased declared
/// name (same convention as the generic-map variant path).
fn parse_top_generics(
    m: &ModuleIr,
    top: &str,
    specs: &[(String, String)],
) -> Result<HashMap<String, Value>, ElabError> {
    let mut params: HashMap<String, Value> = HashMap::new();
    for (name, raw) in specs {
        let Some(p) = m.params.iter().find(|p| p.name.eq_ignore_ascii_case(name)) else {
            let declared: Vec<String> = m.params.iter().map(|p| p.name.clone()).collect();
            let declared = if declared.is_empty() {
                "none".to_string()
            } else {
                declared.join(", ")
            };
            return Err(ElabError::Message(format!(
                "unknown generic '{name}' for top '{top}' (declared: {declared})"
            )));
        };
        let width = p.default.bits.len().max(1);
        let v = parse_generic_value(raw, width, name)?;
        params.insert(p.name.to_ascii_lowercase(), v);
    }
    Ok(params)
}

/// Parse one `-g` value into a `Value` of `width` bits. Accepted forms:
/// signed decimal integer (two's-complement, truncated to the generic's
/// width), VHDL based literal (`16#20#`, `2#1010#`), `true`/`false` for
/// boolean generics, and a single bit state (`'0'`, `'1'`, `'x'`, `'z'`).
/// Anything else is a hard error naming the generic — a typo'd knob must
/// not fail silently.
fn parse_generic_value(raw: &str, width: usize, name: &str) -> Result<Value, ElabError> {
    let t = raw.trim();
    if let Some(b) = bit_state(t) {
        return Ok(Value::from_bit(b).resize(width));
    }
    match t.to_ascii_lowercase().as_str() {
        "true" => return Ok(Value::from_u64(width, 1)),
        "false" => return Ok(Value::from_u64(width, 0)),
        _ => {}
    }
    if let Some(v) = parse_int_literal(t) {
        return Ok(Value::from_u64(width, v));
    }
    Err(ElabError::Message(format!(
        "cannot parse value '{raw}' for generic '{name}' (expected a decimal or based-literal \
         integer like 16#20#, true/false, or a bit state like '1')"
    )))
}

/// A single-quoted bit state letter → its `Value` bit constant.
fn bit_state(t: &str) -> Option<u8> {
    let b = t.strip_prefix('\'')?.strip_suffix('\'')?;
    let mut it = b.chars();
    let c = it.next()?.to_ascii_lowercase();
    if it.next().is_some() {
        return None;
    }
    Some(match c {
        '0' => V0,
        '1' => V1,
        'x' => VX,
        'z' => VZ,
        'u' => VU,
        'w' => VW,
        'l' => VL,
        'h' => VH,
        '-' => VDASH,
        _ => return None,
    })
}

/// Signed decimal integer or VHDL based literal (`base#digits[#]`, base
/// 2..=16, `_` separators allowed) → the bit pattern as `u64` (two's
/// complement for negatives; later truncated to the generic's width).
fn parse_int_literal(t: &str) -> Option<u64> {
    let (neg, rest) = match t.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, t.strip_prefix('+').unwrap_or(t)),
    };
    if rest.is_empty() {
        return None;
    }
    let v: Option<i128> = if let Some((base, digits)) = rest.split_once('#') {
        let digits = digits.strip_suffix('#').unwrap_or(digits).replace('_', "");
        let base = base.parse::<u32>().ok()?;
        if !(2..=16).contains(&base) || digits.is_empty() {
            return None;
        }
        if !digits.chars().all(|c| c.is_ascii_hexdigit()) {
            return None;
        }
        i128::from_str_radix(&digits, base).ok()
    } else {
        let digits = rest.replace('_', "");
        if !digits.chars().all(|c| c.is_ascii_digit()) {
            return None;
        }
        digits.parse::<i128>().ok()
    };
    let v = if neg { v?.wrapping_neg() } else { v? };
    Some(v as u64)
}

/// `-g` value parsing: signed decimal, based literals with `_` separators,
/// booleans, bit states, and the rejection cases.
#[cfg(test)]
mod tests {
    use super::*;

    fn parse(raw: &str, w: usize) -> Option<Value> {
        parse_generic_value(raw, w, "G").ok()
    }

    #[test]
    fn accepted_forms_parse_to_the_expected_bit_patterns() {
        assert_eq!(parse("64", 32).map(|v| v.to_u64()), Some(Some(64)));
        // Negative: two's-complement bit pattern, truncated to width.
        assert_eq!(parse("-1", 4).map(|v| v.to_u64()), Some(Some(0xF)));
        assert_eq!(parse("16#40#", 32).map(|v| v.to_u64()), Some(Some(64)));
        assert_eq!(
            parse("2#1010_1010#", 32).map(|v| v.to_u64()),
            Some(Some(0xAA))
        );
        assert_eq!(parse("true", 1).map(|v| v.to_u64()), Some(Some(1)));
        assert_eq!(parse("false", 1).map(|v| v.to_u64()), Some(Some(0)));
        assert_eq!(parse("'1'", 1).map(|v| v.to_u64()), Some(Some(1)));
    }

    #[test]
    fn garbage_and_bad_bases_are_rejected() {
        assert!(parse("abc", 32).is_none());
        assert!(parse("17#20#", 32).is_none());
        assert!(parse("''", 1).is_none());
        assert!(parse("'01'", 1).is_none());
    }
}
