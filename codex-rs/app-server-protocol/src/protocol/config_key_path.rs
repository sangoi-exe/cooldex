// Merge-safety anchor: app-server config keyPath helpers must preserve literal
// segment boundaries across TUI callers and core config writes so dotted app ids
// do not silently retarget nested keys.

/// Encodes logical config path segments into the dotted `keyPath` wire format.
///
/// Literal `.` and `\` characters inside a segment are escaped with `\`.
pub fn join_config_key_path_segments<I, S>(segments: I) -> String
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    segments
        .into_iter()
        .map(|segment| escape_config_key_path_segment(segment.as_ref()))
        .collect::<Vec<_>>()
        .join(".")
}

/// Parses the dotted `keyPath` wire format into logical config path segments.
///
/// Literal `.` and `\` characters inside a segment must be escaped with `\`.
pub fn parse_config_key_path(path: &str) -> Result<Vec<String>, String> {
    if path.trim().is_empty() {
        return Err("keyPath must not be empty".to_string());
    }

    let mut segments = Vec::new();
    let mut current = String::new();
    let mut escaping = false;

    for character in path.chars() {
        if escaping {
            current.push(character);
            escaping = false;
            continue;
        }

        match character {
            '\\' => escaping = true,
            '.' => {
                segments.push(current);
                current = String::new();
            }
            _ => current.push(character),
        }
    }

    if escaping {
        return Err("keyPath must not end with an escape".to_string());
    }

    segments.push(current);
    Ok(segments)
}

fn escape_config_key_path_segment(segment: &str) -> String {
    let mut escaped = String::with_capacity(segment.len());
    for character in segment.chars() {
        match character {
            '\\' | '.' => {
                escaped.push('\\');
                escaped.push(character);
            }
            _ => escaped.push(character),
        }
    }
    escaped
}

#[cfg(test)]
mod tests {
    use super::join_config_key_path_segments;
    use super::parse_config_key_path;
    use pretty_assertions::assert_eq;

    #[test]
    fn join_config_key_path_segments_escapes_literal_dots_and_backslashes() {
        let path = join_config_key_path_segments(["apps", r"demo.app\beta", "enabled"]);

        assert_eq!(path, r"apps.demo\.app\\beta.enabled");
    }

    #[test]
    fn parse_config_key_path_preserves_literal_dots_and_backslashes() {
        let segments = parse_config_key_path(r"apps.demo\.app\\beta.enabled")
            .expect("escaped keyPath should parse");

        assert_eq!(
            segments,
            vec![
                "apps".to_string(),
                r"demo.app\beta".to_string(),
                "enabled".to_string(),
            ]
        );
    }

    #[test]
    fn parse_config_key_path_rejects_trailing_escape() {
        let err =
            parse_config_key_path("apps.demo\\").expect_err("trailing escape should fail loud");

        assert_eq!(err, "keyPath must not end with an escape".to_string());
    }

    #[test]
    fn parse_config_key_path_round_trips_joined_segments() {
        let expected = vec![
            "apps".to_string(),
            "demo.app".to_string(),
            r"path\segment".to_string(),
        ];
        let path = join_config_key_path_segments(expected.iter().map(String::as_str));

        let actual = parse_config_key_path(&path).expect("joined keyPath should parse");

        assert_eq!(actual, expected);
    }
}
