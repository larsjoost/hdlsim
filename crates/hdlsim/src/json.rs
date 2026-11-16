//! JSON values for JSON-RPC messages, backed by `serde_json::Value`.
//!
//! The `arbitrary_precision` feature preserves full number precision, so integer
//! request ids round-trip exactly (exponent notation may be canonicalized, e.g.
//! `-2.5e2` prints as `-2.5e+2`). [`JsonCtor`] provides the `Json::str`/`num`/
//! `obj`/`parse` constructors so call sites keep their shape.

pub use serde_json::Value as Json;

/// Constructors and parser for [`Json`], mirroring the previous hand-rolled API.
pub trait JsonCtor {
    fn str(s: impl Into<String>) -> Json;
    fn num(n: impl serde::Serialize) -> Json;
    fn obj(pairs: Vec<(&str, Json)>) -> Json;
    fn parse(text: &str) -> Result<Json, String>;
}

impl JsonCtor for Json {
    fn str(s: impl Into<String>) -> Json {
        Json::String(s.into())
    }

    fn num(n: impl serde::Serialize) -> Json {
        serde_json::json!(n)
    }

    /// Build an object from key/value pairs. Keys are stored sorted
    /// (`serde_json::Map` is a `BTreeMap` without `preserve_order`), which keeps
    /// serialized output deterministic.
    fn obj(pairs: Vec<(&str, Json)>) -> Json {
        Json::Object(pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
    }

    /// Parse one JSON value; trailing garbage is an error.
    fn parse(text: &str) -> Result<Json, String> {
        serde_json::from_str(text).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_nested() {
        let src = r#"{"a":[1,-2.5e2,true,null,"x\ny"],"b":{"c":[]}}"#;
        let v = Json::parse(src).unwrap();
        assert_eq!(
            v.get("a").unwrap().as_array().unwrap()[1].as_f64(),
            Some(-250.0)
        );
        assert_eq!(v.get("b").unwrap().get("c"), Some(&Json::Array(vec![])));
        let out = v.to_string();
        assert_eq!(Json::parse(&out).unwrap(), v);
    }

    #[test]
    fn string_escapes_and_unicode() {
        let v = Json::parse(r#""q\\\"\\\\\\nAé""#).unwrap();
        assert_eq!(v.as_str(), Some("q\\\"\\\\\\nA\u{e9}"));
        let out = v.to_string();
        assert_eq!(Json::parse(&out).unwrap(), v);
        // \u escape + surrogate pair
        let v = Json::parse(r#""\u0041\ud83d\ude00""#).unwrap();
        assert_eq!(v.as_str(), Some("A\u{1f600}"));
    }

    #[test]
    fn rejects_garbage() {
        assert!(Json::parse("{").is_err());
        assert!(Json::parse("[1,").is_err());
        assert!(Json::parse("{} x").is_err());
        assert!(Json::parse("\"unterminated").is_err());
        assert!(Json::parse("nope").is_err());
    }

    #[test]
    fn integer_ids_round_trip_exactly() {
        let v = Json::parse("{\"id\": 42}").unwrap();
        assert_eq!(v.get("id").unwrap().as_i64(), Some(42));
        assert_eq!(v.get("id").unwrap().to_string(), "42");
        // Beyond f64 precision: arbitrary_precision keeps every digit.
        let big = "12345678901234567890123";
        let v = Json::parse(big).unwrap();
        assert_eq!(v.to_string(), big);
        // Exponent notation is canonicalized but the value is exact.
        let v = Json::parse("-2.5e2").unwrap();
        assert_eq!(v.as_f64(), Some(-250.0));
    }

    #[test]
    fn constructors() {
        let v = Json::obj(vec![
            ("jsonrpc", Json::str("2.0")),
            ("id", Json::num(42)),
            ("ok", Json::Bool(true)),
        ]);
        assert_eq!(v.get("id").unwrap().as_i64(), Some(42));
        assert_eq!(v.to_string(), r#"{"id":42,"jsonrpc":"2.0","ok":true}"#);
    }
}
