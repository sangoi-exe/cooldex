//! Preserves one bounded, complete tool-call/tool-output batch across remote V2 compaction.

use crate::context_manager::truncate_function_output_payload;
use crate::session::turn_context::TurnContext;
use crate::tool_batch::complete_trailing_tool_batch;
use codex_protocol::config_types::AutoCompactTokenLimitScope;
use codex_protocol::models::FunctionCallOutputPayload;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::TruncationPolicy;
use codex_utils_output_truncation::approx_token_count;
use tracing::debug;
use tracing::warn;

const POST_COMPACT_CONTINUITY_TAIL_MAX_TOKENS: usize = 6_000;
const POST_COMPACT_CONTINUITY_TAIL_MIN_AUTO_LIMIT_TOKENS: usize = 512;
const POST_COMPACT_CONTINUITY_TAIL_AUTO_LIMIT_DIVISOR: usize = 20;

#[cfg(test)]
#[path = "compact_continuity_tail_tests.rs"]
mod tests;

pub(crate) fn append_remote_v2_mid_turn_continuity_tail(
    new_history: &mut Vec<ResponseItem>,
    prompt_input: &[ResponseItem],
    turn_context: &TurnContext,
) {
    append_tail_with_budget(
        new_history,
        prompt_input,
        &turn_context.sub_id,
        continuity_tail_budget_tokens(turn_context),
    );
}

fn append_tail_with_budget(
    new_history: &mut Vec<ResponseItem>,
    prompt_input: &[ResponseItem],
    current_turn_id: &str,
    budget_tokens: usize,
) {
    if budget_tokens == 0 {
        log_omitted(current_turn_id, "zero_budget");
        return;
    }

    let Some(compaction_index) = new_history
        .iter()
        .rposition(|item| matches!(item, ResponseItem::Compaction { .. }))
    else {
        log_omitted(current_turn_id, "missing_compaction_anchor");
        return;
    };

    let candidate = match latest_complete_current_turn_tail(prompt_input, current_turn_id) {
        Ok(candidate) => candidate,
        Err(reason) => {
            log_omitted(current_turn_id, reason);
            return;
        }
    };
    let output_count = candidate.output_count;

    let budgeted = match apply_tail_budget(candidate, budget_tokens) {
        Ok(budgeted) => budgeted,
        Err(reason) => {
            log_omitted(current_turn_id, reason);
            return;
        }
    };

    let item_count = budgeted.items.len();
    let truncated_output_count = budgeted.truncated_output_count;
    let estimated_tokens = budgeted.estimated_tokens;
    new_history.splice(compaction_index + 1..compaction_index + 1, budgeted.items);
    debug!(
        turn_id = %current_turn_id,
        item_count,
        output_count,
        truncated_output_count,
        budget_tokens,
        estimated_tokens,
        "appended remote v2 post-compact continuity tail"
    );
}

fn continuity_tail_budget_tokens(turn_context: &TurnContext) -> usize {
    let effective_limit = match turn_context.config.model_auto_compact_token_limit_scope {
        AutoCompactTokenLimitScope::Total => turn_context.model_info.auto_compact_token_limit(),
        AutoCompactTokenLimitScope::BodyAfterPrefix => turn_context
            .config
            .model_auto_compact_token_limit
            .or_else(|| turn_context.model_info.auto_compact_token_limit()),
    };
    let auto_limit_cap = effective_limit
        .and_then(|limit| usize::try_from(limit).ok())
        .map(|limit| {
            (limit / POST_COMPACT_CONTINUITY_TAIL_AUTO_LIMIT_DIVISOR)
                .max(POST_COMPACT_CONTINUITY_TAIL_MIN_AUTO_LIMIT_TOKENS)
        });

    auto_limit_cap
        .unwrap_or(POST_COMPACT_CONTINUITY_TAIL_MAX_TOKENS)
        .min(POST_COMPACT_CONTINUITY_TAIL_MAX_TOKENS)
}

struct TailCandidate {
    items: Vec<ResponseItem>,
    output_count: usize,
}

struct BudgetedTail {
    items: Vec<ResponseItem>,
    truncated_output_count: usize,
    estimated_tokens: usize,
}

fn latest_complete_current_turn_tail(
    prompt_input: &[ResponseItem],
    current_turn_id: &str,
) -> Result<TailCandidate, &'static str> {
    let batch = complete_trailing_tool_batch(prompt_input)?;
    for item in &prompt_input[batch.range.clone()] {
        ensure_current_turn(item, current_turn_id)?;
    }

    Ok(TailCandidate {
        output_count: batch.range.end - batch.output_start,
        items: prompt_input[batch.range].to_vec(),
    })
}

fn ensure_current_turn(item: &ResponseItem, current_turn_id: &str) -> Result<(), &'static str> {
    if item.turn_id() == Some(current_turn_id) {
        Ok(())
    } else {
        Err("wrong_or_missing_turn_id")
    }
}

fn apply_tail_budget(
    candidate: TailCandidate,
    budget_tokens: usize,
) -> Result<BudgetedTail, &'static str> {
    let mut fixed_tokens = 0usize;
    let mut truncatable_output_indices = Vec::new();
    for (index, item) in candidate.items.iter().enumerate() {
        if truncatable_output_payload(item).is_some() {
            truncatable_output_indices.push(index);
        } else {
            fixed_tokens = fixed_tokens.saturating_add(estimate_item_tokens(item));
        }
    }

    if fixed_tokens > budget_tokens {
        return Err("fixed_tail_items_exceed_budget");
    }

    let remaining_output_budget = budget_tokens.saturating_sub(fixed_tokens);
    fit_truncatable_outputs_to_budget(
        &candidate.items,
        &truncatable_output_indices,
        remaining_output_budget,
        budget_tokens,
    )
    .ok_or("tail_exceeds_budget_after_truncation")
}

fn fit_truncatable_outputs_to_budget(
    original_items: &[ResponseItem],
    truncatable_output_indices: &[usize],
    remaining_output_budget: usize,
    budget_tokens: usize,
) -> Option<BudgetedTail> {
    if truncatable_output_indices.is_empty() {
        let estimated_tokens = estimate_items_tokens(original_items);
        return (estimated_tokens <= budget_tokens).then(|| BudgetedTail {
            items: original_items.to_vec(),
            truncated_output_count: 0,
            estimated_tokens,
        });
    }

    let mut lower = 0usize;
    let mut upper = remaining_output_budget / truncatable_output_indices.len();
    let mut best = None;
    while lower <= upper {
        let candidate_budget = lower + (upper - lower) / 2;
        let budgeted =
            apply_output_body_budget(original_items, truncatable_output_indices, candidate_budget);

        if budgeted.estimated_tokens <= budget_tokens {
            best = Some(budgeted);
            lower = candidate_budget.saturating_add(1);
        } else if candidate_budget == 0 {
            break;
        } else {
            upper = candidate_budget - 1;
        }
    }

    best
}

fn apply_output_body_budget(
    original_items: &[ResponseItem],
    truncatable_output_indices: &[usize],
    output_body_budget: usize,
) -> BudgetedTail {
    let mut items = original_items.to_vec();
    let mut truncated_output_count = 0usize;
    for &index in truncatable_output_indices {
        if estimate_item_tokens(&items[index]) <= output_body_budget {
            continue;
        }
        truncate_output_payload_at(&mut items[index], output_body_budget);
        truncated_output_count += 1;
    }
    let estimated_tokens = estimate_items_tokens(&items);
    BudgetedTail {
        items,
        truncated_output_count,
        estimated_tokens,
    }
}

fn truncatable_output_payload(item: &ResponseItem) -> Option<&FunctionCallOutputPayload> {
    match item {
        ResponseItem::FunctionCallOutput { output, .. }
        | ResponseItem::CustomToolCallOutput { output, .. } => Some(output),
        _ => None,
    }
}

fn truncate_output_payload_at(item: &mut ResponseItem, max_tokens: usize) {
    match item {
        ResponseItem::FunctionCallOutput { output, .. }
        | ResponseItem::CustomToolCallOutput { output, .. } => {
            *output =
                truncate_function_output_payload(output, TruncationPolicy::Tokens(max_tokens));
        }
        _ => {}
    }
}

fn estimate_items_tokens(items: &[ResponseItem]) -> usize {
    items
        .iter()
        .map(estimate_item_tokens)
        .fold(0usize, usize::saturating_add)
}

fn estimate_item_tokens(item: &ResponseItem) -> usize {
    match serde_json::to_string(item) {
        Ok(serialized) => approx_token_count(&serialized),
        Err(error) => {
            warn!(
                %error,
                "failed to serialize response item for remote v2 post-compact continuity-tail budget"
            );
            usize::MAX
        }
    }
}

fn log_omitted(current_turn_id: &str, reason: &'static str) {
    debug!(
        turn_id = %current_turn_id,
        reason,
        "omitted remote v2 post-compact continuity tail"
    );
}
