//! Unit tests for the JS/lib **builtin-type table** (`builtin_model`): the JS
//! builtin `Error` maps to a typed, declared `JsError { name, message, stack? }`
//! model instead of degrading to `Json`, which is what flips the orchestrator's
//! `RpcProcessInstance.onExit` from degraded to clean. Reuses the `demo_report`
//! and `op_item` scaffolds from the sibling [`super::tests`] module.

use super::tests::{demo_report, op_item};
use super::*;
use crate::api::ApiItem;

/// The focused unit: a callback param whose inner type is the JS builtin `Error`
/// parses to `{"callback":{"params":[{"nullable":{"model":"JsError"}}]}}` — the
/// builtin maps to the typed `JsError` model, so the callback is CLEAN.
#[test]
fn callback_param_error_maps_to_jserror_model() {
    let mut c = Converter::new(BTreeSet::new(), BTreeSet::new());
    let p = c.parse_type("(error?: Error | undefined) => void");
    assert!(!p.degraded);
    assert_eq!(
        serde_json::to_value(&p.ty).unwrap(),
        serde_json::json!({"callback": {"params": [{"nullable": {"model": "JsError"}}]}})
    );
}

/// The onExit shape end-to-end: a register→unsubscribe method whose listener is
/// `(error?: Error | undefined) => void`, on a CONSTRUCTIBLE interface. The op
/// flips to a CLEAN `subscription`, the callback param is a typed nullable
/// `JsError`, and `JsError { name, message, stack? }` is declared in `models[]`.
#[test]
fn onexit_style_builtin_error_op_counts_clean() {
    let class = ApiItem::new("class", "m#RpcProcessInstance", "RpcProcessInstance", "m");
    let on_exit = op_item(
        "method",
        "onExit",
        Some("RpcProcessInstance"),
        vec![("listener", "(error?: Error | undefined) => void")],
        Some("() => void"),
    );
    let factory = op_item(
        "function",
        "createRpcProcessInstance",
        None,
        vec![],
        Some("RpcProcessInstance"),
    );
    let out = build_fluessig(&demo_report(vec![class, on_exit, factory]), &[]);

    let op = out
        .api
        .interfaces
        .iter()
        .flat_map(|i| &i.ops)
        .find(|o| o.name == "onExit")
        .expect("onExit op is emitted");
    assert_eq!(op.shape, "subscription");
    assert_eq!(op.returns, FlType::Scalar("void".to_string()));
    assert_eq!(
        serde_json::to_value(&op.params[0].ty).unwrap(),
        serde_json::json!({"callback": {"params": [{"nullable": {"model": "JsError"}}]}})
    );

    // `JsError` is declared with the three standard fields; `stack` is nullable.
    let m = out
        .api
        .models
        .iter()
        .find(|m| m.name == "JsError")
        .expect("JsError model is declared in models[]");
    let shape: Vec<(&str, &FlType, bool)> = m
        .fields
        .iter()
        .map(|f| (f.name.as_str(), &f.ty, f.nullable))
        .collect();
    assert_eq!(
        shape,
        vec![
            ("name", &FlType::Scalar("string".to_string()), false),
            ("message", &FlType::Scalar("string".to_string()), false),
            ("stack", &FlType::Scalar("string".to_string()), true),
        ]
    );

    // The onExit op flipped clean: nothing degraded, and no honest-degrade reason
    // was recorded (both the factory and the now-clean subscription count clean).
    assert_eq!(out.stats.ops_degraded, 0);
    assert_eq!(out.stats.ops_clean, 2);
    assert!(out.stats.degradation_reasons.is_empty());
}
