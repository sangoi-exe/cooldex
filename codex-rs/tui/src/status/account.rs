#[derive(Debug, Clone)]
pub(crate) enum StatusAccountDisplay {
    ChatGpt {
        label: Option<String>,
        email: Option<String>,
        plan: Option<String>,
    },
    ApiKey,
}
