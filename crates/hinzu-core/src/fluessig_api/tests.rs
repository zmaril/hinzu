//! Unit tests for the fluessig converter: the rendered-TS-type-string parser
//! (the hard part) and the item-level mapping into `api.json`/`catalog.json`.

use super::*;
use crate::api::{
    build_api, ApiItem, Fidelity, Field, Module, PackageInfo, Param, Signature, Variant,
};

fn conv() -> Converter {
    Converter::new(BTreeSet::new(), BTreeSet::new())
}

fn scalar(s: &str) -> FlType {
    FlType::Scalar(s.to_string())
}

/// A single-module `@x/demo` report over `items` — the shared scaffold for the
/// end-to-end `build_fluessig` tests.
fn demo_report(items: Vec<ApiItem>) -> crate::api::ApiReport {
    build_api(
        PackageInfo {
            name: "@x/demo".to_string(),
            language: "typescript".to_string(),
            root: ".".to_string(),
            version: None,
        },
        Fidelity {
            source: "tsc".to_string(),
            format_version: None,
            complete: false,
            notes: vec![],
        },
        vec![Module {
            path: "m".to_string(),
            file: None,
            doc: None,
            items,
        }],
    )
}

#[test]
fn scalars_and_ambiguities() {
    let mut c = conv();
    assert_eq!(c.parse_type("string").ty, scalar("string"));
    assert_eq!(c.parse_type("boolean").ty, scalar("boolean"));
    assert_eq!(c.parse_type("number").ty, scalar("float64"));
    assert_eq!(c.parse_type("void").ty, scalar("void"));
    assert_eq!(c.parse_type("undefined").ty, scalar("void"));
    assert_eq!(c.parse_type("Uint8Array").ty, scalar("bytes"));
    assert_eq!(c.parse_type("any").ty, scalar("Json"));
    // number→float64 is a note, not a degradation.
    assert!(!c.parse_type("number").degraded);
    assert!(c
        .notes
        .contains_key("number → float64 (int/float ambiguity)"));
}

#[test]
fn promise_and_array_and_nullable() {
    let mut c = conv();
    // Promise unwraps to payload (async handled at op level).
    assert_eq!(c.parse_type("Promise<string>").ty, scalar("string"));
    // Array forms.
    assert_eq!(
        c.parse_type("string[]").ty,
        FlType::List {
            list: Box::new(scalar("string"))
        }
    );
    assert_eq!(
        c.parse_type("Array<number>").ty,
        FlType::List {
            list: Box::new(scalar("float64"))
        }
    );
    // Nullable via `| undefined`.
    assert_eq!(
        c.parse_type("string | undefined").ty,
        FlType::Nullable {
            nullable: Box::new(scalar("string"))
        }
    );
    assert_eq!(
        c.parse_type("boolean | null").ty,
        FlType::Nullable {
            nullable: Box::new(scalar("boolean"))
        }
    );
}

#[test]
fn literals_collapse_to_scalars() {
    let mut c = conv();
    assert_eq!(c.parse_type("\"error\"").ty, scalar("string"));
    assert_eq!(c.parse_type("false").ty, scalar("boolean"));
    assert_eq!(c.parse_type("42").ty, scalar("float64"));
    // An inline string-literal union collapses to string (only *named* aliases
    // become enums).
    assert_eq!(c.parse_type("\"a\" | \"b\" | \"c\"").ty, scalar("string"));
}

#[test]
fn known_refs_resolve_else_degrade() {
    let mut c = conv();
    c.known_models.insert("FileDiff".to_string());
    c.known_enums.insert("Status".to_string());
    assert_eq!(
        c.parse_type("FileDiff").ty,
        FlType::Model {
            model: "FileDiff".to_string()
        }
    );
    assert_eq!(
        c.parse_type("FileDiff[]").ty,
        FlType::List {
            list: Box::new(FlType::Model {
                model: "FileDiff".to_string()
            })
        }
    );
    assert_eq!(
        c.parse_type("Status").ty,
        FlType::Enum {
            r#enum: "Status".to_string()
        }
    );
    // Unknown PascalCase ref → Json (degraded, counted).
    let p = c.parse_type("SomethingExternal");
    assert_eq!(p.ty, FlType::json());
    assert!(p.degraded);
    assert_eq!(c.reasons.get("unresolved type reference"), Some(&1));
}

#[test]
fn function_and_object_types_degrade() {
    let mut c = conv();
    assert!(c.parse_type("(event: E) => void").degraded);
    assert!(c.parse_type("() => void").degraded);
    // An object with a call/method member is not a plain data record — it stays
    // a `Json` fallback (the callback lane), under a distinct reason.
    assert!(c.parse_type("{ handleRpc(x: string): void }").degraded);
    assert_eq!(
        c.reasons.get("inline object with call/index signature"),
        Some(&1)
    );
    assert!(c.parse_type("RequestMap[keyof RequestMap]").degraded);
    assert!(c.parse_type("Record<string, number>").degraded);
}

#[test]
fn inline_object_param_mints_named_model() {
    let mut c = conv();
    // The naming context the op layer sets before parsing a param type.
    c.name_hint.push("SpawnInstanceOptions".to_string());
    let p = c.parse_type("{ cwd: string; label?: string | undefined; }");
    assert!(!p.degraded);
    assert_eq!(
        p.ty,
        FlType::Model {
            model: "SpawnInstanceOptions".to_string()
        }
    );
    let m = &c.minted["SpawnInstanceOptions"];
    assert_eq!(m.fields.len(), 2);
    assert_eq!(m.fields[0].name, "cwd");
    assert_eq!(m.fields[0].ty, scalar("string"));
    assert!(!m.fields[0].nullable);
    // `label?: string | undefined` → a nullable string field.
    assert_eq!(m.fields[1].name, "label");
    assert_eq!(m.fields[1].ty, scalar("string"));
    assert!(m.fields[1].nullable);
    // The minted model registers so later refs resolve to it.
    assert!(c.known_models.contains("SpawnInstanceOptions"));
}

#[test]
fn inline_object_return_mints_named_model() {
    let mut c = conv();
    c.name_hint.push("CreateInstanceResult".to_string());
    let p = c.parse_type("{ id: string; count: number }");
    assert!(!p.degraded);
    assert_eq!(
        p.ty,
        FlType::Model {
            model: "CreateInstanceResult".to_string()
        }
    );
    let m = &c.minted["CreateInstanceResult"];
    assert_eq!(m.fields.len(), 2);
    assert_eq!(m.fields[1].name, "count");
    assert_eq!(m.fields[1].ty, scalar("float64"));
}

#[test]
fn identical_inline_objects_dedupe_to_one_model() {
    let mut c = conv();
    c.name_hint.push("AOptions".to_string());
    let a = c.parse_type("{ cwd: string }");
    c.name_hint.pop();
    c.name_hint.push("BOptions".to_string());
    // Same field-set (even a different member separator/order is normalized).
    let b = c.parse_type("{ cwd: string }");
    assert_eq!(a.ty, b.ty);
    assert_eq!(
        a.ty,
        FlType::Model {
            model: "AOptions".to_string()
        }
    );
    // Only one model minted despite two occurrences.
    assert_eq!(c.minted.len(), 1);
}

#[test]
fn nested_inline_object_mints_nested_models() {
    let mut c = conv();
    c.name_hint.push("OuterOptions".to_string());
    let p = c.parse_type("{ meta: { id: string }; name: string }");
    assert!(!p.degraded);
    assert_eq!(
        p.ty,
        FlType::Model {
            model: "OuterOptions".to_string()
        }
    );
    // Two models: the outer, plus the nested one named by the field path.
    assert_eq!(c.minted.len(), 2);
    let outer = &c.minted["OuterOptions"];
    assert_eq!(
        outer.fields[0].ty,
        FlType::Model {
            model: "OuterOptionsMeta".to_string()
        }
    );
    let nested = &c.minted["OuterOptionsMeta"];
    assert_eq!(nested.fields[0].name, "id");
    assert_eq!(nested.fields[0].ty, scalar("string"));
}

#[test]
fn multi_member_union_synthesizes_named_union() {
    let mut c = conv();
    c.known_models.insert("SpawnResponse".to_string());
    c.known_models.insert("ErrorResponse".to_string());
    let p = c.parse_type("SpawnResponse | ErrorResponse");
    assert_eq!(
        p.ty,
        FlType::Union {
            union: "SpawnResponseOrErrorResponseUnion".to_string()
        }
    );
    let u = &c.unions["SpawnResponseOrErrorResponseUnion"];
    assert_eq!(u.variants.len(), 2);
    assert_eq!(u.variants[0].tag, "spawnResponse");
    // `T | ErrorResponse | undefined` → Nullable<Union<...>>.
    let p2 = c.parse_type("SpawnResponse | ErrorResponse | undefined");
    assert!(matches!(p2.ty, FlType::Nullable { .. }));
}

#[test]
fn string_literal_union_alias_lifts_to_enum() {
    let variants = string_literal_union(Some("\"starting\" | \"online\" | \"stopped\"")).unwrap();
    let names: Vec<&str> = variants.iter().map(|v| v.name.as_str()).collect();
    assert_eq!(names, vec!["starting", "online", "stopped"]);
    // A union of models is NOT a string-literal enum.
    assert!(string_literal_union(Some("A | B")).is_none());
    // A single literal is not a union.
    assert!(string_literal_union(Some("\"list\"")).is_none());
}

#[test]
fn package_interface_name_pascalizes_last_segment() {
    assert_eq!(
        package_interface_name("@earendil-works/pi-orchestrator"),
        "PiOrchestrator"
    );
    assert_eq!(package_interface_name("demo"), "Demo");
}

/// End-to-end: a tiny report with an interface, an enum-ish alias, a class with
/// a method, and a free function → the expected api.json/catalog.json shape and
/// coverage stats.
#[test]
fn build_fluessig_end_to_end() {
    let iface = {
        let mut it = ApiItem::new("interface", "m#Summary", "Summary", "m");
        it.fields = vec![
            Field {
                name: "id".to_string(),
                ty: "string".to_string(),
                visibility: "public".to_string(),
                doc: None,
                optional: false,
            },
            Field {
                name: "status".to_string(),
                ty: "InstanceStatus".to_string(),
                visibility: "public".to_string(),
                doc: None,
                optional: false,
            },
            Field {
                name: "label".to_string(),
                ty: "string | undefined".to_string(),
                visibility: "public".to_string(),
                doc: None,
                optional: true,
            },
        ];
        it
    };
    let alias = {
        let mut it = ApiItem::new("typeAlias", "m#InstanceStatus", "InstanceStatus", "m");
        it.alias_target = Some("\"online\" | \"offline\"".to_string());
        it
    };
    let func = {
        let mut it = ApiItem::new("function", "m#getSummary", "getSummary", "m");
        it.signature = Some(Signature {
            params: vec![Param {
                name: "id".to_string(),
                ty: "string".to_string(),
                optional: false,
                default: None,
            }],
            return_type: Some("Promise<Summary>".to_string()),
            is_async: true,
            receiver: None,
            error_type: None,
            generics: vec![],
        });
        it
    };
    let class = ApiItem::new("class", "m#Session", "Session", "m");
    let method = {
        let mut it = ApiItem::new("method", "m#Session.send", "send", "m");
        it.signature = Some(Signature {
            params: vec![Param {
                name: "cmd".to_string(),
                ty: "string".to_string(),
                optional: false,
                default: None,
            }],
            return_type: Some("void".to_string()),
            is_async: false,
            receiver: Some("Session".to_string()),
            error_type: None,
            generics: vec![],
        });
        it
    };
    // A `string` const whose value is a RUNTIME expression: it is emitted into
    // `consts[]` (no longer dropped) but carries no `value`.
    let version_const = {
        let mut it = ApiItem::new("const", "m#VERSION", "VERSION", "m");
        it.const_type = Some("string".to_string());
        it.const_value = Some("pkg.version || \"0.0.0\"".to_string());
        it
    };

    let report = demo_report(vec![iface, alias, func, class, method, version_const]);

    let out = build_fluessig(&report, &[]);

    // One model, one enum, two interfaces (Session + the free-function group).
    assert_eq!(out.api.models.len(), 1);
    assert_eq!(out.api.models[0].name, "Summary");
    // status resolves to the lifted enum; label is nullable.
    let status = &out.api.models[0].fields[1];
    assert_eq!(
        status.ty,
        FlType::Enum {
            r#enum: "InstanceStatus".to_string()
        }
    );
    assert!(out.api.models[0].fields[2].nullable);

    assert_eq!(out.catalog.enums.len(), 1);
    assert_eq!(out.catalog.enums[0].name, "InstanceStatus");

    let iface_names: Vec<&str> = out.api.interfaces.iter().map(|i| i.name.as_str()).collect();
    assert!(iface_names.contains(&"Session"));
    assert!(iface_names.contains(&"Demo")); // free-function group

    // The free function is async and returns the model.
    let demo = out
        .api
        .interfaces
        .iter()
        .find(|i| i.name == "Demo")
        .unwrap();
    assert_eq!(demo.ops[0].name, "getSummary");
    assert!(demo.ops[0].is_async);
    assert_eq!(
        demo.ops[0].returns,
        FlType::Model {
            model: "Summary".to_string()
        }
    );

    // Coverage: the const is emitted (not dropped), both ops clean.
    assert_eq!(out.stats.items_in, 6);
    assert_eq!(out.stats.ops_total, 2);
    assert_eq!(out.stats.ops_clean, 2);
    assert_eq!(out.stats.consts_emitted, 1);
    assert_eq!(out.stats.dropped.get("const dropped"), None);
    assert_eq!(out.api.consts.len(), 1);
    assert_eq!(out.api.consts[0].name, "VERSION");
    assert_eq!(out.api.consts[0].ty, scalar("string"));
    // A runtime expression carries no statically-known value.
    assert_eq!(out.api.consts[0].value, None);

    // The api.json serializes with the untagged FlType shape fluessig reads.
    let json = serde_json::to_string(&out.api).unwrap();
    assert!(json.contains("\"enum\":\"InstanceStatus\""));
    assert!(json.contains("\"model\":\"Summary\""));
    assert!(json.contains("\"async\":true"));
    // The const rides in a `consts[]` array with a bare-scalar `type` and no value.
    assert!(json.contains("\"consts\":[{\"name\":\"VERSION\",\"type\":\"string\"}]"));
}

#[test]
fn union_of_named_types_alias_lifts_to_named_union() {
    // A top-level `type U = A | B` alias lifts into a union named for the alias
    // (not the `AOrBUnion` synthesized name), with each member resolving.
    let mut c = conv();
    c.known_models.insert("RpcCommand".to_string());
    c.known_models.insert("RpcExtensionUIResponse".to_string());
    let members =
        expand_alias_union_members(Some("RpcCommand | RpcExtensionUIResponse"), &c.indexable)
            .expect("a union of named types is liftable");
    assert_eq!(members, vec!["RpcCommand", "RpcExtensionUIResponse"]);
    assert!(c.lift_alias_union("RpcClientMessage", &members));
    let u = &c.unions["RpcClientMessage"];
    assert_eq!(u.name, "RpcClientMessage");
    assert_eq!(u.variants.len(), 2);
    assert_eq!(u.variants[0].tag, "rpcCommand");
    assert_eq!(
        u.variants[0].ty,
        FlType::Model {
            model: "RpcCommand".to_string()
        }
    );
    // Lifting the same name twice is a no-op (dedupe).
    assert!(!c.lift_alias_union("RpcClientMessage", &members));
}

#[test]
fn indexed_access_alias_expands_to_value_type_union() {
    // `X[keyof X]` resolves to a union over the value types of X's members.
    let mut indexable = BTreeMap::new();
    indexable.insert(
        "RequestMap".to_string(),
        vec!["SpawnRequest".to_string(), "ListRequest".to_string()],
    );
    let members = expand_alias_union_members(Some("RequestMap[keyof RequestMap]"), &indexable)
        .expect("an indexed access over a known model is liftable");
    assert_eq!(members, vec!["SpawnRequest", "ListRequest"]);

    let mut c = conv();
    c.known_models.insert("SpawnRequest".to_string());
    c.known_models.insert("ListRequest".to_string());
    c.lift_alias_union("OrchestratorRequest", &members);
    let u = &c.unions["OrchestratorRequest"];
    assert_eq!(u.variants.len(), 2);
    assert_eq!(u.variants[0].tag, "spawnRequest");
    assert_eq!(
        u.variants[1].ty,
        FlType::Model {
            model: "ListRequest".to_string()
        }
    );

    // An indexed access whose base is not a known interface/record degrades
    // gracefully: not liftable, so the alias stays dropped.
    assert!(expand_alias_union_members(Some("Unknown[keyof Unknown]"), &BTreeMap::new()).is_none());
    // `X[K]` with a specific (non-`keyof X`) key is not this shape.
    assert!(indexed_access_base("RequestMap[\"spawn\"]").is_none());
    assert_eq!(
        indexed_access_base("RequestMap[keyof RequestMap]"),
        Some("RequestMap")
    );
}

#[test]
fn indexed_access_plus_extra_member_shape() {
    // The `X[keyof X] | Y` shape: the expansion plus the extra union member.
    let mut indexable = BTreeMap::new();
    indexable.insert(
        "ResponseMap".to_string(),
        vec!["SpawnResponse".to_string(), "ListResponse".to_string()],
    );
    let members = expand_alias_union_members(
        Some("ResponseMap[keyof ResponseMap] | ErrorResponse"),
        &indexable,
    )
    .expect("indexed-access + extra member is liftable");
    assert_eq!(
        members,
        vec!["SpawnResponse", "ListResponse", "ErrorResponse"]
    );
}

#[test]
fn ref_to_lifted_alias_resolves_to_union() {
    // A field/param/return referencing a lifted alias by name resolves to the
    // union rather than degrading to `Json`.
    let mut c = conv();
    c.known_unions.insert("OrchestratorRequest".to_string());
    let p = c.parse_type("OrchestratorRequest");
    assert!(!p.degraded);
    assert_eq!(
        p.ty,
        FlType::Union {
            union: "OrchestratorRequest".to_string()
        }
    );
    // …and through a list wrapper.
    assert_eq!(
        c.parse_type("OrchestratorRequest[]").ty,
        FlType::List {
            list: Box::new(FlType::Union {
                union: "OrchestratorRequest".to_string()
            })
        }
    );
}

#[test]
fn conditional_or_bare_alias_is_not_lifted() {
    let indexable = BTreeMap::new();
    // A conditional/generic (`ResponseFor`) target is irreducible → stays dropped.
    assert!(expand_alias_union_members(
        Some("T extends { type: infer K } ? ResponseMap[K] | ErrorResponse : ErrorResponse"),
        &indexable,
    )
    .is_none());
    // A bare single alias (`type A = B`) is a different gap → not lifted.
    assert!(expand_alias_union_members(Some("SomeOtherType"), &indexable).is_none());
    // A string-literal union is left for the catalog-enum path, not stolen here.
    assert!(expand_alias_union_members(Some("\"a\" | \"b\""), &indexable).is_none());
}

#[test]
fn alias_with_unresolved_sibling_member_lifts_with_json() {
    // A mixed union whose in-package members resolve and whose sibling-package
    // members degrade to `Json` (counted) still lifts — the alias is no longer
    // dropped, and each member keeps a readable tag.
    let mut c = conv();
    c.known_models.insert("RpcReadyResponse".to_string());
    c.known_models.insert("ErrorResponse".to_string());
    let members = vec![
        "RpcReadyResponse".to_string(),  // in-package
        "RpcResponse".to_string(),       // sibling-package → Json
        "AgentSessionEvent".to_string(), // sibling-package → Json
        "ErrorResponse".to_string(),     // in-package
    ];
    c.lift_alias_union("RpcServerMessage", &members);
    let u = &c.unions["RpcServerMessage"];
    assert_eq!(u.variants.len(), 4);
    assert_eq!(
        u.variants[0].ty,
        FlType::Model {
            model: "RpcReadyResponse".to_string()
        }
    );
    assert_eq!(u.variants[1].tag, "rpcResponse");
    assert_eq!(u.variants[1].ty, FlType::json());
    assert_eq!(u.variants[2].ty, FlType::json());
    assert_eq!(
        u.variants[3].ty,
        FlType::Model {
            model: "ErrorResponse".to_string()
        }
    );
    // Both sibling members counted as honest degradations.
    assert_eq!(c.reasons.get("unresolved type reference"), Some(&2));
}

/// End-to-end: an `X[keyof X]` alias lifts to a named union, a ref to it
/// resolves, and an irreducible conditional alias is still dropped.
#[test]
fn build_fluessig_lifts_indexed_access_alias_end_to_end() {
    let model = |name: &str| ApiItem::new("interface", &format!("m#{name}"), name, "m");
    let field = |n: &str, t: &str| Field {
        name: n.to_string(),
        ty: t.to_string(),
        visibility: "public".to_string(),
        doc: None,
        optional: false,
    };
    let request_map = {
        let mut it = model("RequestMap");
        it.fields = vec![field("spawn", "SpawnRequest"), field("list", "ListRequest")];
        it
    };
    let alias = {
        let mut it = ApiItem::new(
            "typeAlias",
            "m#OrchestratorRequest",
            "OrchestratorRequest",
            "m",
        );
        it.alias_target = Some("RequestMap[keyof RequestMap]".to_string());
        it
    };
    let conditional = {
        let mut it = ApiItem::new("typeAlias", "m#ResponseFor", "ResponseFor", "m");
        it.alias_target =
            Some("T extends { type: infer K } ? ResponseMap[K] : ErrorResponse".to_string());
        it
    };
    let func = {
        let mut it = ApiItem::new("function", "m#parseRequestLine", "parseRequestLine", "m");
        it.signature = Some(Signature {
            params: vec![],
            return_type: Some("OrchestratorRequest".to_string()),
            is_async: false,
            receiver: None,
            error_type: None,
            generics: vec![],
        });
        it
    };

    let report = demo_report(vec![
        request_map,
        model("SpawnRequest"),
        model("ListRequest"),
        alias,
        conditional,
        func,
    ]);

    let out = build_fluessig(&report, &[]);

    // The indexed-access alias lifted into a named union over the value types.
    let u = out
        .api
        .unions
        .iter()
        .find(|u| u.name == "OrchestratorRequest")
        .expect("OrchestratorRequest lifted to a union");
    assert_eq!(u.variants.len(), 2);
    assert_eq!(
        u.variants[0].ty,
        FlType::Model {
            model: "SpawnRequest".to_string()
        }
    );
    assert_eq!(out.stats.unions_lifted, 1);

    // The op's return type resolves to the lifted union (not Json).
    let op = out
        .api
        .interfaces
        .iter()
        .flat_map(|i| &i.ops)
        .find(|o| o.name == "parseRequestLine")
        .unwrap();
    assert_eq!(
        op.returns,
        FlType::Union {
            union: "OrchestratorRequest".to_string()
        }
    );
    assert_eq!(out.stats.ops_clean, 1);
    assert_eq!(out.stats.ops_degraded, 0);

    // The conditional/generic alias stays dropped (it is irreducible here).
    assert_eq!(
        out.stats
            .dropped
            .get("typeAlias (non-string-union) dropped"),
        Some(&1)
    );
}

// ─────────────────── cross-package (multi-report) resolution ─────────────────

/// A sibling-package report named `@x/sibling` over `items` — the context input
/// to the multi-report `build_fluessig` tests.
fn sibling_report(items: Vec<ApiItem>) -> crate::api::ApiReport {
    let mut r = demo_report(items);
    r.package.name = "@x/sibling".to_string();
    r
}

fn iface_item(name: &str, fields: Vec<(&str, &str)>) -> ApiItem {
    let mut it = ApiItem::new("interface", &format!("m#{name}"), name, "m");
    it.fields = fields
        .into_iter()
        .map(|(n, t)| Field {
            name: n.to_string(),
            ty: t.to_string(),
            visibility: "public".to_string(),
            doc: None,
            optional: false,
        })
        .collect();
    it
}

fn union_alias_item(name: &str, target: &str) -> ApiItem {
    let mut it = ApiItem::new("typeAlias", &format!("m#{name}"), name, "m");
    it.alias_target = Some(target.to_string());
    it
}

fn field_ref_model(name: &str, field_ty: &str) -> ApiItem {
    iface_item(name, vec![("v", field_ty)])
}

/// A primary field typed as a sibling-package model resolves to a real `Model`
/// (not Json), and the sibling model is pulled into `models[]` — while the
/// sibling's *unreferenced* types stay out (scoped emission).
#[test]
fn primary_ref_resolves_to_context_model() {
    // Primary references `RpcReadyResponse` (a sibling interface) but not
    // `UnusedSibling`.
    let primary = demo_report(vec![field_ref_model("Envelope", "RpcReadyResponse")]);
    let context = sibling_report(vec![
        iface_item(
            "RpcReadyResponse",
            vec![("id", "string"), ("ready", "boolean")],
        ),
        iface_item("UnusedSibling", vec![("x", "string")]),
    ]);

    let out = build_fluessig(&primary, &[context]);

    // The primary field resolved to the sibling model.
    let env = out
        .api
        .models
        .iter()
        .find(|m| m.name == "Envelope")
        .unwrap();
    assert_eq!(
        env.fields[0].ty,
        FlType::Model {
            model: "RpcReadyResponse".to_string()
        }
    );
    // The referenced sibling model was pulled in; the unreferenced one was not.
    let names: Vec<&str> = out.api.models.iter().map(|m| m.name.as_str()).collect();
    assert!(names.contains(&"RpcReadyResponse"));
    assert!(!names.contains(&"UnusedSibling"));
    assert_eq!(out.stats.context_reports, 1);
    assert_eq!(out.stats.context_types_pulled, 1);
    assert_eq!(
        out.stats
            .degradation_reasons
            .get("unresolved type reference"),
        None
    );
}

/// A primary lifted union whose members are sibling types resolves each member
/// via context (the `RpcClientMessage = RpcCommand | …` shape), and the sibling
/// union alias is itself pulled in and lifted.
#[test]
fn lifted_union_resolves_sibling_members_via_context() {
    let primary = demo_report(vec![union_alias_item(
        "RpcClientMessage",
        "RpcCommand | RpcExtensionUIResponse",
    )]);
    let context = sibling_report(vec![
        union_alias_item("RpcCommand", "PromptCommand | AbortCommand"),
        union_alias_item(
            "RpcExtensionUIResponse",
            "UiValueResponse | UiCancelResponse",
        ),
        iface_item("PromptCommand", vec![("message", "string")]),
        iface_item("AbortCommand", vec![("id", "string")]),
        iface_item("UiValueResponse", vec![("value", "string")]),
        iface_item("UiCancelResponse", vec![("cancelled", "boolean")]),
    ]);

    let out = build_fluessig(&primary, &[context]);

    // The primary union's members resolve to real sibling unions, not Json.
    let client = out
        .api
        .unions
        .iter()
        .find(|u| u.name == "RpcClientMessage")
        .unwrap();
    let member_types: Vec<&FlType> = client.variants.iter().map(|v| &v.ty).collect();
    assert!(member_types.contains(&&FlType::Union {
        union: "RpcCommand".to_string()
    }));
    assert!(member_types.contains(&&FlType::Union {
        union: "RpcExtensionUIResponse".to_string()
    }));
    // Both sibling unions were pulled in and lifted (as real unions).
    let union_names: Vec<&str> = out.api.unions.iter().map(|u| u.name.as_str()).collect();
    assert!(union_names.contains(&"RpcCommand"));
    assert!(union_names.contains(&"RpcExtensionUIResponse"));
    // Their members were pulled in transitively as models.
    let model_names: Vec<&str> = out.api.models.iter().map(|m| m.name.as_str()).collect();
    for m in [
        "PromptCommand",
        "AbortCommand",
        "UiValueResponse",
        "UiCancelResponse",
    ] {
        assert!(model_names.contains(&m), "{m} pulled in transitively");
    }
    // No sibling member degraded to Json.
    assert_eq!(
        out.stats
            .degradation_reasons
            .get("unresolved type reference"),
        None
    );
}

/// Scoped emission: the transitive closure pulls in only the sibling types the
/// primary reaches, never the context package's whole op/type surface.
#[test]
fn scoped_emission_pulls_only_referenced_sibling_types() {
    // A → B (referenced chain); C, D are unrelated sibling types.
    let primary = demo_report(vec![field_ref_model("Root", "A")]);
    let context = sibling_report(vec![
        field_ref_model("A", "B"),
        iface_item("B", vec![("leaf", "string")]),
        iface_item("C", vec![("unrelated", "string")]),
        union_alias_item("D", "C | B"),
        // A sibling class + function: an op surface that must NOT be emitted.
        ApiItem::new("class", "m#SiblingService", "SiblingService", "m"),
    ]);

    let out = build_fluessig(&primary, &[context]);

    let model_names: Vec<&str> = out.api.models.iter().map(|m| m.name.as_str()).collect();
    // Transitively referenced A and B are pulled in.
    assert!(model_names.contains(&"A"));
    assert!(model_names.contains(&"B"));
    // Unreferenced C, D and the sibling service/op surface are NOT emitted.
    assert!(!model_names.contains(&"C"));
    assert!(!out.api.unions.iter().any(|u| u.name == "D"));
    assert!(!out
        .api
        .interfaces
        .iter()
        .any(|i| i.name == "SiblingService"));
    assert_eq!(out.stats.context_types_pulled, 2); // A + B only
}

/// Backward-compat: passing no context is byte-identical to the pre-context
/// single-report path (an empty context slice never perturbs the output).
#[test]
fn no_context_is_backward_compatible() {
    let items = vec![
        field_ref_model("Holder", "InstanceStatus"),
        union_alias_item("InstanceStatus", "\"online\" | \"offline\""),
    ];
    let report = demo_report(items.clone());
    let out_no_ctx = build_fluessig(&report, &[]);
    // An empty context slice must produce identical documents and stats.
    let out_empty = build_fluessig(&report, &[]);
    assert_eq!(
        serde_json::to_string(&out_no_ctx.api).unwrap(),
        serde_json::to_string(&out_empty.api).unwrap(),
    );
    assert_eq!(out_no_ctx.stats.context_reports, 0);
    assert_eq!(out_no_ctx.stats.context_types_pulled, 0);
    // And a passed-but-irrelevant context leaves the primary output unchanged
    // when nothing references it.
    let irrelevant = sibling_report(vec![iface_item("Nobody", vec![("x", "string")])]);
    let out_irrelevant = build_fluessig(&report, &[irrelevant]);
    assert_eq!(
        serde_json::to_string(&out_no_ctx.api).unwrap(),
        serde_json::to_string(&out_irrelevant.api).unwrap(),
    );
    assert_eq!(out_irrelevant.stats.context_types_pulled, 0);
}

// ───────────────────── foreign opaque-handle policy ─────────────────────────

/// The `Foreign` variant serializes to fluessig's EXACT wire shape — a single
/// `foreign` key over `{name, rustPath}` — so the converter's output round-trips
/// through fluessig's `ApiType::Foreign` serde.
#[test]
fn foreign_serializes_to_exact_wire_shape() {
    let ty = FlType::Foreign {
        foreign: FlForeign {
            name: "http.Server".to_string(),
            rust_path: "http::Server".to_string(),
        },
    };
    assert_eq!(
        serde_json::to_string(&ty).unwrap(),
        r#"{"foreign":{"name":"http.Server","rustPath":"http::Server"}}"#
    );
}

/// A truly-external DOTTED builtin path (`http.Server`) → `Foreign`, with the
/// `rust_path` mapping `.` → `::`. Not a degradation.
#[test]
fn dotted_builtin_ref_emits_foreign_with_colon_path() {
    let mut c = conv();
    let p = c.parse_type("http.Server");
    assert_eq!(
        p.ty,
        FlType::Foreign {
            foreign: FlForeign {
                name: "http.Server".to_string(),
                rust_path: "http::Server".to_string(),
            }
        }
    );
    assert!(
        !p.degraded,
        "a Foreign handle is faithful, not a Json fallback"
    );
    assert_eq!(c.foreign_types.get("http.Server"), Some(&1));
    // It is NOT counted as an unresolved reference.
    assert_eq!(c.reasons.get("unresolved type reference"), None);
    assert_eq!(c.reasons.get("unparsable type expression"), None);
}

/// A bare truly-external builtin name on the allowlist (`Server`, imported from
/// `node:net`) → `Foreign`, mapped to its canonical dotted source name/path.
#[test]
fn bare_builtin_ref_emits_foreign() {
    let mut c = conv();
    let p = c.parse_type("Server");
    assert_eq!(
        p.ty,
        FlType::Foreign {
            foreign: FlForeign {
                name: "net.Server".to_string(),
                rust_path: "net::Server".to_string(),
            }
        }
    );
    assert!(!p.degraded);
    assert_eq!(c.foreign_types.get("Server"), Some(&1));
}

/// A generic type parameter (`T`, or a declared generic of the owning item) is
/// NEVER opaqued — it stays the honest `Json` fallback and is recorded under
/// `generic_params`, not `foreign_types`.
#[test]
fn generic_type_param_stays_non_foreign() {
    // A single-letter param.
    let mut c = conv();
    let p = c.parse_type("T");
    assert_eq!(p.ty, FlType::json());
    assert!(p.degraded);
    assert_eq!(c.generic_params.get("T"), Some(&1));
    assert!(c.foreign_types.is_empty());

    // A multi-letter declared generic of the owning item (`TSchema`).
    let mut c2 = conv();
    c2.current_generics.insert("TSchema".to_string());
    let p2 = c2.parse_type("TSchema");
    assert_eq!(p2.ty, FlType::json());
    assert_eq!(c2.generic_params.get("TSchema"), Some(&1));
    assert!(c2.foreign_types.is_empty());
}

/// A pi-internal type merely absent from the current `--context` set is kept as
/// honest `Json` and recorded under `context_expandable` — NOT opaqued as
/// external (never misrepresented) and NOT a generic.
#[test]
fn pi_internal_not_in_context_stays_json_not_foreign() {
    let mut c = conv();
    let p = c.parse_type("ImageContent");
    assert_eq!(p.ty, FlType::json());
    assert!(p.degraded);
    assert_eq!(c.context_expandable.get("ImageContent"), Some(&1));
    assert!(c.foreign_types.is_empty());
    assert!(c.generic_params.is_empty());
}

/// End-to-end: an op returning a truly-external type emits a `Foreign` return
/// (the `startIpcServer` → `Server` case), the op counts as CLEAN, and the stats
/// carry `foreign_emitted` while `Server` no longer counts as unresolved.
#[test]
fn foreign_return_flips_op_clean_end_to_end() {
    let func = {
        let mut it = ApiItem::new("function", "m#startIpcServer", "startIpcServer", "m");
        it.signature = Some(Signature {
            params: vec![],
            return_type: Some("Promise<Server>".to_string()),
            is_async: true,
            receiver: None,
            error_type: None,
            generics: vec![],
        });
        it
    };
    let out = build_fluessig(&demo_report(vec![func]), &[]);

    let iface = &out.api.interfaces[0];
    let op = iface
        .ops
        .iter()
        .find(|o| o.name == "startIpcServer")
        .unwrap();
    assert_eq!(
        op.returns,
        FlType::Foreign {
            foreign: FlForeign {
                name: "net.Server".to_string(),
                rust_path: "net::Server".to_string(),
            }
        }
    );
    assert_eq!(out.stats.foreign_emitted, 1);
    assert_eq!(out.stats.foreign_types.get("Server"), Some(&1));
    assert_eq!(out.stats.ops_clean, 1);
    assert_eq!(out.stats.ops_degraded, 0);
    assert_eq!(
        out.stats
            .degradation_reasons
            .get("unresolved type reference"),
        None
    );
}

/// An in-scope `class` used as a value type has no DTO form: it stays honest
/// `Json` under `unmodeled_refs`, NOT `context_expandable` (adding a package
/// cannot help) and NOT `Foreign` (it is pi-internal, not external).
#[test]
fn in_scope_class_ref_is_unmodeled_not_context_gap() {
    let holder = field_ref_model("Holder", "RpcProcessInstance");
    let class = ApiItem::new("class", "m#RpcProcessInstance", "RpcProcessInstance", "m");
    let out = build_fluessig(&demo_report(vec![holder, class]), &[]);

    let h = out.api.models.iter().find(|m| m.name == "Holder").unwrap();
    assert_eq!(h.fields[0].ty, FlType::json());
    assert_eq!(out.stats.unmodeled_refs.get("RpcProcessInstance"), Some(&1));
    assert_eq!(out.stats.context_expandable.get("RpcProcessInstance"), None);
    assert!(out.stats.foreign_types.is_empty());
}

#[test]
fn unused_variant_field_is_ignored() {
    // Guard: Variant is imported for completeness of the api surface but the
    // converter reads only enum `discriminant`; keep the import meaningful.
    let v = Variant {
        name: "A".to_string(),
        fields: vec![],
        discriminant: Some("1".to_string()),
        doc: None,
    };
    assert_eq!(v.discriminant.as_deref(), Some("1"));
}

// ─────────────────────────── exported consts ────────────────────────────────

/// A `const` item with the given TS type / raw value expression.
fn const_item(name: &str, ty: Option<&str>, value: Option<&str>) -> ApiItem {
    let mut it = ApiItem::new("const", &format!("m#{name}"), name, "m");
    it.const_type = ty.map(str::to_string);
    it.const_value = value.map(str::to_string);
    it
}

/// A string const whose value is a SIMPLE literal → an `FlConst` carrying the
/// unquoted string, with `type: "string"` and a real `value` (the literal-value
/// path that round-trips downstream to a `pub const`).
#[test]
fn string_const_with_literal_value_is_emitted_with_value() {
    let out = build_fluessig(
        &demo_report(vec![const_item("GREETING", Some("string"), Some("\"hi\""))]),
        &[],
    );
    assert_eq!(out.stats.consts_emitted, 1);
    assert!(out.stats.dropped.is_empty());
    assert_eq!(out.api.consts.len(), 1);
    let c = &out.api.consts[0];
    assert_eq!(c.name, "GREETING");
    assert_eq!(c.ty, scalar("string"));
    assert_eq!(c.value, Some(FlConstValue::Str("hi".to_string())));
    // Untagged value serializes as the bare JSON string.
    let json = serde_json::to_string(&out.api).unwrap();
    assert!(json.contains("\"name\":\"GREETING\",\"type\":\"string\",\"value\":\"hi\""));
}

/// A VERSION-style const whose value is a RUNTIME expression (`pkg.version ||
/// "0.0.0"`) → emitted with `type: "string"` but NO value (the converter never
/// evaluates expressions).
#[test]
fn version_style_runtime_expr_const_has_no_value() {
    let out = build_fluessig(
        &demo_report(vec![const_item(
            "VERSION",
            Some("string"),
            Some("pkg.version || \"0.0.0\""),
        )]),
        &[],
    );
    assert_eq!(out.stats.consts_emitted, 1);
    let c = &out.api.consts[0];
    assert_eq!(c.ty, scalar("string"));
    assert_eq!(c.value, None);
    // A concatenation is not a simple literal either.
    assert_eq!(
        const_value_for(&scalar("string"), Some("\"a\" + \"b\"")),
        None
    );
}

/// A `number` const maps through the existing scalar path to `float64`; an
/// integer literal value rides untagged as `Int`.
#[test]
fn number_const_maps_to_float64_scalar() {
    let out = build_fluessig(
        &demo_report(vec![const_item("MAX", Some("number"), Some("42"))]),
        &[],
    );
    assert_eq!(out.stats.consts_emitted, 1);
    let c = &out.api.consts[0];
    assert_eq!(c.ty, scalar("float64"));
    assert_eq!(c.value, Some(FlConstValue::Int(42)));
    // A boolean const round-trips its literal too.
    let b = build_fluessig(
        &demo_report(vec![const_item("ON", Some("boolean"), Some("true"))]),
        &[],
    );
    assert_eq!(b.api.consts[0].ty, scalar("boolean"));
    assert_eq!(b.api.consts[0].value, Some(FlConstValue::Bool(true)));
}

/// An intrinsically-untyped const (`any`) has no fluessig type form: it is NOT
/// emitted as a bogus typed const, but counted honestly under
/// `dropped["const dropped (untyped)"]`.
#[test]
fn any_const_is_honestly_skipped_not_emitted() {
    let out = build_fluessig(
        &demo_report(vec![const_item("isBunBinary", Some("any"), None)]),
        &[],
    );
    assert_eq!(out.stats.consts_emitted, 0);
    assert!(out.api.consts.is_empty());
    assert_eq!(out.stats.dropped.get("const dropped (untyped)"), Some(&1));
    // No `consts` key is serialized when empty (byte-compat with pre-const shape).
    let json = serde_json::to_string(&out.api).unwrap();
    assert!(!json.contains("\"consts\""));
}

/// A const referencing an in-package `class` (no DTO form) is still EMITTED — the
/// declaration and its referenced type stay visible — lowering to whatever
/// `parse_type` yields (here honest `Json`), distinct from the untyped-`any` skip.
#[test]
fn class_typed_const_is_emitted_as_parse_type_yields() {
    let out = build_fluessig(
        &demo_report(vec![
            const_item("supervisor", Some("Supervisor"), Some("new Supervisor()")),
            ApiItem::new("class", "m#Supervisor", "Supervisor", "m"),
        ]),
        &[],
    );
    assert_eq!(out.stats.consts_emitted, 1);
    let c = out
        .api
        .consts
        .iter()
        .find(|c| c.name == "supervisor")
        .unwrap();
    assert_eq!(c.ty, FlType::json());
    assert_eq!(c.value, None);
    assert!(!out.stats.dropped.contains_key("const dropped (untyped)"));
}

/// A report with NO const serializes byte-identically to the pre-const shape:
/// the `consts` key is absent, and `consts_emitted` is 0.
#[test]
fn no_const_report_serializes_without_consts_key() {
    let items = vec![
        field_ref_model("Holder", "InstanceStatus"),
        union_alias_item("InstanceStatus", "\"online\" | \"offline\""),
    ];
    let out = build_fluessig(&demo_report(items), &[]);
    assert_eq!(out.stats.consts_emitted, 0);
    assert!(out.api.consts.is_empty());
    let json = serde_json::to_string(&out.api).unwrap();
    assert!(!json.contains("\"consts\""));
}
