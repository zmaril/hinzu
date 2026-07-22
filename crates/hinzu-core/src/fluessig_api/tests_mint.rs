//! Tests for minting a NAMED INTERFACE from an anonymous object-of-methods (a
//! handle return) — the case where `openRpcStream` returns
//! `{ handleRpc(c): Promise<R>; close(): void } | undefined`. Kept in its own file
//! so `tests.rs` stays under the size cap.

use super::tests::demo_report;
use super::*;
use crate::api::{ApiItem, Signature};

/// A `record` data item named `name` with the given `fields` (`(field, ty)`).
fn record(name: &str, fields: &[(&str, &str)]) -> ApiItem {
    let mut it = ApiItem::new("record", &format!("m#{name}"), name, "m");
    it.fields = fields
        .iter()
        .map(|(f, t)| crate::api::Field {
            name: f.to_string(),
            ty: t.to_string(),
            visibility: "public".to_string(),
            doc: None,
            optional: false,
        })
        .collect();
    it
}

/// A free `function` named `name` with no params returning `ret`.
fn func_ret(name: &str, ret: &str) -> ApiItem {
    let mut it = ApiItem::new("function", &format!("m#{name}"), name, "m");
    it.signature = Some(Signature {
        params: vec![],
        return_type: Some(ret.to_string()),
        is_async: false,
        receiver: None,
        error_type: None,
        generics: vec![],
    });
    it
}

/// The minted interface in an output (the one interface that is not the
/// free-function `Demo` group).
fn minted(out: &super::FluessigOutput) -> &FlInterface {
    out.api
        .interfaces
        .iter()
        .find(|i| i.name != "Demo")
        .expect("a minted interface")
}

#[test]
fn anonymous_method_object_mints_an_interface_handle() {
    let report = demo_report(vec![
        record("RpcCommand", &[("kind", "string")]),
        record("RpcResponse", &[("ok", "boolean")]),
        func_ret(
            "openRpcStream",
            "{ handleRpc(command: RpcCommand): Promise<RpcResponse>; close(): void; } | undefined",
        ),
    ]);
    let out = build_fluessig(&report, &[]);

    // The op's return is now a typed nullable handle ref, not Json.
    let demo = out
        .api
        .interfaces
        .iter()
        .find(|i| i.name == "Demo")
        .unwrap();
    let stream = demo.ops.iter().find(|o| o.name == "openRpcStream").unwrap();
    let FlType::Nullable { nullable } = &stream.returns else {
        panic!("expected a nullable return, got {:?}", stream.returns);
    };
    let FlType::Model { model } = nullable.as_ref() else {
        panic!("expected a model handle ref, got {nullable:?}");
    };

    // A minted interface with that name, declared in interfaces[].
    let iface = minted(&out);
    assert_eq!(&iface.name, model);
    assert_eq!(out.stats.interfaces_minted, 1);

    // Its ops: close() (unary, void) and handleRpc (async unary, typed).
    let close = iface.ops.iter().find(|o| o.name == "close").unwrap();
    assert_eq!(close.shape, "unary");
    assert!(!close.is_async);
    assert!(close.params.is_empty());
    assert_eq!(close.returns, FlType::Scalar("void".to_string()));

    let handle = iface.ops.iter().find(|o| o.name == "handleRpc").unwrap();
    assert_eq!(handle.shape, "unary");
    assert!(handle.is_async);
    assert_eq!(
        handle.params[0].ty,
        FlType::Model {
            model: "RpcCommand".to_string()
        }
    );
    assert_eq!(
        handle.returns,
        FlType::Model {
            model: "RpcResponse".to_string()
        }
    );

    // The mint ADDS its 2 ops to the tally; the owning op flips clean → 3/3.
    assert_eq!(out.stats.ops_total, 3);
    assert_eq!(out.stats.ops_clean, 3);
    assert_eq!(out.stats.ops_degraded, 0);
}

#[test]
fn identical_method_objects_dedupe_to_one_interface() {
    // Two ops returning the SAME method-object shape mint a single interface.
    let report = demo_report(vec![
        func_ret("streamA", "{ ping(): void; } | undefined"),
        func_ret("streamB", "{ ping(): void; } | undefined"),
    ]);
    let out = build_fluessig(&report, &[]);

    assert_eq!(out.stats.interfaces_minted, 1);
    // Both ops reference the same minted name.
    let demo = out
        .api
        .interfaces
        .iter()
        .find(|i| i.name == "Demo")
        .unwrap();
    let name_of = |op: &str| {
        let o = demo.ops.iter().find(|o| o.name == op).unwrap();
        match &o.returns {
            FlType::Nullable { nullable } => match nullable.as_ref() {
                FlType::Model { model } => model.clone(),
                other => panic!("{other:?}"),
            },
            other => panic!("{other:?}"),
        }
    };
    assert_eq!(name_of("streamA"), name_of("streamB"));

    // 2 free ops + the single minted `ping` op = 3, all clean; deduped mint counts
    // its op ONCE.
    assert_eq!(out.stats.ops_total, 3);
    assert_eq!(out.stats.ops_clean, 3);
}

#[test]
fn union_param_method_and_context_resolution() {
    // handleRequest(request: A | B): Promise<void> — a union param, void async
    // return. Mirrors the handler `openRpcStream` shape.
    let report = demo_report(vec![
        record("RpcCommand", &[("kind", "string")]),
        record("RpcExtensionUIResponse", &[("id", "string")]),
        func_ret(
            "openRpcStream",
            "{ handleRequest(request: RpcCommand | RpcExtensionUIResponse): Promise<void>; close(): void; } | undefined",
        ),
    ]);
    let out = build_fluessig(&report, &[]);

    let iface = minted(&out);
    let hr = iface
        .ops
        .iter()
        .find(|o| o.name == "handleRequest")
        .unwrap();
    assert!(hr.is_async);
    assert_eq!(hr.returns, FlType::Scalar("void".to_string()));
    // The union param resolves to a synthesized union, not Json.
    assert!(matches!(hr.params[0].ty, FlType::Union { .. }));
    assert_eq!(out.stats.interfaces_minted, 1);
}

#[test]
fn data_only_object_stays_a_model_not_an_interface() {
    // A pure DATA object still mints a #37 MODEL — not touched by the method path.
    let report = demo_report(vec![func_ret("cfg", "{ id: string; name: string; }")]);
    let out = build_fluessig(&report, &[]);
    assert_eq!(out.stats.interfaces_minted, 0);
    assert_eq!(out.stats.models_minted, 1);
    // No extra interface beyond the free-function group.
    assert_eq!(out.api.interfaces.len(), 1);
}

#[test]
fn mixed_data_and_method_object_degrades_honestly() {
    // A MIXED object (data field + method) is not a faithful interface (which holds
    // only ops) — it falls through to the data path and degrades to Json.
    let report = demo_report(vec![func_ret(
        "mixed",
        "{ id: string; ping(): void; } | undefined",
    )]);
    let out = build_fluessig(&report, &[]);
    assert_eq!(out.stats.interfaces_minted, 0);
    let demo = &out.api.interfaces[0];
    let op = demo.ops.iter().find(|o| o.name == "mixed").unwrap();
    // Nullable Json — the honest degrade.
    assert_eq!(
        op.returns,
        FlType::Nullable {
            nullable: Box::new(FlType::Scalar("Json".to_string()))
        }
    );
    assert_eq!(out.stats.ops_degraded, 1);
}
