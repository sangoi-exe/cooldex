## Multi agents
You have the possibility to spawn and use other agents to complete a task. For example, this can be use for:
* Very large tasks with multiple well-defined scopes
* When you want a review from another agent. This can review your own work or the work of another agent.
* If you need to interact with another agent to debate an idea and have insight from a fresh context
* To split concrete implementation or review work across clearly bounded scopes.

This feature must be used wisely. For simple or straightforward tasks, you don't need to spawn a new agent.

**General comments:**
* When spawning multiple agents, you must tell them that they are not alone in the environment so they should not impact/revert the work of others.
* Do not delegate test execution or compile-triggering validation in this workspace. Keep sub-agents focused on bounded implementation, review, or reconnaissance scopes.
* When you're done with a sub-agent, don't forget to close it using `close_agent`.
<!-- Merge-safety anchor: collab prompt wait guidance must stay aligned with the runtime wait semantics so `any_final` does not regress into stale timeout-poll advice. -->
* If you are awaiting sub-agents, prefer one `wait(...)` call that matches the real intent:
  * use `wait(..., return_when="any_final", disable_timeout=true)` when you want the next requested agent that is not already final to reach a final status; if every requested agent is already final, the wait may return immediately
  * use `wait(..., return_when="all_final", disable_timeout=true)` only when you truly need every requested agent to reach a final status
  * omit `return_when` only when you intentionally want the timed convenience mode instead of a blocking final-status condition
  * never combine `disable_timeout=true` with `timeout_ms`
* Sub-agents do not necessarily have the same tool set as the lead. In particular, they keep `recall` but do not receive `manage_context`, so tell them if they are allowed to spawn sub-agents themselves or not.
