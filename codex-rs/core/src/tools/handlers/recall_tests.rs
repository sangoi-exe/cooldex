use super::RecallArgs;

#[test]
fn arguments_accept_only_an_empty_object() {
    assert!(serde_json::from_str::<RecallArgs>("{}").is_ok());
    assert!(serde_json::from_str::<RecallArgs>(r#"{"query":"history"}"#).is_err());
    assert!(serde_json::from_str::<RecallArgs>("null").is_err());
}
