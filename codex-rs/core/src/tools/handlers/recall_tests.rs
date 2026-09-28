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
            r#"{"intervals":3}"#,
            RecallIntervals::Count(NonZeroUsize::new(/*n*/ 3).unwrap()),
        ),
        (r#"{"intervals":"all"}"#, RecallIntervals::All(AllIntervals::All)),
    ] {
        let args: RecallArgs = parse_arguments(arguments).expect("valid recall arguments");
        assert_eq!(args.intervals, expected);
    }
}

#[test]
fn invalid_intervals_and_unknown_fields_fail_the_strict_argument_boundary() {
    for arguments in [
        r#"{"intervals":0}"#,
        r#"{"intervals":-1}"#,
        r#"{"intervals":1.5}"#,
        r#"{"intervals":"one"}"#,
        r#"{"intervals":null}"#,
        r#"{"intervals":1,"unknown":true}"#,
        "null",
    ] {
        assert!(
            matches!(
                parse_arguments::<RecallArgs>(arguments),
                Err(FunctionCallError::RespondToModel(_))
            ),
            "invalid arguments must fail: {arguments}"
        );
    }
}
