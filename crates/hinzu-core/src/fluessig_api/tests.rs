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
    let dropped_const = ApiItem::new("const", "m#VERSION", "VERSION", "m");

    let report = demo_report(vec![iface, alias, func, class, method, dropped_const]);

    let out = build_fluessig(&report);

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

    // Coverage: const dropped, both ops clean.
    assert_eq!(out.stats.items_in, 6);
    assert_eq!(out.stats.ops_total, 2);
    assert_eq!(out.stats.ops_clean, 2);
    assert_eq!(out.stats.dropped.get("const dropped"), Some(&1));

    // The api.json serializes with the untagged FlType shape fluessig reads.
    let json = serde_json::to_string(&out.api).unwrap();
    assert!(json.contains("\"enum\":\"InstanceStatus\""));
    assert!(json.contains("\"model\":\"Summary\""));
    assert!(json.contains("\"async\":true"));
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

    let out = build_fluessig(&report);

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
