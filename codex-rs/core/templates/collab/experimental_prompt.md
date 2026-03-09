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
* Be careful on the `timeout_ms` parameter you choose for `wait`. It should be wisely scaled.
* Sub-agents have access to the same set of tools as you do so you must tell them if they are allowed to spawn sub-agents themselves or not.
