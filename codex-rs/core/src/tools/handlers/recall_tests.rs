use std::num::NonZeroUsize;

use pretty_assertions::assert_eq;

use super::RecallArgs;
use crate::function_tool::FunctionCallError;
use crate::session::recall::AllIntervals;
use crate::session::recall::RecallIntervals;
use crate::tools::handlers::parse_arguments;

#[test]
fn arguments_default_to_one_and_accept_a_positive_count_or_all() {
    for (arguments, expected) in [
        ("{}", RecallIntervals::Count(NonZeroUsize::MIN)),
        (
            r#"{"intervals":1}"#,
            RecallIntervals::Count(NonZeroUsize::MIN),
        ),
        (
            r#"{"intervals":3}"#,
            RecallIntervals::Count(NonZeroUsize::new(/*n*/ 3).unwrap()),
        ),
        (
            r#"{"intervals":"all"}"#,
            RecallIntervals::All(AllIntervals::All),
        ),
    ] {
        assert_eq!(
            parse_arguments(arguments),
            Ok(RecallArgs {
                intervals: expected
            })
        );
    }
}

#[test]
fn invalid_interval_representations_fail_the_strict_argument_boundary() {
    for arguments in [
        r#"{"intervals":0}"#,
        r#"{"intervals":-1}"#,
        r#"{"intervals":1.5}"#,
        r#"{"intervals":"one"}"#,
        r#"{"intervals":"1"}"#,
        r#"{"intervals":"ALL"}"#,
        r#"{"intervals":null}"#,
        r#"{"intervals":true}"#,
        r#"{"intervals":false}"#,
        r#"{"intervals":[]}"#,
        r#"{"intervals":[1]}"#,
        r#"{"intervals":["all"]}"#,
        r#"{"intervals":{}}"#,
        r#"{"intervals":{"all":null}}"#,
    ] {
        let Some(error) = serde_json::from_str::<RecallArgs>(arguments).err() else {
            panic!("invalid intervals must fail: {arguments}");
        };
        assert_eq!(
            parse_arguments::<RecallArgs>(arguments).err(),
            Some(FunctionCallError::RespondToModel(format!(
                "failed to parse function arguments: {error}"
            )))
        );
    }
}

#[test]
fn non_objects_duplicates_and_unknown_fields_fail_the_strict_argument_boundary() {
    for arguments in [
        "[]",
        "[1]",
        r#"["all"]"#,
        "[{}]",
        "null",
        "1",
        r#""all""#,
        "true",
        r#"{"intervals":1,"intervals":1}"#,
        r#"{"intervals":1,"intervals":"all"}"#,
        r#"{"unknown":true}"#,
        r#"{"intervals":1,"unknown":true}"#,
    ] {
        let Some(error) = serde_json::from_str::<RecallArgs>(arguments).err() else {
            panic!("invalid arguments must fail: {arguments}");
        };
        assert_eq!(
            parse_arguments::<RecallArgs>(arguments).err(),
            Some(FunctionCallError::RespondToModel(format!(
                "failed to parse function arguments: {error}"
            )))
        );
    }
}
