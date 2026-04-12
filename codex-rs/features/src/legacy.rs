use crate::Feature;
use crate::Features;
use tracing::info;

#[derive(Clone, Copy)]
struct Alias {
    legacy_key: &'static str,
    feature: Feature,
}

const ALIASES: &[Alias] = &[
    Alias {
        legacy_key: "connectors",
        feature: Feature::Apps,
    },
    Alias {
        legacy_key: "enable_experimental_windows_sandbox",
        feature: Feature::WindowsSandbox,
    },
    Alias {
        legacy_key: "experimental_use_unified_exec_tool",
        feature: Feature::UnifiedExec,
    },
    Alias {
        legacy_key: "experimental_use_freeform_apply_patch",
        feature: Feature::ApplyPatchFreeform,
    },
    Alias {
        legacy_key: "include_apply_patch_tool",
        feature: Feature::ApplyPatchFreeform,
    },
    Alias {
        legacy_key: "request_permissions",
        feature: Feature::ExecPermissionApprovals,
    },
    Alias {
        legacy_key: "web_search",
        feature: Feature::WebSearchRequest,
    },
    Alias {
        legacy_key: "collab",
        feature: Feature::Collab,
    },
    Alias {
        legacy_key: "memory_tool",
        feature: Feature::MemoryTool,
    },
];

// Merge-safety anchor: legacy feature aliases here are the only place that may
// translate stale user-facing keys into active canon; removed keys must not stay
// accepted on config/CLI/app-server surfaces once the shipped owner moves on.
pub fn legacy_feature_keys() -> impl Iterator<Item = &'static str> {
    ALIASES.iter().map(|alias| alias.legacy_key)
}

pub fn user_toggle_legacy_feature_keys() -> impl Iterator<Item = &'static str> {
    ALIASES
        .iter()
        .copied()
        .filter(|alias| alias_accepts_user_toggle(*alias))
        .map(|alias| alias.legacy_key)
}

fn alias_for_key(key: &str) -> Option<Alias> {
    ALIASES
        .iter()
        .copied()
        .find(|alias| alias.legacy_key == key)
}

fn alias_accepts_user_toggle(alias: Alias) -> bool {
    alias.legacy_key != "collab" && crate::feature_accepts_user_toggle(alias.feature)
}

pub(crate) fn diagnostic_feature_for_key(key: &str) -> Option<Feature> {
    alias_for_key(key).map(|alias| alias.feature)
}

pub(crate) fn feature_for_key(key: &str) -> Option<Feature> {
    alias_for_key(key).map(|alias| {
        log_alias(alias.legacy_key, alias.feature);
        alias.feature
    })
}

pub(crate) fn user_toggle_feature_for_key(key: &str) -> Option<Feature> {
    alias_for_key(key)
        .filter(|alias| alias_accepts_user_toggle(*alias))
        .map(|alias| {
            log_alias(alias.legacy_key, alias.feature);
            alias.feature
        })
}

#[derive(Debug, Default)]
pub(crate) struct LegacyFeatureToggles {
    pub include_apply_patch_tool: Option<bool>,
    pub experimental_use_freeform_apply_patch: Option<bool>,
    pub experimental_use_unified_exec_tool: Option<bool>,
}

impl LegacyFeatureToggles {
    pub fn apply(self, features: &mut Features) {
        set_if_some(
            features,
            Feature::ApplyPatchFreeform,
            self.include_apply_patch_tool,
            "include_apply_patch_tool",
        );
        set_if_some(
            features,
            Feature::ApplyPatchFreeform,
            self.experimental_use_freeform_apply_patch,
            "experimental_use_freeform_apply_patch",
        );
        set_if_some(
            features,
            Feature::UnifiedExec,
            self.experimental_use_unified_exec_tool,
            "experimental_use_unified_exec_tool",
        );
    }
}

fn set_if_some(
    features: &mut Features,
    feature: Feature,
    maybe_value: Option<bool>,
    alias_key: &'static str,
) {
    if let Some(enabled) = maybe_value {
        set_feature(features, feature, enabled);
        log_alias(alias_key, feature);
        features.record_legacy_usage(alias_key, feature);
    }
}

fn set_feature(features: &mut Features, feature: Feature, enabled: bool) {
    if enabled {
        features.enable(feature);
    } else {
        features.disable(feature);
    }
}

fn log_alias(alias: &str, feature: Feature) {
    let canonical = feature.key();
    if alias == canonical {
        return;
    }
    info!(
        %alias,
        canonical,
        "legacy feature toggle detected; prefer `[features].{canonical}`"
    );
}
