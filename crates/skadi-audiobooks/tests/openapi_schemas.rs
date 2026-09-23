//! The audiobooks module contributes real schemas to the OpenAPI document
//! (SKADI-T-0546).
//!
//! `skadi-api` sits below this crate, so its own tests can only use a stand-in
//! type. This is the end-to-end check: the schemas the module hands up are the
//! ones the published document references.

use skadi_api::openapi;

#[test]
fn the_module_schemas_make_the_audiobook_responses_describable() {
    // `AudiobooksHttp::schemas` needs no state — it only names types — so the
    // contribution can be checked without standing up a store or a runner.
    let schemas = audiobook_schemas();
    let names: Vec<&str> = schemas.iter().map(|(n, _)| n.as_str()).collect();
    for want in ["ChapterDto", "WatcherDto", "SeriesRollupDto"] {
        assert!(names.contains(&want), "{want} missing from {names:?}");
    }

    let doc = openapi::document(&schemas);

    // With the module mounted, the described operations point at real schemas.
    let chapters = &doc["paths"]["/books/{id}/files/{fid}/chapters"]["get"]["responses"]["200"]["content"]
        ["application/json"]["schema"]["$ref"];
    assert_eq!(chapters, "#/components/schemas/ChapterDto");

    let registered = doc["components"]["schemas"].as_object().unwrap();
    assert!(registered.contains_key("ChapterDto"));
    // The shared envelope survives the merge.
    assert!(registered.contains_key("Error"));

    // The schema is derived, so it carries the DTO's actual fields rather than
    // an empty object.
    assert!(
        registered["ChapterDto"]["properties"]
            .as_object()
            .is_some_and(|p| !p.is_empty()),
        "ChapterDto has no properties — the derive produced nothing useful"
    );
}

/// Without the module, the same operation must fall back to a bare 200 rather
/// than publish a `$ref` to a schema nobody registered.
#[test]
fn an_unmounted_domain_leaves_no_dangling_ref() {
    let doc = openapi::document(&Vec::new());
    assert!(
        doc["paths"]["/books/{id}/files/{fid}/chapters"]["get"]["responses"]["200"]
            .get("content")
            .is_none()
    );
}

fn audiobook_schemas() -> openapi::Schemas {
    skadi_audiobooks::http::audiobook_response_schemas()
}

/// `Book` is the domain model, not a DTO, so its schema drags in every nested
/// type — ids, `ExternalIds`, `RootFolder`, `AcquisitionStatus`, `MediaInfo`,
/// `BookFile`. A missing derive anywhere in that graph shows up as a `$ref` to a
/// definition nobody emitted, which is exactly what this catches (SKADI-T-0548).
#[test]
fn the_book_schema_and_everything_it_references_resolve() {
    let doc = openapi::document(&audiobook_schemas());
    let schemas = doc["components"]["schemas"].as_object().unwrap();

    assert!(schemas.contains_key("Book"), "Book is not registered");
    assert_eq!(
        doc["paths"]["/books"]["get"]["responses"]["200"]["content"]["application/json"]["schema"]
            ["$ref"],
        "#/components/schemas/Book"
    );

    // Every `$ref` anywhere in the document must name a registered schema.
    fn refs(v: &serde_json::Value, out: &mut Vec<String>) {
        match v {
            serde_json::Value::Object(map) => {
                for (k, val) in map {
                    if k == "$ref"
                        && let Some(r) = val.as_str()
                    {
                        out.push(r.to_string());
                    }
                    refs(val, out);
                }
            }
            serde_json::Value::Array(items) => items.iter().for_each(|i| refs(i, out)),
            _ => {}
        }
    }
    let mut found = Vec::new();
    refs(&doc, &mut found);
    for r in &found {
        let name = r.strip_prefix("#/components/schemas/").unwrap();
        assert!(
            schemas.contains_key(name),
            "$ref points at {name}, which nothing registered — a nested type is \
             missing its JsonSchema derive"
        );
    }

    // The schema describes the real fields, so a rename in the domain model
    // changes the published document rather than silently changing the API.
    let props = schemas["Book"]["properties"].as_object().unwrap();
    for field in ["id", "title", "monitored", "files", "root_folder"] {
        assert!(props.contains_key(field), "Book schema is missing {field}");
    }
}
