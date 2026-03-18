use codex_protocol::models::FunctionCallOutputPayload;

const TOKEN_QTY_PREFIX: &str = "Token qty: ";
const LEGACY_TOKEN_QTY_PREFIX: &str = "Original token count: ";
const CHUNK_ID_PREFIX: &str = "Chunk ID: ";
const PROCESS_EXITED_PREFIX: &str = "Process exited with code ";
const PROCESS_RUNNING_PREFIX: &str = "Process running with session ID ";
const WALL_TIME_PREFIX: &str = "Wall time: ";
const OUTPUT_SECTION_HEADER: &str = "Output:";

// Merge-safety anchor: legacy LocalShellCall identity is `call_id.or(id)` across router,
// normalization, and compaction flows; drifting back to `call_id` only drops paired outputs.
pub(crate) fn local_shell_call_output_id(
    id: &Option<String>,
    call_id: &Option<String>,
) -> Option<String> {
    call_id.clone().or_else(|| id.clone())
}

// Merge-safety anchor: prompt_gc runtime activation scans `FunctionCallOutput` payload text for
// the canonical token-count header regardless of which function-like producer emitted it.
pub(crate) fn function_call_output_token_qty(output: &FunctionCallOutputPayload) -> Option<usize> {
    let text = output.text_content()?;
    unified_exec_token_qty_from_text(text)
}

pub(crate) fn is_unified_exec_token_qty_marker_line(line: &str) -> bool {
    line.starts_with(TOKEN_QTY_PREFIX) || line.starts_with(LEGACY_TOKEN_QTY_PREFIX)
}

pub(crate) fn is_unified_exec_output_frame(text: &str) -> bool {
    unified_exec_token_qty_from_text(text).is_some()
}

fn unified_exec_token_qty_from_text(text: &str) -> Option<usize> {
    let mut header_lines = text.lines();
    let mut current = header_lines.next()?;

    if current.starts_with(CHUNK_ID_PREFIX) {
        current = header_lines.next()?;
    }
    if !current.starts_with(WALL_TIME_PREFIX) {
        return None;
    }

    let mut current = header_lines.next()?;
    if current.starts_with(PROCESS_EXITED_PREFIX) {
        current = header_lines.next()?;
    }
    if current.starts_with(PROCESS_RUNNING_PREFIX) {
        current = header_lines.next()?;
    }

    let token_qty = parse_unified_exec_token_qty_line(current)?;
    (header_lines.next() == Some(OUTPUT_SECTION_HEADER)).then_some(token_qty)
}

fn parse_unified_exec_token_qty_line(line: &str) -> Option<usize> {
    [TOKEN_QTY_PREFIX, LEGACY_TOKEN_QTY_PREFIX]
        .into_iter()
        .find_map(|prefix| {
            let token_qty = line.strip_prefix(prefix)?.trim();
            token_qty.parse::<usize>().ok()
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_protocol::models::FunctionCallOutputBody;
    use pretty_assertions::assert_eq;

    #[test]
    fn local_shell_call_output_id_prefers_call_id() {
        assert_eq!(
            local_shell_call_output_id(
                &Some("legacy-id".to_string()),
                &Some("call-id".to_string()),
            ),
            Some("call-id".to_string())
        );
    }

    #[test]
    fn local_shell_call_output_id_falls_back_to_legacy_id() {
        assert_eq!(
            local_shell_call_output_id(&Some("legacy-id".to_string()), &None),
            Some("legacy-id".to_string())
        );
    }

    #[test]
    fn function_call_output_token_qty_parses_marker() {
        let output = FunctionCallOutputPayload {
            body: FunctionCallOutputBody::Text(
                "Wall time: 0.1000 seconds\nToken qty: 2798\nOutput:\nhello".to_string(),
            ),
            success: Some(true),
        };

        assert_eq!(function_call_output_token_qty(&output), Some(2798));
    }

    #[test]
    fn function_call_output_token_qty_parses_legacy_marker() {
        let output = FunctionCallOutputPayload {
            body: FunctionCallOutputBody::Text(
                "Wall time: 0.1000 seconds\nOriginal token count: 2798\nOutput:\nhello".to_string(),
            ),
            success: Some(true),
        };

        assert_eq!(function_call_output_token_qty(&output), Some(2798));
    }

    #[test]
    fn function_call_output_token_qty_ignores_missing_or_invalid_marker() {
        let missing = FunctionCallOutputPayload::from_text("Output:\nhello".to_string());
        let invalid = FunctionCallOutputPayload::from_text(
            "Wall time: 0.1000 seconds\nToken qty: nope\nOutput:\nhello".to_string(),
        );

        assert_eq!(function_call_output_token_qty(&missing), None);
        assert_eq!(function_call_output_token_qty(&invalid), None);
    }

    #[test]
    fn function_call_output_token_qty_ignores_marker_inside_output_body() {
        let output = FunctionCallOutputPayload::from_text(
            "Wall time: 0.1000 seconds\nOutput:\nToken qty: 500\nhello".into(),
        );

        assert_eq!(function_call_output_token_qty(&output), None);
    }

    #[test]
    fn function_call_output_token_qty_requires_full_unified_exec_header() {
        let missing_output = FunctionCallOutputPayload::from_text(
            "Wall time: 0.1000 seconds\nToken qty: 500".into(),
        );
        let missing_wall_time =
            FunctionCallOutputPayload::from_text("Token qty: 500\nOutput:\nhello".into());
        let extra_header = FunctionCallOutputPayload::from_text(
            "Wall time: 0.1000 seconds\nExit code: 0\nToken qty: 500\nOutput:\nhello".into(),
        );
        let out_of_order = FunctionCallOutputPayload::from_text(
            "Token qty: 500\nWall time: 0.1000 seconds\nOutput:\nhello".into(),
        );
        let leading_whitespace = FunctionCallOutputPayload::from_text(
            "Wall time: 0.1000 seconds\n Token qty: 500\nOutput:\nhello".into(),
        );

        assert_eq!(function_call_output_token_qty(&missing_output), None);
        assert_eq!(function_call_output_token_qty(&missing_wall_time), None);
        assert_eq!(function_call_output_token_qty(&extra_header), None);
        assert_eq!(function_call_output_token_qty(&out_of_order), None);
        assert_eq!(function_call_output_token_qty(&leading_whitespace), None);
    }

    #[test]
    fn function_call_output_token_qty_accepts_marker_without_call_name_context() {
        let output = FunctionCallOutputPayload::from_text(
            "Wall time: 0.1000 seconds\nToken qty: 500\nOutput:\nhello".into(),
        );

        assert_eq!(function_call_output_token_qty(&output), Some(500));
    }

    #[test]
    fn is_unified_exec_output_frame_requires_full_header_shape() {
        assert!(is_unified_exec_output_frame(
            "Wall time: 0.1000 seconds\nToken qty: 500\nOutput:\nhello"
        ));
        assert!(!is_unified_exec_output_frame(
            "Token qty: 500\nOutput:\nhello"
        ));
    }

    #[test]
    fn is_unified_exec_token_qty_marker_line_accepts_current_and_legacy_markers() {
        assert!(is_unified_exec_token_qty_marker_line("Token qty: 123"));
        assert!(is_unified_exec_token_qty_marker_line(
            "Original token count: 123"
        ));
        assert!(!is_unified_exec_token_qty_marker_line("Output:"));
    }
}
