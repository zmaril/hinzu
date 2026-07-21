//! Unit tests for the fluessig converter: the rendered-TS-type-string parser
//! (the hard part) and the item-level mapping into `api.json`/`catalog.json`.

use super::*;
use crate::api::{
    build_api, ApiItem, Fidelity, Field, Module, PackageInfo, Param, Signature, Variant,
};

fn conv() -> Converter {
    Converter {
        known_enums: BTreeSet::new(),
        known_models: BTreeSet::new(),
        unions: BTreeMap::new(),
        reasons: BTreeMap::new(),
        notes: BTreeMap::new(),
    }
}

fn scalar(s: &str) -> FlType {
    FlType::Scalar(s.to_string())
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
    assert!(c.parse_type("{ a: string; b: number }").degraded);
    assert!(c.parse_type("RequestMap[keyof RequestMap]").degraded);
    assert!(c.parse_type("Record<string, number>").degraded);
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

    let report = build_api(
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
            items: vec![iface, alias, func, class, method, dropped_const],
        }],
    );

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
