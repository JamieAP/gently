//! Parse arbitrary JSON without rounding numbers or treating user keys as
//! serde_json's internal number/raw-value transport markers.
use anyhow::Result;
use serde_json::{value::RawValue, Value};
use std::collections::BTreeMap;

pub fn parse(input: &str) -> Result<Value> {
    let raw: &RawValue = serde_json::from_str(input)?;
    decode(raw, 128)
}

pub fn parse_bytes(input: &[u8]) -> Result<Value> {
    parse(std::str::from_utf8(input)?)
}

fn decode(raw: &RawValue, remaining: usize) -> Result<Value> {
    let input = raw.get();
    match input.as_bytes().first() {
        Some(b'{' | b'[') if remaining == 0 => {
            anyhow::bail!("JSON recursion limit exceeded")
        }
        Some(b'{') => {
            // Typed maps preserve every user key literally. Value's generic
            // visitor reserves private keys when arbitrary precision is on.
            // Each subtree is reparsed, so work grows with input size and depth;
            // the depth limit bounds this tradeoff for ordinary hook payloads.
            let members: BTreeMap<String, &RawValue> = serde_json::from_str(input)?;
            let object = members
                .into_iter()
                .map(|(key, child)| Ok((key, decode(child, remaining - 1)?)))
                .collect::<Result<serde_json::Map<String, Value>>>()?;
            Ok(Value::Object(object))
        }
        Some(b'[') => {
            let children: Vec<&RawValue> = serde_json::from_str(input)?;
            Ok(Value::Array(
                children
                    .into_iter()
                    .map(|child| decode(child, remaining - 1))
                    .collect::<Result<Vec<_>>>()?,
            ))
        }
        _ => Ok(serde_json::from_str(input)?),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transport_marker_keys_remain_literal_objects_at_every_depth() {
        let value = parse(
            r#"{"nested":[{"$serde_json::private::Number":"literal","other":true},{"$serde_json::private::RawValue":"literal"}]}"#,
        )
        .unwrap();
        assert_eq!(
            value["nested"][0]["$serde_json::private::Number"],
            "literal"
        );
        assert_eq!(value["nested"][0]["other"], true);
        assert_eq!(
            value["nested"][1]["$serde_json::private::RawValue"],
            "literal"
        );
    }

    #[test]
    fn malformed_trailing_and_excessively_nested_json_are_rejected() {
        for input in ["{", "{\"number\":01}", "[true] false", "{\"x\":NaN}"] {
            assert!(parse(input).is_err(), "{input}");
        }
        let nested = format!("{}null{}", "[".repeat(256), "]".repeat(256));
        assert!(parse(&nested).is_err());
    }
}
