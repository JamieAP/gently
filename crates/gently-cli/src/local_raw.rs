//! Encrypt raw fields before persistence; decrypt only at the explicit reader.

use crate::{config::Config, query_client::SpanRow};
use anyhow::{ensure, Context, Result};
use gently_core::Span;
use gently_harness::{Attrs, Parsed, SpanOp};
#[cfg(test)]
use gently_raw::ReaderIdentities;
use gently_raw::{RawContext, RawObject, RawPayload, SignedManifest, TrustPin, VerifiedManifest};
use gently_store::Store;
use serde::de::DeserializeOwned;
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Read,
    path::Path,
};

const RAW_FIELDS: &[&str] = &[
    "gently.prompt",
    "gently.assistant",
    "gently.tool_input",
    "gently.tool_response",
    "gently.reason",
    "gently.compact_instructions",
    "gently.compact_summary",
    "gently.tool_calls",
    "gently.error",
    "gently.error_details",
    "gently.hook_payload",
    "gently.message.delta",
    "gently.instruction_file",
];

/// Inspect only authenticated public policy. This never opens a reader identity.
pub fn policy_health(cfg: &Config) -> (&'static str, Option<u64>) {
    if !cfg.capture_raw_values {
        return ("disabled", None);
    }
    let inspect = || -> Result<u64> {
        let signed: SignedManifest =
            read_public_json(cfg.raw_manifest.as_deref().context("missing policy")?)?;
        let pin: TrustPin = read_public_json(cfg.raw_trust.as_deref().context("missing trust")?)?;
        let verified = VerifiedManifest::verify_at(&signed, &pin, 0)?;
        ensure!(
            verified.manifest().tenant_id == cfg.tenant_id,
            "wrong tenant"
        );
        Ok(verified.manifest().expires_unix_secs)
    };
    match inspect() {
        Err(_) => ("unavailable", None),
        Ok(expires) => {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|v| v.as_secs())
                .unwrap_or(u64::MAX);
            let state = if expires <= now {
                "expired"
            } else if expires.saturating_sub(now) <= 7 * 24 * 3600 {
                "expiring_soon"
            } else {
                "valid"
            };
            (state, Some(expires))
        }
    }
}

/// In-memory raw material plus authenticated public recipient policy. Capture
/// never opens an identity file or invokes a private-key provider.
pub struct PreparedRaw {
    manifest: VerifiedManifest,
    context: RawContext,
    fields: BTreeMap<String, String>,
}

/// Clear content fingerprints and references created by a failed raw attempt
/// before committing its metadata-only replacement.
pub fn metadata_only(parsed: &mut Parsed) {
    for op in &mut parsed.ops {
        op_attrs_mut(op).retain(|(key, _)| !key.ends_with(".sha256") && !key.ends_with(".raw_ref"));
    }
}

/// Remove unkeyed content hashes unconditionally, then attach opaque refs only
/// when trusted recipient policy permits encryption. Failed capture preserves
/// length-only telemetry; it never falls back to plaintext or content hashes.
pub fn prepare(
    cfg: &Config,
    raw: &Value,
    parsed: &mut Parsed,
    harness: &str,
) -> Result<Option<PreparedRaw>> {
    metadata_only(parsed);
    if !cfg.capture_raw_values {
        return Ok(None);
    }
    let mut fields = BTreeMap::from([("gently.hook_payload".into(), serde_json::to_string(raw)?)]);
    for op in &parsed.ops {
        for (key, _) in op_attrs(op) {
            if let Some(base) = key.strip_suffix(".bytes") {
                if let Some(value) = extract_field(raw, base)? {
                    fields.insert(base.to_string(), value);
                }
            }
        }
    }
    if fields.is_empty() {
        return Ok(None);
    }
    let signed: SignedManifest = read_public_json(
        cfg.raw_manifest
            .as_deref()
            .context("raw capture requires a recipient manifest")?,
    )?;
    let pin: TrustPin = read_public_json(
        cfg.raw_trust
            .as_deref()
            .context("raw capture requires a local trust pin")?,
    )?;
    let manifest = VerifiedManifest::verify(&signed, &pin)?;
    ensure!(
        manifest.manifest().tenant_id == cfg.tenant_id,
        "recipient manifest belongs to another tenant"
    );
    let context = RawContext {
        tenant_id: cfg.tenant_id.clone(),
        device_id: cfg.device_id.clone(),
        key_epoch: manifest.manifest().key_epoch,
        raw_ref: gently_raw::new_raw_ref(),
        session_id: parsed.session_id.clone(),
        harness: harness.into(),
        event: raw
            .get("hook_event_name")
            .and_then(Value::as_str)
            .context("raw event is missing")?
            .into(),
    };
    for op in &mut parsed.ops {
        let attrs = op_attrs_mut(op);
        for base in fields.keys() {
            if attrs.iter().any(|(key, _)| key == &format!("{base}.bytes")) {
                attrs.push((format!("{base}.raw_ref"), context.raw_ref.clone()));
            }
        }
    }
    Ok(Some(PreparedRaw {
        manifest,
        context,
        fields,
    }))
}

impl PreparedRaw {
    pub fn reference(&self) -> &str {
        &self.context.raw_ref
    }
    /// Run inside the event transaction, after apply, so field bindings cover
    /// both emitted spans and pending tools whose close arrives in a later hook.
    pub fn seal_for_spans(self, store: &Store, spans: &[Span]) -> Result<RawObject> {
        let mut bindings: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for span in spans {
            collect_bindings(
                &mut bindings,
                &span.span_id.to_hex(),
                &span.attributes,
                &self.context.raw_ref,
            );
        }
        for (span_id, attrs_json) in
            store.open_span_attributes_with_reference(&self.context.raw_ref)?
        {
            let attrs: Attrs =
                serde_json::from_str(&attrs_json).context("invalid pending span attributes")?;
            collect_bindings(&mut bindings, &span_id, &attrs, &self.context.raw_ref);
        }
        for ids in bindings.values_mut() {
            ids.sort();
            ids.dedup();
        }
        ensure!(
            self.fields.keys().all(|field| bindings.contains_key(field)),
            "raw field has no owning span"
        );
        Ok(gently_raw::seal(
            &self.manifest,
            self.context,
            self.fields,
            bindings,
        )?)
    }
}

fn collect_bindings(
    bindings: &mut BTreeMap<String, Vec<String>>,
    span_id: &str,
    attrs: &Attrs,
    reference: &str,
) {
    for (key, value) in attrs {
        if value == reference {
            if let Some(base) = key
                .strip_suffix(".raw_ref")
                .filter(|base| RAW_FIELDS.contains(base))
            {
                bindings
                    .entry(base.into())
                    .or_default()
                    .push(span_id.into());
            }
        }
    }
}

fn read_public_json<T: DeserializeOwned>(path: &Path) -> Result<T> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(64 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= 64 * 1024,
        "public recipient policy is too large"
    );
    serde_json::from_slice(&bytes).context("invalid public recipient policy")
}

fn extract_field(raw: &Value, base: &str) -> Result<Option<String>> {
    let strings: &[&str] = match base {
        "gently.prompt" => &["prompt", "user_prompt", "user"],
        "gently.assistant" => &["last_assistant_message", "assistant", "assistant_message"],
        "gently.reason" => &["reason"],
        "gently.message.delta" => &["delta"],
        "gently.instruction_file" => &["file_path"],
        "gently.compact_instructions" => &["custom_instructions"],
        "gently.compact_summary" => &["compact_summary"],
        "gently.error" => &["error"],
        _ => &[],
    };
    for field in strings {
        if let Some(value) = raw.get(*field).and_then(Value::as_str) {
            return Ok(Some(value.into()));
        }
    }
    let json_field = match base {
        "gently.tool_input" => "tool_input",
        "gently.tool_response" => "tool_response",
        "gently.tool_calls" => "tool_calls",
        "gently.error_details" => "error_details",
        _ => return Ok(None),
    };
    raw.get(json_field)
        .map(serde_json::to_string)
        .transpose()
        .map_err(Into::into)
}

fn op_attrs(op: &SpanOp) -> &Attrs {
    match op {
        SpanOp::OpenSession { attrs }
        | SpanOp::CloseSession { attrs, .. }
        | SpanOp::OpenTurn { attrs }
        | SpanOp::CloseTurn { attrs, .. }
        | SpanOp::OpenTool { attrs, .. }
        | SpanOp::CloseTool { attrs, .. }
        | SpanOp::OpenAgent { attrs, .. }
        | SpanOp::CloseAgent { attrs, .. }
        | SpanOp::Mark { attrs, .. }
        | SpanOp::MarkContext { attrs, .. } => attrs,
    }
}
fn op_attrs_mut(op: &mut SpanOp) -> &mut Attrs {
    match op {
        SpanOp::OpenSession { attrs }
        | SpanOp::CloseSession { attrs, .. }
        | SpanOp::OpenTurn { attrs }
        | SpanOp::CloseTurn { attrs, .. }
        | SpanOp::OpenTool { attrs, .. }
        | SpanOp::CloseTool { attrs, .. }
        | SpanOp::OpenAgent { attrs, .. }
        | SpanOp::CloseAgent { attrs, .. }
        | SpanOp::Mark { attrs, .. }
        | SpanOp::MarkContext { attrs, .. } => attrs,
    }
}

/// Unique raw refs needed to hydrate a collector row. Field names and reference
/// framing are checked before either a local lookup or cloud request.
pub fn row_references(row: &SpanRow) -> Result<Vec<String>> {
    let Some(blob) = row.attrs_json.as_deref() else {
        return Ok(Vec::new());
    };
    let attrs = crate::json_fidelity::parse(blob).context("invalid span attributes")?;
    let mut refs = BTreeSet::new();
    for attr in attrs.as_array().context("invalid span attribute shape")? {
        let Some(base) = attr
            .get("key")
            .and_then(Value::as_str)
            .and_then(|key| key.strip_suffix(".raw_ref"))
        else {
            continue;
        };
        ensure!(RAW_FIELDS.contains(&base), "unknown raw field reference");
        let reference = attr
            .get("value")
            .and_then(|v| v.get("stringValue"))
            .and_then(Value::as_str)
            .context("invalid raw reference")?;
        ensure!(
            reference.len() == 32
                && reference
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
            "invalid raw reference"
        );
        refs.insert(reference.to_string());
    }
    Ok(refs.into_iter().collect())
}

/// Authenticate the object context and its field-to-span binding before adding
/// plaintext to this in-memory result. The collector never receives this row.
#[cfg(test)]
fn resolve_row(
    tenant_id: &str,
    row: &mut SpanRow,
    object: &RawObject,
    identities: &ReaderIdentities,
) -> Result<()> {
    validate_row_context(tenant_id, row, &object.context)?;
    let payload = gently_raw::open(object, &object.context, identities)?;
    resolve_payload(tenant_id, row, &payload)
}

/// Check each row independently before either unlocking or using a cached
/// authenticated payload. Its enclosing object is never trusted as row context.
pub(crate) fn validate_row_context(
    tenant_id: &str,
    row: &SpanRow,
    context: &RawContext,
) -> Result<()> {
    ensure!(
        row_references(row)?.contains(&context.raw_ref),
        "raw reference does not belong to this row"
    );
    ensure!(
        row.resource_attr("gently.tenant_id").as_deref() == Some(tenant_id),
        "raw row tenant mismatch"
    );
    let mut expected = context.clone();
    expected.tenant_id = tenant_id.into();
    expected.session_id = row
        .session_id
        .clone()
        .context("raw row session is missing")?;
    expected.harness = row.harness.clone().context("raw row harness is missing")?;
    expected.device_id = row
        .resource_attr("gently.device_id")
        .context("raw row device is missing")?;
    ensure!(
        &expected == context,
        "raw object context does not match this row"
    );
    Ok(())
}

/// Hydrate from an already authenticated, request-local payload. Context and
/// field-to-span ownership still apply to every row that references it.
pub(crate) fn resolve_payload(
    tenant_id: &str,
    row: &mut SpanRow,
    payload: &RawPayload,
) -> Result<()> {
    validate_row_context(tenant_id, row, &payload.context)?;
    let blob = row
        .attrs_json
        .as_ref()
        .context("raw row attributes are missing")?;
    let mut attrs: Value = crate::json_fidelity::parse(blob).context("invalid span attributes")?;
    let arr = attrs
        .as_array_mut()
        .context("invalid span attribute shape")?;
    let mut additions = Vec::new();
    for attr in arr.iter() {
        if attr
            .get("value")
            .and_then(|v| v.get("stringValue"))
            .and_then(Value::as_str)
            != Some(payload.context.raw_ref.as_str())
        {
            continue;
        }
        let Some(base) = attr
            .get("key")
            .and_then(Value::as_str)
            .and_then(|key| key.strip_suffix(".raw_ref"))
        else {
            continue;
        };
        ensure!(
            payload
                .bindings
                .get(base)
                .is_some_and(|ids| ids.contains(&row.span_id)),
            "raw field belongs to another span"
        );
        let raw = payload
            .fields
            .get(base)
            .context("encrypted raw field is missing")?;
        additions.push(json!({"key":base,"value":{"stringValue":raw}}));
    }
    for addition in additions {
        arr.retain(|attr| attr.get("key") != addition.get("key"));
        arr.push(addition);
    }
    row.attrs_json = Some(serde_json::to_string(&attrs)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use gently_raw::{DeviceIdentity, Manifest, OwnerKey, Recipient};

    fn fixture() -> (RawObject, ReaderIdentities, SpanRow) {
        let identity = DeviceIdentity::generate();
        let owner = OwnerKey::generate();
        let manifest = Manifest {
            version: 1,
            tenant_id: "personal".into(),
            key_epoch: 1,
            expires_unix_secs: 4_102_444_800,
            readers: vec![Recipient {
                device_id: "reader".into(),
                key_id: "key".into(),
                recipient: identity.to_public().to_string(),
            }],
        };
        let pin = TrustPin {
            tenant_id: "personal".into(),
            owner_verify_key_b64: owner.verification_key_b64(),
            min_epoch: 1,
            manifest_digest: gently_raw::manifest_digest(&manifest).unwrap(),
        };
        let verified =
            VerifiedManifest::verify(&gently_raw::sign_manifest(manifest, &owner).unwrap(), &pin)
                .unwrap();
        let context = RawContext {
            tenant_id: "personal".into(),
            device_id: "capture-host".into(),
            key_epoch: 1,
            raw_ref: gently_raw::new_raw_ref(),
            session_id: "session".into(),
            harness: "codex".into(),
            event: "UserPromptSubmit".into(),
        };
        let span_id = "0123456789abcdef";
        let object = gently_raw::seal(
            &verified,
            context.clone(),
            BTreeMap::from([("gently.prompt".into(), "private fixture".into())]),
            BTreeMap::from([("gently.prompt".into(), vec![span_id.into()])]),
        )
        .unwrap();
        let row = SpanRow {
            span_id: span_id.into(),
            trace_id: "trace".into(),
            parent_span_id: None,
            name: "turn:1".into(),
            kind: 1,
            start_unix_nano: "1".into(),
            end_unix_nano: Some("2".into()),
            status: 1,
            session_id: Some("session".into()),
            harness: Some("codex".into()),
            tool_name: None,
            tool_use_id: None,
            effective_start_unix_nano: None,
            effective_end_unix_nano: None,
            resource_json: Some(
                json!([
                    {"key":"gently.tenant_id","value":{"stringValue":"personal"}},
                    {"key":"gently.device_id","value":{"stringValue":"capture-host"}}
                ])
                .to_string(),
            ),
            attrs_json: Some(
                json!([
                    {"key":"gently.event","value":{"stringValue":"Stop"}},
                    {"key":"gently.prompt.raw_ref","value":{"stringValue":context.raw_ref}}
                ])
                .to_string(),
            ),
        };
        (object, ReaderIdentities::from_native(vec![identity]), row)
    }

    #[test]
    fn completed_row_resolves_open_event_content_using_authenticated_span_binding() {
        let (object, identities, mut row) = fixture();
        resolve_row("personal", &mut row, &object, &identities).unwrap();
        let attrs: Value = serde_json::from_str(row.attrs_json.as_deref().unwrap()).unwrap();
        assert!(attrs
            .as_array()
            .unwrap()
            .iter()
            .any(|attr| attr["key"] == "gently.prompt"
                && attr["value"]["stringValue"] == "private fixture"));
    }

    #[test]
    fn hydration_preserves_literal_private_json_keys() {
        let (object, identities, mut row) = fixture();
        let mut attrs = crate::json_fidelity::parse(row.attrs_json.as_deref().unwrap()).unwrap();
        let literal = json!({
            "key": "gently.synthetic_metadata",
            "value": {"$serde_json::private::Number": "literal-key"}
        });
        attrs.as_array_mut().unwrap().push(literal.clone());
        row.attrs_json = Some(attrs.to_string());
        resolve_row("personal", &mut row, &object, &identities).unwrap();
        let resolved = crate::json_fidelity::parse(row.attrs_json.as_deref().unwrap()).unwrap();
        assert!(resolved.as_array().unwrap().contains(&literal));
    }

    #[test]
    fn hydration_rejects_swapped_span_tenant_device_and_reader_without_mutating_row() {
        let (object, identities, row) = fixture();
        let mut wrong_span = row.clone();
        wrong_span.span_id = "fedcba9876543210".into();
        let before = wrong_span.attrs_json.clone();
        assert!(resolve_row("personal", &mut wrong_span, &object, &identities).is_err());
        assert_eq!(wrong_span.attrs_json, before);
        let mut wrong_tenant = row.clone();
        assert!(resolve_row("other", &mut wrong_tenant, &object, &identities).is_err());
        let mut wrong_device = row.clone();
        wrong_device.resource_json = wrong_device
            .resource_json
            .map(|json| json.replace("capture-host", "different-host"));
        assert!(resolve_row("personal", &mut wrong_device, &object, &identities).is_err());
        let stranger = ReaderIdentities::from_native(vec![DeviceIdentity::generate()]);
        assert!(resolve_row("personal", &mut row.clone(), &object, &stranger).is_err());
    }

    #[test]
    fn raw_reference_requests_reject_unknown_fields_and_malformed_refs() {
        let (_, _, mut row) = fixture();
        row.attrs_json = Some(json!([{"key":"unknown.raw_ref","value":{"stringValue":"00000000000000000000000000000000"}}]).to_string());
        assert!(row_references(&row).is_err());
        row.attrs_json = Some(
            json!([{"key":"gently.prompt.raw_ref","value":{"stringValue":"../../fixture"}}])
                .to_string(),
        );
        assert!(row_references(&row).is_err());
    }
}
