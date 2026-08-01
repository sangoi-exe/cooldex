use super::support::apply_patch_spec;
use super::support::empty_object_schema;
use super::support::freeform_spec;
use super::support::function_spec;
use super::support::function_spec_with;
use super::support::namespace_spec;
use super::support::prost_struct_to_json;
use codex_cursor_agent_service::CursorMappingError;
use codex_cursor_agent_service::CursorToolSnapshot;
use codex_tools::AdditionalProperties;
use codex_tools::JsonSchema;
use codex_tools::ResponsesApiTool;
use codex_tools::ToolSpec;
use pretty_assertions::assert_eq;
use serde_json::json;
use std::collections::BTreeMap;

const DECLARED_TOOL_REJECTIONS: &[&str] = &[
    "deferred_function",
    "duplicate_identity",
    "duplicate_wire_name",
    "empty_name",
    "invalid_numeric_schema",
    "name_too_long",
    "noncanonical_freeform",
    "output_schema",
    "schema_too_large",
    "tool_search",
    "total_schema_too_large",
    "web_search",
    "description_too_long",
];

#[test]
fn maps_supported_tools_with_exact_identity_and_schema() {
    let specs = vec![
        function_spec("echo"),
        namespace_spec("math", "sum"),
        apply_patch_spec(),
    ];
    let snapshot = CursorToolSnapshot::from_specs(&specs).expect("supported tools should map");

    let definitions = snapshot.definitions();
    assert_eq!(definitions.len(), 3);
    assert_eq!(
        (
            definitions[0].name.as_str(),
            definitions[0].provider_identifier.as_str(),
            definitions[0].tool_name.as_str(),
        ),
        ("echo", "cooldex", "echo")
    );
    assert_eq!(
        (
            definitions[1].name.as_str(),
            definitions[1].provider_identifier.as_str(),
            definitions[1].tool_name.as_str(),
        ),
        ("math__sum", "math", "sum")
    );
    assert_eq!(
        (
            definitions[2].name.as_str(),
            definitions[2].provider_identifier.as_str(),
            definitions[2].tool_name.as_str(),
        ),
        ("apply_patch", "cooldex", "apply_patch")
    );

    for definition in definitions {
        let schema_json: serde_json::Value = serde_json::from_str(
            definition
                .input_schema_json
                .as_deref()
                .expect("mapped tool should carry canonical JSON schema"),
        )
        .expect("mapped schema JSON should parse");
        assert_eq!(
            prost_struct_to_json(
                definition
                    .input_schema
                    .as_ref()
                    .expect("mapped tool should carry protobuf schema"),
            ),
            schema_json
        );
    }

    assert_eq!(
        serde_json::from_str::<serde_json::Value>(
            definitions[2]
                .input_schema_json
                .as_deref()
                .expect("apply_patch should have a schema"),
        )
        .expect("apply_patch schema should parse"),
        json!({
            "type": "object",
            "properties": {"patch": {"type": "string"}},
            "required": ["patch"],
            "additionalProperties": false
        })
    );
}

#[test]
fn preserves_nested_enum_array_one_of_any_of_and_closed_object_schema() {
    let mut properties = BTreeMap::new();
    properties.insert(
        "mode".to_string(),
        JsonSchema::string_enum(vec![json!("fast"), json!("safe")], None),
    );
    properties.insert(
        "items".to_string(),
        JsonSchema::array(
            JsonSchema::one_of(
                vec![JsonSchema::string(None), JsonSchema::integer(None)],
                None,
            ),
            None,
        ),
    );
    properties.insert(
        "value".to_string(),
        JsonSchema::any_of(
            vec![JsonSchema::boolean(None), JsonSchema::null(None)],
            None,
        ),
    );
    let schema = JsonSchema::object(
        properties,
        Some(vec!["mode".to_string(), "items".to_string()]),
        Some(AdditionalProperties::Boolean(false)),
    );

    let snapshot =
        CursorToolSnapshot::from_specs(&[function_spec_with("complex", "Complex schema", schema)])
            .expect("representable schema should map");
    let mapped: serde_json::Value = serde_json::from_str(
        snapshot.definitions()[0]
            .input_schema_json
            .as_deref()
            .expect("schema JSON should be present"),
    )
    .expect("schema JSON should parse");

    assert_eq!(
        mapped["properties"]["mode"]["enum"],
        json!(["fast", "safe"])
    );
    assert_eq!(
        mapped["properties"]["items"]["items"]["oneOf"]
            .as_array()
            .map(Vec::len),
        Some(2)
    );
    assert_eq!(
        mapped["properties"]["value"]["anyOf"]
            .as_array()
            .map(Vec::len),
        Some(2)
    );
    assert_eq!(mapped["additionalProperties"], json!(false));
}

#[test]
fn rejects_every_declared_unsupported_tool_cell_in_isolation() {
    let mut tested = Vec::new();
    for case in DECLARED_TOOL_REJECTIONS {
        let error = match *case {
            "deferred_function" => {
                let mut spec = function_spec("deferred");
                let ToolSpec::Function(tool) = &mut spec else {
                    unreachable!();
                };
                tool.defer_loading = Some(true);
                CursorToolSnapshot::from_specs(&[spec]).expect_err("deferred tool should reject")
            }
            "duplicate_identity" => {
                CursorToolSnapshot::from_specs(&[function_spec("same"), function_spec("same")])
                    .expect_err("duplicate identity should reject")
            }
            "duplicate_wire_name" => CursorToolSnapshot::from_specs(&[
                function_spec("math__sum"),
                namespace_spec("math", "sum"),
            ])
            .expect_err("duplicate flattened name should reject"),
            "empty_name" => CursorToolSnapshot::from_specs(&[function_spec("")])
                .expect_err("empty tool name should reject"),
            "invalid_numeric_schema" => {
                let schema = JsonSchema {
                    enum_values: Some(vec![json!(9_007_199_254_740_993_u64)]),
                    ..Default::default()
                };
                CursorToolSnapshot::from_specs(&[function_spec_with(
                    "bad_number",
                    "Bad number",
                    schema,
                )])
                .expect_err("inexact protobuf number should reject")
            }
            "name_too_long" => CursorToolSnapshot::from_specs(&[function_spec(&"n".repeat(257))])
                .expect_err("oversized name should reject"),
            "noncanonical_freeform" => CursorToolSnapshot::from_specs(&[freeform_spec("exec")])
                .expect_err("noncanonical freeform should reject"),
            "output_schema" => {
                let mut spec = function_spec("output_schema");
                let ToolSpec::Function(tool) = &mut spec else {
                    unreachable!();
                };
                tool.output_schema = Some(json!({"type": "object"}));
                CursorToolSnapshot::from_specs(&[spec]).expect_err("output schema should reject")
            }
            "schema_too_large" => {
                let schema = JsonSchema::string(Some("s".repeat(1_048_576)));
                CursorToolSnapshot::from_specs(&[function_spec_with(
                    "large_schema",
                    "Large schema",
                    schema,
                )])
                .expect_err("oversized schema should reject")
            }
            "tool_search" => CursorToolSnapshot::from_specs(&[ToolSpec::ToolSearch {
                execution: "sync".to_string(),
                description: "Search".to_string(),
                parameters: empty_object_schema(),
            }])
            .expect_err("tool search should reject"),
            "total_schema_too_large" => {
                let specs = (0..10)
                    .map(|index| {
                        function_spec_with(
                            &format!("large_{index}"),
                            "Large aggregate",
                            JsonSchema::string(Some("t".repeat(900_000))),
                        )
                    })
                    .collect::<Vec<_>>();
                CursorToolSnapshot::from_specs(&specs)
                    .expect_err("aggregate schema cap should reject")
            }
            "web_search" => CursorToolSnapshot::from_specs(&[ToolSpec::WebSearch {
                external_web_access: None,
                indexed_web_access: None,
                filters: None,
                user_location: None,
                search_context_size: None,
                search_content_types: None,
            }])
            .expect_err("web search should reject"),
            "description_too_long" => CursorToolSnapshot::from_specs(&[function_spec_with(
                "verbose",
                &"d".repeat(16_385),
                empty_object_schema(),
            )])
            .expect_err("oversized description should reject"),
            unknown => panic!("undeclared tool rejection case: {unknown}"),
        };

        match *case {
            "deferred_function" => assert!(matches!(error, CursorMappingError::DeferredTool(_))),
            "duplicate_identity" => {
                assert!(matches!(
                    error,
                    CursorMappingError::DuplicateToolIdentity { .. }
                ))
            }
            "duplicate_wire_name" => {
                assert!(matches!(
                    error,
                    CursorMappingError::DuplicateWireToolName(_)
                ))
            }
            "empty_name" => assert_eq!(error, CursorMappingError::EmptyToolName),
            "invalid_numeric_schema" => {
                assert!(matches!(
                    error,
                    CursorMappingError::InvalidToolSchema { .. }
                ))
            }
            "name_too_long" => assert!(matches!(error, CursorMappingError::ToolNameTooLong(_))),
            "noncanonical_freeform" | "tool_search" | "web_search" => {
                assert!(matches!(error, CursorMappingError::UnsupportedTool(_)))
            }
            "output_schema" => {
                assert!(matches!(
                    error,
                    CursorMappingError::UnsupportedOutputSchema(_)
                ))
            }
            "schema_too_large" => {
                assert!(matches!(error, CursorMappingError::ToolSchemaTooLarge(_)))
            }
            "total_schema_too_large" => {
                assert_eq!(error, CursorMappingError::TotalToolSchemaTooLarge)
            }
            "description_too_long" => {
                assert!(matches!(
                    error,
                    CursorMappingError::ToolDescriptionTooLong(_)
                ))
            }
            unknown => panic!("undeclared tool rejection assertion: {unknown}"),
        }
        tested.push(*case);
    }

    assert_eq!(tested, DECLARED_TOOL_REJECTIONS);
}

#[test]
fn creates_an_empty_frozen_tool_snapshot() {
    let snapshot = CursorToolSnapshot::from_specs(&[]).expect("empty snapshot should be valid");

    assert!(snapshot.definitions().is_empty());
}

#[test]
fn normalizes_a_default_schema_to_an_explicit_closed_empty_object() {
    let snapshot = CursorToolSnapshot::from_specs(&[ToolSpec::Function(ResponsesApiTool {
        name: "empty".to_string(),
        description: "Empty".to_string(),
        strict: true,
        defer_loading: None,
        parameters: JsonSchema::default(),
        output_schema: None,
    })])
    .expect("default schema should normalize");

    assert_eq!(
        serde_json::from_str::<serde_json::Value>(
            snapshot.definitions()[0]
                .input_schema_json
                .as_deref()
                .expect("normalized schema should be present"),
        )
        .expect("normalized schema should parse"),
        json!({
            "type": "object",
            "properties": {},
            "required": [],
            "additionalProperties": false
        })
    );
}
