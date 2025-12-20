use crate::codex::Session;
use crate::function_tool::FunctionCallError;
use crate::protocol::ContextInclusionItem;
use crate::protocol::ContextOverlayItem;
use crate::protocol::ContextOverlayReplacement;
use crate::protocol::RolloutItem;
use crate::rid::parse_rid;
use crate::rid::rid_to_string;
use crate::state::ContextOverlay;
use crate::tools::context::ToolInvocation;
use crate::tools::context::ToolOutput;
use crate::tools::context::ToolPayload;
use crate::tools::registry::ToolHandler;
use crate::tools::registry::ToolKind;
use async_trait::async_trait;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ReasoningItemContent;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::TokenUsageInfo;
use serde::Deserialize;
use serde_json::json;
use sha1::Digest;
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::collections::HashMap;
use std::collections::HashSet;

const MAX_TOP_ITEMS: usize = 10;

pub struct ManageContextHandler;

#[derive(Debug, Deserialize)]
struct ManageContextToolArgs {
    // v2: non-interactive retrieve/apply
    #[serde(default)]
    mode: Option<String>,
    #[serde(default)]
    include_items: Option<bool>,
    #[serde(default)]
    include_notes: Option<bool>,
    #[serde(default)]
    include_token_usage: Option<bool>,
    #[serde(default)]
    include_pairs: Option<bool>,
    #[serde(default)]
    max_items: Option<usize>,

    #[serde(default)]
    snapshot_id: Option<String>,
    #[serde(default)]
    ops: Vec<ManageContextOp>,

    // v1: interactive action-based API
    #[serde(default)]
    action: Option<String>,
    #[serde(default)]
    ids: Vec<String>,
    #[serde(default)]
    indices: Vec<usize>,
    #[serde(default)]
    call_ids: Vec<String>,
    #[serde(default)]
    replacements: Vec<ManageContextReplacement>,
    #[serde(default)]
    notes: Vec<String>,
    #[serde(default)]
    note_indices: Vec<usize>,
    #[serde(default)]
    dry_run: bool,
}

#[derive(Debug, Deserialize)]
struct ManageContextReplacement {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    index: Option<usize>,
    #[serde(default)]
    call_id: Option<String>,
    text: String,
}

#[derive(Debug, Deserialize)]
struct ManageContextOp {
    op: String,
    #[serde(default)]
    targets: Option<ManageContextTargets>,
    #[serde(default)]
    cascade: Option<String>,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    notes: Vec<String>,
    #[serde(default)]
    note_indices: Vec<usize>,
}

#[derive(Debug, Default, Deserialize)]
struct ManageContextTargets {
    #[serde(default)]
    ids: Vec<String>,
    #[serde(default)]
    indices: Vec<usize>,
    #[serde(default)]
    call_ids: Vec<String>,
}

#[async_trait]
impl ToolHandler for ManageContextHandler {
    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }

    async fn handle(&self, invocation: ToolInvocation) -> Result<ToolOutput, FunctionCallError> {
        let ToolInvocation {
            session, payload, ..
        } = invocation;

        let ToolPayload::Function { arguments } = payload else {
            return Err(FunctionCallError::RespondToModel(
                "manage_context handler received unsupported payload".to_string(),
            ));
        };

        let args: ManageContextToolArgs = serde_json::from_str(&arguments).map_err(|e| {
            FunctionCallError::RespondToModel(format!("failed to parse function arguments: {e:?}"))
        })?;

        let result = handle_manage_context(session.as_ref(), &args).await?;

        Ok(ToolOutput::Function {
            content: serde_json::to_string(&result.json).unwrap_or_else(|_| "{}".to_string()),
            success: Some(true),
        })
    }
}

struct ManageContextResult {
    json: serde_json::Value,
}

async fn handle_manage_context(
    session: &Session,
    args: &ManageContextToolArgs,
) -> Result<ManageContextResult, FunctionCallError> {
    if let Some(mode) = args.mode.as_deref() {
        return match mode {
            "retrieve" => handle_retrieve(session, args).await,
            "apply" => handle_apply(session, args).await,
            _ => Err(FunctionCallError::RespondToModel(format!(
                "unknown manage_context mode: {mode}"
            ))),
        };
    }

    let Some(action) = args.action.as_deref() else {
        return Err(FunctionCallError::RespondToModel(
            "manage_context requires either mode (v2) or action (v1)".to_string(),
        ));
    };

    match action {
        "help" => Ok(ManageContextResult {
            json: json!({
                "action": "help",
                "summary": [
                    "manage_context is an internal tool for the agent to keep long sessions healthy without /compact.",
                    "Preferred flow: mode=retrieve (snapshot) then mode=apply (atomic ops) using snapshot_id (anti-drift)."
                ],
                "rules": [
                    "replace is allowed ONLY for ToolOutput and Reasoning (never user/assistant messages).",
                    "delete is destructive; deleting a tool call also deletes its outputs.",
                    "If snapshot_id mismatches, re-run retrieve and retry apply."
                ],
                "tip": "Use retrieve(include_items=false) to get the bounded breakdown + top offenders without bloating context.",
                "example_retrieve": {
                    "mode": "retrieve",
                    "include_items": false,
                    "include_token_usage": true,
                },
                "example_apply": {
                    "mode": "apply",
                    "snapshot_id": "<from retrieve>",
                    "ops": [
                        {"op":"replace","targets":{"call_ids":["call_123"]},"text":"Key results: ..."},
                        {"op":"exclude","targets":{"indices":[0,1,2]}},
                        {"op":"add_note","notes":["Decision: ...","Constraint: ..."]}
                    ]
                }
            }),
        }),
        "status" => {
            let (token_info, overlay, history_len, included_len, summaries, items) = {
                let state = session.state.lock().await;
                let token_info = state.token_info.clone();
                let overlay = state.context_overlay_snapshot();
                let history_len = state.history_snapshot().len();
                let ev = state.build_context_items_event();
                let included_len = ev.items.iter().filter(|it| it.included).count();
                let items = state.history_snapshot();
                (
                    token_info,
                    overlay,
                    history_len,
                    included_len,
                    ev.items,
                    items,
                )
            };

            let (context_window, context_left_percent, tokens_in_context) =
                token_window_summary(token_info.as_ref());

            let breakdown = build_breakdown(&summaries, &items, &overlay, MAX_TOP_ITEMS);

            Ok(ManageContextResult {
                json: json!({
                    "action": "status",
                    "model_context_window": context_window,
                    "tokens_in_context": tokens_in_context,
                    "context_left_percent": context_left_percent,
                    "history_len": history_len,
                    "included_len": included_len,
                    "replacements": overlay.replacements_by_rid.len(),
                    "notes": overlay.notes.len(),
                    "breakdown": breakdown,
                }),
            })
        }
        "list" => {
            let (summaries, items, overlay) = {
                let state = session.state.lock().await;
                let ev = state.build_context_items_event();
                (
                    ev.items,
                    state.history_snapshot(),
                    state.context_overlay_snapshot(),
                )
            };

            let mut out = Vec::with_capacity(summaries.len());
            for summary in summaries {
                let item = items.get(summary.index);
                let (call_id, tool_name) = match item {
                    Some(ResponseItem::FunctionCall { call_id, name, .. }) => {
                        (Some(call_id.clone()), Some(name.clone()))
                    }
                    Some(ResponseItem::FunctionCallOutput { call_id, .. }) => {
                        (Some(call_id.clone()), None)
                    }
                    Some(ResponseItem::CustomToolCall { call_id, name, .. }) => {
                        (Some(call_id.clone()), Some(name.clone()))
                    }
                    Some(ResponseItem::CustomToolCallOutput { call_id, .. }) => {
                        (Some(call_id.clone()), None)
                    }
                    Some(ResponseItem::LocalShellCall {
                        call_id: Some(call_id),
                        ..
                    }) => (Some(call_id.clone()), Some("local_shell".to_string())),
                    _ => (None, None),
                };

                let rid = summary.id.as_ref().and_then(|id| parse_rid(id));
                let replacement = rid.and_then(|rid| overlay.replacements_by_rid.get(&rid));

                out.push(json!({
                    "index": summary.index,
                    "id": summary.id,
                    "category": summary.category,
                    "included": summary.included,
                    "preview": summary.preview,
                    "call_id": call_id,
                    "tool_name": tool_name,
                    "replaced": replacement.is_some(),
                    "effective_preview": replacement.map(|text| preview_text(text)),
                }));
            }

            Ok(ManageContextResult {
                json: json!({
                    "action": "list",
                    "items": out,
                    "notes": overlay.notes,
                }),
            })
        }
        "include" | "exclude" => {
            if args.dry_run {
                let target_indices = resolve_target_indices(session, args).await?;
                return Ok(ManageContextResult {
                    json: json!({
                        "action": action,
                        "dry_run": true,
                        "indices": target_indices,
                    }),
                });
            }

            let included = action == "include";
            let (context_items_event, included_indices, included_ids) = {
                let mut state = session.state.lock().await;
                let indices = resolve_target_indices_locked(&state, args);
                state.set_context_inclusion(&indices, included);
                let ev = state.build_context_items_event();
                let (included_indices, included_ids) = included_snapshot(&ev);
                (ev, included_indices, included_ids)
            };

            session
                .persist_rollout_items(std::slice::from_ref(&RolloutItem::ContextInclusion(
                    ContextInclusionItem {
                        included_indices,
                        deleted_indices: Vec::new(),
                        included_ids,
                        deleted_ids: Vec::new(),
                    },
                )))
                .await;

            Ok(ManageContextResult {
                json: json!({
                    "action": action,
                    "ok": true,
                    "total": context_items_event.total,
                }),
            })
        }
        "delete" => {
            if args.dry_run {
                let target_indices = resolve_target_indices(session, args).await?;
                return Ok(ManageContextResult {
                    json: json!({
                        "action": "delete",
                        "dry_run": true,
                        "indices": target_indices,
                    }),
                });
            }

            let (context_items_event, included_indices, included_ids, deleted_indices, deleted_ids) = {
                let mut state = session.state.lock().await;
                let indices = resolve_target_indices_locked(&state, args);
                let prune = state.prune_by_indices(indices);
                let ev = state.build_context_items_event();
                let (included_indices, included_ids) = included_snapshot(&ev);
                let deleted_ids = prune
                    .deleted_rids
                    .iter()
                    .copied()
                    .map(rid_to_string)
                    .collect::<Vec<String>>();
                (
                    ev,
                    included_indices,
                    included_ids,
                    prune.deleted_indices,
                    deleted_ids,
                )
            };

            session
                .persist_rollout_items(std::slice::from_ref(&RolloutItem::ContextInclusion(
                    ContextInclusionItem {
                        included_indices,
                        deleted_indices,
                        included_ids,
                        deleted_ids,
                    },
                )))
                .await;

            Ok(ManageContextResult {
                json: json!({
                    "action": "delete",
                    "ok": true,
                    "total": context_items_event.total,
                }),
            })
        }
        "replace" => {
            if args.replacements.is_empty() {
                return Err(FunctionCallError::RespondToModel(
                    "manage_context.replace requires non-empty replacements".to_string(),
                ));
            }

            if args.dry_run {
                let target_indices =
                    resolve_replacement_targets(session, &args.replacements).await?;
                return Ok(ManageContextResult {
                    json: json!({
                        "action": "replace",
                        "dry_run": true,
                        "targets": target_indices,
                    }),
                });
            }

            let overlay_item = {
                let mut state = session.state.lock().await;
                let items = state.history_snapshot();
                let rids = state.history_rids_snapshot();

                let mut updates: Vec<(u64, String)> = Vec::new();
                for replacement in &args.replacements {
                    let indices = resolve_replacement_target_indices(&items, &rids, replacement)?;
                    for idx in indices {
                        let Some(item) = items.get(idx) else {
                            continue;
                        };
                        if !matches!(
                            item,
                            ResponseItem::FunctionCallOutput { .. }
                                | ResponseItem::CustomToolCallOutput { .. }
                                | ResponseItem::Reasoning { .. }
                        ) {
                            return Err(FunctionCallError::RespondToModel(format!(
                                "replace only supports tool outputs and reasoning (index={idx})"
                            )));
                        }
                        let Some(rid) = rids.get(idx).copied() else {
                            continue;
                        };
                        updates.push((rid, replacement.text.clone()));
                    }
                }

                state.upsert_context_replacements(updates);
                context_overlay_rollout_item(&state.context_overlay_snapshot())
            };

            session
                .persist_rollout_items(std::slice::from_ref(&overlay_item))
                .await;

            Ok(ManageContextResult {
                json: json!({
                    "action": "replace",
                    "ok": true,
                }),
            })
        }
        "clear_replace" => {
            if args.dry_run {
                let target_indices = resolve_target_indices(session, args).await?;
                return Ok(ManageContextResult {
                    json: json!({
                        "action": "clear_replace",
                        "dry_run": true,
                        "indices": target_indices,
                    }),
                });
            }

            let overlay_item = {
                let mut state = session.state.lock().await;
                let indices = resolve_target_indices_locked(&state, args);
                if indices.is_empty() && args.ids.is_empty() && args.call_ids.is_empty() {
                    state.clear_context_replacements();
                } else {
                    let rids = state.history_rids_snapshot();
                    let mut to_clear: Vec<u64> = Vec::new();
                    for idx in indices {
                        if let Some(rid) = rids.get(idx).copied() {
                            to_clear.push(rid);
                        }
                    }
                    state.clear_context_replacements_for(&to_clear);
                }
                context_overlay_rollout_item(&state.context_overlay_snapshot())
            };

            session
                .persist_rollout_items(std::slice::from_ref(&overlay_item))
                .await;

            Ok(ManageContextResult {
                json: json!({
                    "action": "clear_replace",
                    "ok": true,
                }),
            })
        }
        "add_note" => {
            if args.notes.is_empty() {
                return Err(FunctionCallError::RespondToModel(
                    "manage_context.add_note requires non-empty notes".to_string(),
                ));
            }

            if args.dry_run {
                return Ok(ManageContextResult {
                    json: json!({
                        "action": "add_note",
                        "dry_run": true,
                        "notes": args.notes,
                    }),
                });
            }

            let overlay_item = {
                let mut state = session.state.lock().await;
                state.add_context_notes(args.notes.clone());
                context_overlay_rollout_item(&state.context_overlay_snapshot())
            };

            session
                .persist_rollout_items(std::slice::from_ref(&overlay_item))
                .await;

            Ok(ManageContextResult {
                json: json!({
                    "action": "add_note",
                    "ok": true,
                }),
            })
        }
        "remove_note" => {
            if args.note_indices.is_empty() {
                return Err(FunctionCallError::RespondToModel(
                    "manage_context.remove_note requires note_indices".to_string(),
                ));
            }

            if args.dry_run {
                return Ok(ManageContextResult {
                    json: json!({
                        "action": "remove_note",
                        "dry_run": true,
                        "note_indices": args.note_indices,
                    }),
                });
            }

            let overlay_item = {
                let mut state = session.state.lock().await;
                state.remove_context_notes(&args.note_indices);
                context_overlay_rollout_item(&state.context_overlay_snapshot())
            };

            session
                .persist_rollout_items(std::slice::from_ref(&overlay_item))
                .await;

            Ok(ManageContextResult {
                json: json!({
                    "action": "remove_note",
                    "ok": true,
                }),
            })
        }
        "clear_notes" => {
            if args.dry_run {
                return Ok(ManageContextResult {
                    json: json!({
                        "action": "clear_notes",
                        "dry_run": true,
                    }),
                });
            }

            let overlay_item = {
                let mut state = session.state.lock().await;
                state.clear_context_notes();
                context_overlay_rollout_item(&state.context_overlay_snapshot())
            };

            session
                .persist_rollout_items(std::slice::from_ref(&overlay_item))
                .await;

            Ok(ManageContextResult {
                json: json!({
                    "action": "clear_notes",
                    "ok": true,
                }),
            })
        }
        "include_all" => {
            if args.dry_run {
                return Ok(ManageContextResult {
                    json: json!({
                        "action": "include_all",
                        "dry_run": true,
                    }),
                });
            }

            let (context_items_event, included_indices, included_ids) = {
                let mut state = session.state.lock().await;
                state.set_include_mask(None);
                let ev = state.build_context_items_event();
                let (included_indices, included_ids) = included_snapshot(&ev);
                (ev, included_indices, included_ids)
            };

            session
                .persist_rollout_items(std::slice::from_ref(&RolloutItem::ContextInclusion(
                    ContextInclusionItem {
                        included_indices,
                        deleted_indices: Vec::new(),
                        included_ids,
                        deleted_ids: Vec::new(),
                    },
                )))
                .await;

            Ok(ManageContextResult {
                json: json!({
                    "action": "include_all",
                    "ok": true,
                    "total": context_items_event.total,
                }),
            })
        }
        _ => Err(FunctionCallError::RespondToModel(format!(
            "unknown manage_context action: {action}"
        ))),
    }
}

async fn handle_retrieve(
    session: &Session,
    args: &ManageContextToolArgs,
) -> Result<ManageContextResult, FunctionCallError> {
    let include_items = args.include_items.unwrap_or(true);
    let include_notes = args.include_notes.unwrap_or(true);
    let include_token_usage = args.include_token_usage.unwrap_or(true);
    let include_pairs = args.include_pairs.unwrap_or(true);

    let (token_info, overlay, summaries, items, snapshot_id) = {
        let state = session.state.lock().await;
        let token_info = state.token_info.clone();
        let overlay = state.context_overlay_snapshot();
        let ev = state.build_context_items_event();
        let items = state.history_snapshot();
        let snapshot_id = snapshot_id_for_context(&ev.items, &overlay);
        (token_info, overlay, ev.items, items, snapshot_id)
    };

    let max_items = args.max_items.unwrap_or(summaries.len());
    let slice_start = summaries.len().saturating_sub(max_items);

    let breakdown = build_breakdown(&summaries, &items, &overlay, MAX_TOP_ITEMS);

    let mut out_items = Vec::new();
    if include_items {
        out_items.reserve(summaries.len().saturating_sub(slice_start));
        for summary in summaries.iter().skip(slice_start) {
            let item = items.get(summary.index);
            let (call_id, tool_name, pair) = describe_pair(item, include_pairs);

            let rid = summary.id.as_ref().and_then(|id| parse_rid(id));
            let replacement = rid.and_then(|rid| overlay.replacements_by_rid.get(&rid));
            let raw_bytes = item.map(estimate_item_bytes).unwrap_or(0);
            let effective_bytes = replacement.map(|t| t.len() as u64).unwrap_or(raw_bytes);

            out_items.push(json!({
                "index": summary.index,
                "id": summary.id,
                "category": summary.category,
                "included": summary.included,
                "preview": summary.preview,
                "call_id": call_id,
                "tool_name": tool_name,
                "pair": pair,
                "replaced": replacement.is_some(),
                "effective_preview": replacement.map(|text| preview_text(text)),
                "approx_bytes": {
                    "raw": raw_bytes,
                    "effective": effective_bytes,
                },
            }));
        }
    }

    let token_usage = if include_token_usage {
        let (context_window, context_left_percent, tokens_in_context) =
            token_window_summary(token_info.as_ref());
        Some(json!({
            "model_context_window": context_window,
            "tokens_in_context": tokens_in_context,
            "context_left_percent": context_left_percent,
        }))
    } else {
        None
    };

    Ok(ManageContextResult {
        json: json!({
            "mode": "retrieve",
            "snapshot_id": snapshot_id,
            "token_usage": token_usage,
            "breakdown": breakdown,
            "items": if include_items { Some(out_items) } else { None },
            "notes": if include_notes { Some(overlay.notes) } else { None },
        }),
    })
}

async fn handle_apply(
    session: &Session,
    args: &ManageContextToolArgs,
) -> Result<ManageContextResult, FunctionCallError> {
    if args.ops.is_empty() {
        return Err(FunctionCallError::RespondToModel(
            "manage_context.apply requires non-empty ops".to_string(),
        ));
    }

    let mut rollout_items: Vec<RolloutItem> = Vec::new();

    let (apply_result, new_snapshot_id) = {
        let mut state = session.state.lock().await;
        let before_ev = state.build_context_items_event();
        let before_overlay = state.context_overlay_snapshot();
        let current_snapshot_id = snapshot_id_for_context(&before_ev.items, &before_overlay);

        if let Some(expected) = args.snapshot_id.as_deref()
            && expected != current_snapshot_id
        {
            return Err(FunctionCallError::RespondToModel(format!(
                "snapshot mismatch (expected {expected}, got {current_snapshot_id}); run manage_context with mode=retrieve again"
            )));
        }

        let snapshot_items = state.history_snapshot();
        let snapshot_rids = state.history_rids_snapshot();
        let resolved_ops = resolve_ops(&snapshot_items, &snapshot_rids, &args.ops)?;

        if args.dry_run {
            let (summary, new_snapshot_id) = simulate_apply(
                &before_ev.items,
                &snapshot_items,
                &snapshot_rids,
                &before_overlay,
                &resolved_ops,
            )?;
            return Ok(ManageContextResult {
                json: json!({
                    "mode": "apply",
                    "dry_run": true,
                    "ok": true,
                    "applied": summary,
                    "new_snapshot_id": new_snapshot_id,
                }),
            });
        }

        let (summary, include_changed, overlay_changed, deleted_rids) =
            apply_resolved_ops(&mut state, &resolved_ops)?;

        if include_changed || !deleted_rids.is_empty() {
            let after_ev = state.build_context_items_event();
            let (included_indices, included_ids) = included_snapshot(&after_ev);
            let deleted_ids = deleted_rids.into_iter().map(rid_to_string).collect();
            rollout_items.push(RolloutItem::ContextInclusion(ContextInclusionItem {
                included_indices,
                included_ids,
                deleted_indices: Vec::new(),
                deleted_ids,
            }));
        }

        if overlay_changed {
            let overlay_item = context_overlay_rollout_item(&state.context_overlay_snapshot());
            rollout_items.push(overlay_item);
        }

        let final_ev = state.build_context_items_event();
        let final_overlay = state.context_overlay_snapshot();
        let new_snapshot_id = snapshot_id_for_context(&final_ev.items, &final_overlay);
        (summary, new_snapshot_id)
    };

    if !rollout_items.is_empty() {
        session.persist_rollout_items(&rollout_items).await;
    }

    Ok(ManageContextResult {
        json: json!({
            "mode": "apply",
            "dry_run": false,
            "ok": true,
            "applied": apply_result,
            "new_snapshot_id": new_snapshot_id,
        }),
    })
}

fn token_window_summary(
    token_info: Option<&TokenUsageInfo>,
) -> (Option<u64>, Option<u8>, Option<u64>) {
    let Some(info) = token_info else {
        return (None, None, None);
    };

    let context_window = info.model_context_window;
    let percent_left =
        context_window.map(|w| info.last_token_usage.percent_of_context_window_remaining(w));
    let tokens_in_context = Some(info.last_token_usage.tokens_in_context_window());
    (context_window, percent_left, tokens_in_context)
}

fn included_snapshot(ev: &crate::protocol::ContextItemsEvent) -> (Vec<usize>, Vec<String>) {
    let mut included_indices = Vec::new();
    let mut included_ids = Vec::new();
    for item in &ev.items {
        if item.included {
            included_indices.push(item.index);
            if let Some(id) = &item.id {
                included_ids.push(id.clone());
            }
        }
    }
    (included_indices, included_ids)
}

fn snapshot_id_for_context(
    items: &[crate::protocol::ContextItemSummary],
    overlay: &ContextOverlay,
) -> String {
    let mut hasher = sha1::Sha1::new();
    hasher.update(b"items\n");
    for item in items {
        hasher.update((item.index as u64).to_le_bytes());
        hasher.update(b"\n");
        if let Some(id) = &item.id {
            hasher.update(id.as_bytes());
        }
        hasher.update(b"\n");
        hasher.update(prune_category_tag(&item.category).as_bytes());
        hasher.update(b"\n");
        hasher.update([if item.included { 1 } else { 0 }]);
        hasher.update(b"\n");
    }

    hasher.update(b"replacements\n");
    for (rid, text) in &overlay.replacements_by_rid {
        hasher.update(rid.to_le_bytes());
        hasher.update(b"\n");
        hasher.update(text.as_bytes());
        hasher.update(b"\n");
    }

    hasher.update(b"notes\n");
    for note in &overlay.notes {
        hasher.update(note.as_bytes());
        hasher.update(b"\n");
    }

    format!("{:x}", hasher.finalize())
}

fn prune_category_tag(category: &crate::protocol::PruneCategory) -> &'static str {
    use crate::protocol::PruneCategory;
    match category {
        PruneCategory::ToolOutput => "tool_output",
        PruneCategory::ToolCall => "tool_call",
        PruneCategory::Reasoning => "reasoning",
        PruneCategory::AssistantMessage => "assistant_message",
        PruneCategory::UserMessage => "user_message",
        PruneCategory::UserInstructions => "user_instructions",
        PruneCategory::EnvironmentContext => "environment_context",
    }
}

#[derive(Default)]
struct CategoryBreakdown {
    total_items: usize,
    included_items: usize,
    raw_bytes_total: u64,
    effective_bytes_total: u64,
    raw_bytes_included: u64,
    effective_bytes_included: u64,
}

fn build_breakdown(
    summaries: &[crate::protocol::ContextItemSummary],
    items: &[ResponseItem],
    overlay: &ContextOverlay,
    max_top_items: usize,
) -> serde_json::Value {
    let mut by_category: HashMap<crate::protocol::PruneCategory, CategoryBreakdown> =
        HashMap::new();
    let mut top_included: Vec<serde_json::Value> = Vec::new();

    for summary in summaries {
        let item = items.get(summary.index);
        let raw_bytes = item.map(estimate_item_bytes).unwrap_or(0);

        let rid = summary.id.as_ref().and_then(|id| parse_rid(id));
        let replacement = rid.and_then(|rid| overlay.replacements_by_rid.get(&rid));
        let effective_bytes = replacement.map(|t| t.len() as u64).unwrap_or(raw_bytes);

        let entry = by_category.entry(summary.category.clone()).or_default();
        entry.total_items += 1;
        entry.raw_bytes_total = entry.raw_bytes_total.saturating_add(raw_bytes);
        entry.effective_bytes_total = entry.effective_bytes_total.saturating_add(effective_bytes);
        if summary.included {
            entry.included_items += 1;
            entry.raw_bytes_included = entry.raw_bytes_included.saturating_add(raw_bytes);
            entry.effective_bytes_included = entry
                .effective_bytes_included
                .saturating_add(effective_bytes);
        }

        if summary.included && max_top_items > 0 {
            let (call_id, tool_name, _pair) = describe_pair(item, false);
            top_included.push(json!({
                "approx_bytes": effective_bytes,
                "index": summary.index,
                "id": summary.id,
                "category": summary.category,
                "preview": summary.preview,
                "call_id": call_id,
                "tool_name": tool_name,
                "replaced": replacement.is_some(),
            }));
        }
    }

    top_included.sort_by(|a, b| {
        let al = a
            .get("approx_bytes")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        let bl = b
            .get("approx_bytes")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        bl.cmp(&al)
    });
    top_included.truncate(max_top_items);

    let mut ordered: BTreeMap<String, serde_json::Value> = BTreeMap::new();
    for category in [
        crate::protocol::PruneCategory::EnvironmentContext,
        crate::protocol::PruneCategory::UserInstructions,
        crate::protocol::PruneCategory::UserMessage,
        crate::protocol::PruneCategory::AssistantMessage,
        crate::protocol::PruneCategory::ToolCall,
        crate::protocol::PruneCategory::ToolOutput,
        crate::protocol::PruneCategory::Reasoning,
    ] {
        if let Some(stats) = by_category.get(&category) {
            ordered.insert(
                prune_category_tag(&category).to_string(),
                json!({
                    "total_items": stats.total_items,
                    "included_items": stats.included_items,
                    "approx_bytes": {
                        "raw_total": stats.raw_bytes_total,
                        "effective_total": stats.effective_bytes_total,
                        "raw_included": stats.raw_bytes_included,
                        "effective_included": stats.effective_bytes_included,
                    }
                }),
            );
        }
    }

    json!({
        "by_category": ordered,
        "top_included_items": top_included,
    })
}

fn estimate_item_bytes(item: &ResponseItem) -> u64 {
    match item {
        ResponseItem::Message { role, content, .. } => {
            let mut total = role.len() as u64;
            for c in content {
                match c {
                    ContentItem::InputText { text } | ContentItem::OutputText { text } => {
                        total = total.saturating_add(text.len() as u64);
                    }
                    ContentItem::InputImage { image_url } => {
                        total = total.saturating_add(image_url.len() as u64);
                    }
                }
            }
            total
        }
        ResponseItem::Reasoning {
            summary,
            content,
            encrypted_content,
            ..
        } => {
            let mut total = 0u64;
            for s in summary {
                match s {
                    codex_protocol::models::ReasoningItemReasoningSummary::SummaryText { text } => {
                        total = total.saturating_add(text.len() as u64);
                    }
                }
            }
            if let Some(content) = content {
                for c in content {
                    match c {
                        ReasoningItemContent::ReasoningText { text }
                        | ReasoningItemContent::Text { text } => {
                            total = total.saturating_add(text.len() as u64);
                        }
                    }
                }
            }
            if let Some(enc) = encrypted_content {
                total = total.saturating_add(enc.len() as u64);
            }
            total
        }
        ResponseItem::LocalShellCall {
            call_id, action, ..
        } => {
            let mut total = call_id.as_ref().map(|s| s.len() as u64).unwrap_or(0);
            match action {
                codex_protocol::models::LocalShellAction::Exec(exec) => {
                    for part in &exec.command {
                        total = total.saturating_add(part.len() as u64);
                    }
                    if let Some(wd) = &exec.working_directory {
                        total = total.saturating_add(wd.len() as u64);
                    }
                    if let Some(env) = &exec.env {
                        for (k, v) in env {
                            total = total.saturating_add(k.len() as u64);
                            total = total.saturating_add(v.len() as u64);
                        }
                    }
                }
            }
            total
        }
        ResponseItem::FunctionCall {
            name,
            arguments,
            call_id,
            ..
        } => (name.len() + arguments.len() + call_id.len()) as u64,
        ResponseItem::FunctionCallOutput { call_id, output } => {
            (call_id.len() + output.content.len()) as u64
        }
        ResponseItem::CustomToolCall {
            call_id,
            name,
            input,
            ..
        } => (call_id.len() + name.len() + input.len()) as u64,
        ResponseItem::CustomToolCallOutput { call_id, output } => {
            (call_id.len() + output.len()) as u64
        }
        ResponseItem::WebSearchCall { action, .. } => match action {
            codex_protocol::models::WebSearchAction::Search { query } => query.len() as u64,
            codex_protocol::models::WebSearchAction::Other => 0,
        },
        ResponseItem::Other => 0,
    }
}

fn describe_pair(
    item: Option<&ResponseItem>,
    include_pairs: bool,
) -> (Option<String>, Option<String>, Option<serde_json::Value>) {
    let Some(item) = item else {
        return (None, None, None);
    };

    let (call_id, tool_name, pair_kind, pair_call_id) = match item {
        ResponseItem::FunctionCall { call_id, name, .. } => (
            Some(call_id.clone()),
            Some(name.clone()),
            Some("call"),
            Some(call_id.clone()),
        ),
        ResponseItem::FunctionCallOutput { call_id, .. } => (
            Some(call_id.clone()),
            None,
            Some("output"),
            Some(call_id.clone()),
        ),
        ResponseItem::CustomToolCall { call_id, name, .. } => (
            Some(call_id.clone()),
            Some(name.clone()),
            Some("call"),
            Some(call_id.clone()),
        ),
        ResponseItem::CustomToolCallOutput { call_id, .. } => (
            Some(call_id.clone()),
            None,
            Some("output"),
            Some(call_id.clone()),
        ),
        ResponseItem::LocalShellCall {
            call_id: Some(call_id),
            ..
        } => (
            Some(call_id.clone()),
            Some("local_shell".to_string()),
            Some("call"),
            Some(call_id.clone()),
        ),
        _ => (None, None, None, None),
    };

    let pair = if include_pairs {
        pair_kind.map(|kind| {
            json!({
                "kind": kind,
                "pair_call_id": pair_call_id,
            })
        })
    } else {
        None
    };

    (call_id, tool_name, pair)
}

async fn resolve_target_indices(
    session: &Session,
    args: &ManageContextToolArgs,
) -> Result<Vec<usize>, FunctionCallError> {
    let state = session.state.lock().await;
    Ok(resolve_target_indices_locked(&state, args))
}

fn resolve_target_indices_locked(
    state: &crate::state::SessionState,
    args: &ManageContextToolArgs,
) -> Vec<usize> {
    let items = state.history_snapshot();
    let rids = state.history_rids_snapshot();
    let mut rid_lookup: HashMap<u64, usize> = HashMap::new();
    for (idx, rid) in rids.iter().copied().enumerate() {
        rid_lookup.insert(rid, idx);
    }

    let mut out: Vec<usize> = Vec::new();
    out.extend(
        args.indices
            .iter()
            .copied()
            .filter(|idx| *idx < items.len()),
    );

    for raw in &args.ids {
        if let Some(rid) = parse_rid(raw)
            && let Some(idx) = rid_lookup.get(&rid)
        {
            out.push(*idx);
        }
    }

    if !args.call_ids.is_empty() {
        let call_set: HashSet<&str> = args.call_ids.iter().map(String::as_str).collect();
        for (idx, item) in items.iter().enumerate() {
            let call_id = match item {
                ResponseItem::FunctionCall { call_id, .. }
                | ResponseItem::FunctionCallOutput { call_id, .. }
                | ResponseItem::CustomToolCall { call_id, .. }
                | ResponseItem::CustomToolCallOutput { call_id, .. } => Some(call_id.as_str()),
                ResponseItem::LocalShellCall {
                    call_id: Some(call_id),
                    ..
                } => Some(call_id.as_str()),
                _ => None,
            };
            if call_id.is_some_and(|cid| call_set.contains(cid)) {
                out.push(idx);
            }
        }
    }

    out.sort_unstable();
    out.dedup();
    out
}

fn resolve_ops(
    snapshot_items: &[ResponseItem],
    snapshot_rids: &[u64],
    ops: &[ManageContextOp],
) -> Result<Vec<ResolvedOp>, FunctionCallError> {
    let mut resolved = Vec::with_capacity(ops.len());

    for (idx, op) in ops.iter().enumerate() {
        let op_index = idx + 1;
        match op.op.as_str() {
            "include" | "exclude" | "delete" | "replace" | "clear_replace" => {
                let targets = op.targets.as_ref().ok_or_else(|| {
                    FunctionCallError::RespondToModel(format!(
                        "op {op_index} ({}) requires targets",
                        op.op
                    ))
                })?;
                let rids = resolve_target_rids(snapshot_items, snapshot_rids, targets);
                if rids.is_empty() {
                    return Err(FunctionCallError::RespondToModel(format!(
                        "op {op_index} ({}) resolved to 0 targets",
                        op.op
                    )));
                }

                match op.op.as_str() {
                    "include" => resolved.push(ResolvedOp::Include { rids }),
                    "exclude" => resolved.push(ResolvedOp::Exclude { rids }),
                    "delete" => {
                        let cascade = op.cascade.as_deref().unwrap_or("tool_outputs");
                        if cascade != "tool_outputs" {
                            return Err(FunctionCallError::RespondToModel(format!(
                                "op {op_index} (delete) only supports cascade=tool_outputs"
                            )));
                        }
                        resolved.push(ResolvedOp::Delete { rids });
                    }
                    "replace" => {
                        let Some(text) = op.text.as_deref() else {
                            return Err(FunctionCallError::RespondToModel(format!(
                                "op {op_index} (replace) requires text"
                            )));
                        };
                        if text.trim().is_empty() {
                            return Err(FunctionCallError::RespondToModel(format!(
                                "op {op_index} (replace) requires non-empty text"
                            )));
                        }
                        for rid in &rids {
                            if let Some(pos) = snapshot_rids.iter().position(|r| r == rid)
                                && let Some(item) = snapshot_items.get(pos)
                                && !matches!(
                                    item,
                                    ResponseItem::FunctionCallOutput { .. }
                                        | ResponseItem::CustomToolCallOutput { .. }
                                        | ResponseItem::Reasoning { .. }
                                )
                            {
                                return Err(FunctionCallError::RespondToModel(format!(
                                    "op {op_index} (replace) only supports tool outputs and reasoning (id={})",
                                    rid_to_string(*rid)
                                )));
                            }
                        }
                        resolved.push(ResolvedOp::Replace {
                            rids,
                            text: text.to_string(),
                        });
                    }
                    "clear_replace" => resolved.push(ResolvedOp::ClearReplace { rids }),
                    _ => unreachable!(),
                }
            }
            "clear_replace_all" => resolved.push(ResolvedOp::ClearReplaceAll),
            "include_all" => resolved.push(ResolvedOp::IncludeAll),
            "add_note" => {
                if op.notes.is_empty() {
                    return Err(FunctionCallError::RespondToModel(format!(
                        "op {op_index} (add_note) requires non-empty notes"
                    )));
                }
                resolved.push(ResolvedOp::AddNote {
                    notes: op.notes.clone(),
                });
            }
            "remove_note" => {
                if op.note_indices.is_empty() {
                    return Err(FunctionCallError::RespondToModel(format!(
                        "op {op_index} (remove_note) requires note_indices"
                    )));
                }
                resolved.push(ResolvedOp::RemoveNote {
                    note_indices: op.note_indices.clone(),
                });
            }
            "clear_notes" => resolved.push(ResolvedOp::ClearNotes),
            other => {
                return Err(FunctionCallError::RespondToModel(format!(
                    "unknown manage_context op: {other}"
                )));
            }
        }
    }

    Ok(resolved)
}

fn resolve_target_rids(
    snapshot_items: &[ResponseItem],
    snapshot_rids: &[u64],
    targets: &ManageContextTargets,
) -> Vec<u64> {
    let mut out: Vec<u64> = Vec::new();

    for &idx in &targets.indices {
        if let Some(rid) = snapshot_rids.get(idx).copied() {
            out.push(rid);
        }
    }

    for raw in &targets.ids {
        if let Some(rid) = parse_rid(raw) {
            out.push(rid);
        }
    }

    if !targets.call_ids.is_empty() {
        let call_set: HashSet<&str> = targets.call_ids.iter().map(String::as_str).collect();
        for (idx, item) in snapshot_items.iter().enumerate() {
            let call_id = match item {
                ResponseItem::FunctionCall { call_id, .. }
                | ResponseItem::FunctionCallOutput { call_id, .. }
                | ResponseItem::CustomToolCall { call_id, .. }
                | ResponseItem::CustomToolCallOutput { call_id, .. } => Some(call_id.as_str()),
                ResponseItem::LocalShellCall {
                    call_id: Some(call_id),
                    ..
                } => Some(call_id.as_str()),
                _ => None,
            };
            if call_id.is_some_and(|cid| call_set.contains(cid))
                && let Some(rid) = snapshot_rids.get(idx).copied()
            {
                out.push(rid);
            }
        }
    }

    out.sort_unstable();
    out.dedup();
    out
}

#[derive(Debug)]
enum ResolvedOp {
    Include { rids: Vec<u64> },
    Exclude { rids: Vec<u64> },
    IncludeAll,
    Delete { rids: Vec<u64> },
    Replace { rids: Vec<u64>, text: String },
    ClearReplace { rids: Vec<u64> },
    ClearReplaceAll,
    AddNote { notes: Vec<String> },
    RemoveNote { note_indices: Vec<usize> },
    ClearNotes,
}

fn simulate_apply(
    snapshot_summaries: &[crate::protocol::ContextItemSummary],
    snapshot_items: &[ResponseItem],
    snapshot_rids: &[u64],
    snapshot_overlay: &ContextOverlay,
    ops: &[ResolvedOp],
) -> Result<(serde_json::Value, String), FunctionCallError> {
    let mut temp = crate::state::SessionState::new();
    temp.replace_history_with_rids(snapshot_items.to_vec(), snapshot_rids.to_vec());

    let mut included: BTreeSet<usize> = BTreeSet::new();
    for item in snapshot_summaries {
        if item.included {
            included.insert(item.index);
        }
    }
    temp.set_include_mask(Some(included));
    temp.set_context_overlay(snapshot_overlay.clone());

    let (summary, _include_changed, _overlay_changed, _deleted_rids) =
        apply_resolved_ops(&mut temp, ops)?;

    let after_ev = temp.build_context_items_event();
    let after_overlay = temp.context_overlay_snapshot();
    let new_snapshot_id = snapshot_id_for_context(&after_ev.items, &after_overlay);

    Ok((summary, new_snapshot_id))
}

fn apply_resolved_ops(
    state: &mut crate::state::SessionState,
    ops: &[ResolvedOp],
) -> Result<(serde_json::Value, bool, bool, Vec<u64>), FunctionCallError> {
    let mut include_changed = false;
    let mut overlay_changed = false;
    let mut deleted_rids: Vec<u64> = Vec::new();
    let mut skipped_missing_targets = 0usize;

    let mut count_included = 0usize;
    let mut count_excluded = 0usize;
    let mut count_deleted = 0usize;
    let mut count_replaced = 0usize;
    let mut count_cleared_replacements = 0usize;
    let mut count_notes_added = 0usize;
    let mut count_notes_removed = 0usize;
    let mut cleared_notes = false;

    for op in ops {
        match op {
            ResolvedOp::Include { rids } => {
                let indices = indices_for_rids(state, rids, &mut skipped_missing_targets);
                if !indices.is_empty() {
                    state.set_context_inclusion(&indices, true);
                    include_changed = true;
                    count_included += indices.len();
                }
            }
            ResolvedOp::Exclude { rids } => {
                let indices = indices_for_rids(state, rids, &mut skipped_missing_targets);
                if !indices.is_empty() {
                    state.set_context_inclusion(&indices, false);
                    include_changed = true;
                    count_excluded += indices.len();
                }
            }
            ResolvedOp::IncludeAll => {
                state.set_include_mask(None);
                include_changed = true;
            }
            ResolvedOp::Delete { rids } => {
                let indices = indices_for_rids(state, rids, &mut skipped_missing_targets);
                if !indices.is_empty() {
                    let res = state.prune_by_indices(indices);
                    count_deleted += res.deleted_indices.len();
                    deleted_rids.extend(res.deleted_rids);
                    include_changed = true;
                }
            }
            ResolvedOp::Replace { rids, text } => {
                let current_rids: HashSet<u64> =
                    state.history_rids_snapshot().into_iter().collect();
                let mut updates = Vec::new();
                for rid in rids {
                    if current_rids.contains(rid) {
                        updates.push((*rid, text.clone()));
                    } else {
                        skipped_missing_targets += 1;
                    }
                }
                if !updates.is_empty() {
                    count_replaced += updates.len();
                    state.upsert_context_replacements(updates);
                    overlay_changed = true;
                }
            }
            ResolvedOp::ClearReplace { rids } => {
                let before = state.context_overlay_snapshot();
                state.clear_context_replacements_for(rids);
                let after = state.context_overlay_snapshot();
                let cleared = before
                    .replacements_by_rid
                    .len()
                    .saturating_sub(after.replacements_by_rid.len());
                count_cleared_replacements += cleared;
                overlay_changed = overlay_changed || cleared > 0;
            }
            ResolvedOp::ClearReplaceAll => {
                let before = state.context_overlay_snapshot();
                state.clear_context_replacements();
                let after = state.context_overlay_snapshot();
                let cleared = before
                    .replacements_by_rid
                    .len()
                    .saturating_sub(after.replacements_by_rid.len());
                count_cleared_replacements += cleared;
                overlay_changed = overlay_changed || cleared > 0;
            }
            ResolvedOp::AddNote { notes } => {
                let before_len = state.context_overlay_snapshot().notes.len();
                state.add_context_notes(notes.clone());
                let after_len = state.context_overlay_snapshot().notes.len();
                let added = after_len.saturating_sub(before_len);
                count_notes_added += added;
                overlay_changed = overlay_changed || added > 0;
            }
            ResolvedOp::RemoveNote { note_indices } => {
                let before_len = state.context_overlay_snapshot().notes.len();
                state.remove_context_notes(note_indices);
                let after_len = state.context_overlay_snapshot().notes.len();
                let removed = before_len.saturating_sub(after_len);
                count_notes_removed += removed;
                overlay_changed = overlay_changed || removed > 0;
            }
            ResolvedOp::ClearNotes => {
                let before_len = state.context_overlay_snapshot().notes.len();
                state.clear_context_notes();
                let after_len = state.context_overlay_snapshot().notes.len();
                let removed = before_len.saturating_sub(after_len);
                if removed > 0 {
                    cleared_notes = true;
                }
                count_notes_removed += removed;
                overlay_changed = overlay_changed || removed > 0;
            }
        }
    }

    let summary = json!({
        "included": count_included,
        "excluded": count_excluded,
        "deleted": count_deleted,
        "replaced": count_replaced,
        "cleared_replacements": count_cleared_replacements,
        "notes_added": count_notes_added,
        "notes_removed": count_notes_removed,
        "notes_cleared": cleared_notes,
        "skipped_missing_targets": skipped_missing_targets,
    });

    Ok((summary, include_changed, overlay_changed, deleted_rids))
}

fn indices_for_rids(
    state: &crate::state::SessionState,
    rids: &[u64],
    skipped_missing_targets: &mut usize,
) -> Vec<usize> {
    if rids.is_empty() {
        return Vec::new();
    }

    let snapshot_rids = state.history_rids_snapshot();
    let mut rid_lookup: HashMap<u64, usize> = HashMap::new();
    for (idx, rid) in snapshot_rids.into_iter().enumerate() {
        rid_lookup.insert(rid, idx);
    }

    let mut out = Vec::with_capacity(rids.len());
    for rid in rids {
        if let Some(idx) = rid_lookup.get(rid).copied() {
            out.push(idx);
        } else {
            *skipped_missing_targets += 1;
        }
    }

    out.sort_unstable();
    out.dedup();
    out
}

fn preview_text(text: &str) -> String {
    const MAX: usize = 80;
    let trimmed = text.trim();
    let first_line = trimmed.split('\n').next().unwrap_or("");
    if first_line.len() <= MAX {
        first_line.to_string()
    } else {
        crate::truncate::truncate_grapheme_head(first_line, MAX)
    }
}

fn context_overlay_rollout_item(overlay: &ContextOverlay) -> RolloutItem {
    let replacements: Vec<ContextOverlayReplacement> = overlay
        .replacements_by_rid
        .iter()
        .map(|(rid, text)| ContextOverlayReplacement {
            id: rid_to_string(*rid),
            text: text.clone(),
        })
        .collect();
    RolloutItem::ContextOverlay(ContextOverlayItem {
        replacements,
        notes: overlay.notes.clone(),
    })
}

async fn resolve_replacement_targets(
    session: &Session,
    replacements: &[ManageContextReplacement],
) -> Result<Vec<serde_json::Value>, FunctionCallError> {
    let state = session.state.lock().await;
    let items = state.history_snapshot();
    let rids = state.history_rids_snapshot();

    let mut out: Vec<serde_json::Value> = Vec::new();
    for replacement in replacements {
        let indices = resolve_replacement_target_indices(&items, &rids, replacement)?;
        out.push(json!({
            "id": replacement.id,
            "index": replacement.index,
            "call_id": replacement.call_id,
            "indices": indices,
        }));
    }
    Ok(out)
}

fn resolve_replacement_target_indices(
    items: &[ResponseItem],
    rids: &[u64],
    replacement: &ManageContextReplacement,
) -> Result<Vec<usize>, FunctionCallError> {
    let mut out: Vec<usize> = Vec::new();

    if let Some(index) = replacement.index
        && index < items.len()
    {
        out.push(index);
    }

    if let Some(id) = &replacement.id
        && let Some(rid) = parse_rid(id)
        && let Some(idx) = rids.iter().position(|r| *r == rid)
    {
        out.push(idx);
    }

    if let Some(call_id) = &replacement.call_id {
        for (idx, item) in items.iter().enumerate() {
            let matches_call = match item {
                ResponseItem::FunctionCallOutput { call_id: cid, .. } => cid == call_id,
                ResponseItem::CustomToolCallOutput { call_id: cid, .. } => cid == call_id,
                _ => false,
            };
            if matches_call {
                out.push(idx);
            }
        }
    }

    if out.is_empty() {
        return Err(FunctionCallError::RespondToModel(
            "replacement target must include id, index, or call_id".to_string(),
        ));
    }

    out.sort_unstable();
    out.dedup();
    Ok(out)
}
