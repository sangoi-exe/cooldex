use crate::stream_events_utils::InFlightFuture;
use codex_protocol::error::CodexErr;
use codex_protocol::error::Result as CodexResult;
use codex_protocol::models::ResponseInputItem;
use futures::future::BoxFuture;
use futures::stream::FuturesUnordered;
use futures::StreamExt;
use std::collections::BTreeMap;

pub(crate) struct SameStreamTools {
    next_sequence: u64,
    next_history_sequence: u64,
    in_flight:
        FuturesUnordered<BoxFuture<'static, (u64, CodexResult<ResponseInputItem>)>>,
    completed_by_sequence: BTreeMap<u64, ResponseInputItem>,
}

pub(crate) struct SameStreamToolCompletion {
    sequence: u64,
    result: CodexResult<ResponseInputItem>,
}

impl SameStreamToolCompletion {
    pub(crate) fn into_parts(self) -> (u64, CodexResult<ResponseInputItem>) {
        (self.sequence, self.result)
    }
}

impl SameStreamTools {
    pub(crate) fn new() -> Self {
        Self {
            next_sequence: 0,
            next_history_sequence: 0,
            in_flight: FuturesUnordered::new(),
            completed_by_sequence: BTreeMap::new(),
        }
    }

    pub(crate) fn push(&mut self, future: InFlightFuture<'static>) {
        let sequence = self.next_sequence;
        self.next_sequence += 1;
        self.in_flight.push(Box::pin(async move {
            let result = future.await;
            (sequence, result)
        }));
    }

    pub(crate) async fn next_completed(&mut self) -> Option<SameStreamToolCompletion> {
        self.in_flight
            .next()
            .await
            .map(|(sequence, result)| SameStreamToolCompletion { sequence, result })
    }

    pub(crate) fn record_sent(
        &mut self,
        sequence: u64,
        result: ResponseInputItem,
    ) -> CodexResult<Vec<ResponseInputItem>> {
        if sequence < self.next_history_sequence
            || self.completed_by_sequence.insert(sequence, result).is_some()
        {
            return Err(CodexErr::Fatal(format!(
                "same-stream tool completion sequence {sequence} was recorded more than once"
            )));
        }

        let mut ready = Vec::new();
        while let Some(result) = self
            .completed_by_sequence
            .remove(&self.next_history_sequence)
        {
            ready.push(result);
            self.next_history_sequence += 1;
        }
        Ok(ready)
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.in_flight.is_empty() && self.completed_by_sequence.is_empty()
    }
}

#[cfg(test)]
#[path = "same_stream_tools_tests.rs"]
mod tests;
