//! Local jq filtering for MCP tool responses.
//!
//! Filters are evaluated only inside the CLI process, after data has been read
//! from the collector and after optional local raw-value resolution. Nothing
//! about the jq program is sent to the Worker.

use anyhow::{anyhow, Context, Result};
use jaq_core::load::{Arena, File, Loader};
use jaq_core::{data, unwrap_valr, Compiler, Ctx, Vars};
use jaq_json::{read, write, Val};
use serde_json::Value;

pub fn apply(payload: Value, jq: Option<&str>) -> Result<Value> {
    let Some(program) = jq.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(payload);
    };

    let input_json = serde_json::to_vec(&payload).context("encoding MCP payload for jq")?;
    let input = read::parse_single(&input_json).context("parsing MCP payload for jq")?;
    let program = File {
        code: program,
        path: (),
    };

    let local_defs = jaq_core::load::parse(
        r#"
        def null: [][0];
        def keys: keys_unsorted | sort;
        def isboolean: . == true or . == false;
        def isnumber: . > true and . < "";
        def max: reduce max_by_or_empty(.) as $x (null; $x);
        "#,
        |p| p.defs(),
    )
    .expect("valid jq defs");
    let defs = jaq_core::defs().chain(local_defs).chain(jaq_json::defs());
    let funs = jaq_core::funs()
        .chain(jaq_std::base_funs())
        .chain(jaq_json::funs());
    let loader = Loader::new(defs);
    let arena = Arena::default();
    let modules = loader
        .load(&arena, program)
        .map_err(|e| anyhow!("jq parse failed: {e:?}"))?;
    let filter = Compiler::default()
        .with_funs(funs)
        .compile(modules)
        .map_err(|e| anyhow!("jq compile failed: {e:?}"))?;

    let ctx = Ctx::<data::JustLut<Val>>::new(&filter.lut, Vars::new([]));
    let mut out = Vec::new();
    for value in filter.id.run((ctx, input)).map(unwrap_valr) {
        let value = value.map_err(|e| anyhow!("jq execution failed: {e}"))?;
        out.push(val_to_json(&value)?);
    }

    Ok(match out.len() {
        0 => Value::Array(Vec::new()),
        1 => out.remove(0),
        _ => Value::Array(out),
    })
}

fn val_to_json(value: &Val) -> Result<Value> {
    let mut bytes = Vec::new();
    write::write(&mut bytes, &write::Pp::default(), 0, value)
        .context("encoding jq output as JSON")?;
    crate::json_fidelity::parse_bytes(&bytes).context("parsing jq JSON output")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn filters_payload() {
        let out = apply(
            json!([
                {"trace_id": "a", "span_count": 1},
                {"trace_id": "b", "span_count": 2}
            ]),
            Some("map({id: .trace_id})"),
        )
        .unwrap();

        assert_eq!(out, json!([{"id": "a"}, {"id": "b"}]));
    }

    #[test]
    fn filters_preserve_literal_transport_marker_objects() {
        let payload = json!({"nested": [
            {"$serde_json::private::Number": "ordinary data"},
            {"$serde_json::private::RawValue": "ordinary data"}
        ]});
        assert_eq!(apply(payload.clone(), Some(".")).unwrap(), payload);
    }

    #[test]
    fn multi_output_becomes_array() {
        let out = apply(
            json!([{"trace_id": "a"}, {"trace_id": "b"}]),
            Some(".[].trace_id"),
        )
        .unwrap();

        assert_eq!(out, json!(["a", "b"]));
    }

    #[test]
    fn keys_filter_is_available() {
        let out = apply(json!({"b": 2, "a": 1}), Some("keys")).unwrap();

        assert_eq!(out, json!(["a", "b"]));
    }

    #[test]
    fn environment_access_is_not_available() {
        let err = apply(json!({}), Some("env")).unwrap_err().to_string();

        assert!(err.contains("jq compile failed"));
    }
}
