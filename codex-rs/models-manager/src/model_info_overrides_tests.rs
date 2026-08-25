use crate::ModelsManagerConfig;
use crate::manager::ModelsManager;
use crate::manager::construct_model_info_from_candidates;
use codex_protocol::openai_models::TruncationPolicyConfig;
use pretty_assertions::assert_eq;
use tempfile::TempDir;

use super::TestModelsEndpoint;
use super::openai_manager_for_tests;
use super::remote_model;

#[test]
fn gpt_5_6_family_forces_full_responses_after_catalog_resolution() {
    let mut sol = remote_model("gpt-5.6-sol", "GPT-5.6 Sol", /*priority*/ 10);
    sol.use_responses_lite = true;
    let mut terra = remote_model("gpt-5.6-terra", "GPT-5.6 Terra", /*priority*/ 9);
    terra.use_responses_lite = true;
    let mut luna = remote_model("gpt-5.6-luna", "GPT-5.6 Luna", /*priority*/ 8);
    luna.use_responses_lite = true;
    let mut unrelated = remote_model("gpt-5.6-codex", "GPT-5.6 Codex", /*priority*/ 7);
    unrelated.use_responses_lite = true;
    let candidates = [sol.clone(), terra.clone(), luna.clone(), unrelated.clone()];
    let config = ModelsManagerConfig::default();

    let actual = [
        construct_model_info_from_candidates("gpt-5.6-sol", &candidates, &config),
        construct_model_info_from_candidates("gpt-5.6-sol-2026-07-20", &candidates, &config),
        construct_model_info_from_candidates("openai/gpt-5.6-sol-2026-07-20", &candidates, &config),
        construct_model_info_from_candidates("gpt-5.6-terra", &candidates, &config),
        construct_model_info_from_candidates("gpt-5.6-terra-2026-07-20", &candidates, &config),
        construct_model_info_from_candidates(
            "openai/gpt-5.6-terra-2026-07-20",
            &candidates,
            &config,
        ),
        construct_model_info_from_candidates("gpt-5.6-luna", &candidates, &config),
        construct_model_info_from_candidates("gpt-5.6-luna-2026-07-20", &candidates, &config),
        construct_model_info_from_candidates(
            "openai/gpt-5.6-luna-2026-07-20",
            &candidates,
            &config,
        ),
        construct_model_info_from_candidates("gpt-5.6-codex-2026-07-20", &candidates, &config),
    ];

    let mut expected_sol = sol;
    expected_sol.use_responses_lite = false;
    let mut expected_sol_versioned = expected_sol.clone();
    expected_sol_versioned.slug = "gpt-5.6-sol-2026-07-20".to_string();
    let mut expected_sol_namespaced = expected_sol.clone();
    expected_sol_namespaced.slug = "openai/gpt-5.6-sol-2026-07-20".to_string();

    let mut expected_terra = terra;
    expected_terra.use_responses_lite = false;
    let mut expected_terra_versioned = expected_terra.clone();
    expected_terra_versioned.slug = "gpt-5.6-terra-2026-07-20".to_string();
    let mut expected_terra_namespaced = expected_terra.clone();
    expected_terra_namespaced.slug = "openai/gpt-5.6-terra-2026-07-20".to_string();

    let mut expected_luna = luna;
    expected_luna.use_responses_lite = false;
    let mut expected_luna_versioned = expected_luna.clone();
    expected_luna_versioned.slug = "gpt-5.6-luna-2026-07-20".to_string();
    let mut expected_luna_namespaced = expected_luna.clone();
    expected_luna_namespaced.slug = "openai/gpt-5.6-luna-2026-07-20".to_string();

    let mut expected_unrelated = unrelated;
    expected_unrelated.slug = "gpt-5.6-codex-2026-07-20".to_string();

    assert_eq!(
        actual,
        [
            expected_sol,
            expected_sol_versioned,
            expected_sol_namespaced,
            expected_terra,
            expected_terra_versioned,
            expected_terra_namespaced,
            expected_luna,
            expected_luna_versioned,
            expected_luna_namespaced,
            expected_unrelated,
        ]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn offline_model_info_without_tool_output_override() {
    let codex_home = TempDir::new().expect("create temp dir");
    let config = ModelsManagerConfig::default();
    let manager = openai_manager_for_tests(
        codex_home.path().to_path_buf(),
        TestModelsEndpoint::new(Vec::new()),
    );

    let model_info = manager.get_model_info("gpt-5.2", &config).await;

    assert_eq!(
        model_info.truncation_policy,
        TruncationPolicyConfig::bytes(/*limit*/ 10_000)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn offline_model_info_with_tool_output_override() {
    let codex_home = TempDir::new().expect("create temp dir");
    let config = ModelsManagerConfig {
        tool_output_token_limit: Some(123),
        ..Default::default()
    };
    let manager = openai_manager_for_tests(
        codex_home.path().to_path_buf(),
        TestModelsEndpoint::new(Vec::new()),
    );

    let model_info = manager.get_model_info("gpt-5.4", &config).await;

    assert_eq!(
        model_info.truncation_policy,
        TruncationPolicyConfig::tokens(/*limit*/ 123)
    );
}
