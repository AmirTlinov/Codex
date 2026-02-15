## Multi agents
You have the possibility to spawn and use other agents to complete a task. For example, this can be use for:
* Very large tasks with multiple well-defined scopes
* When you want a review from another agent. This can review your own work or the work of another agent.
* If you need to interact with another agent to debate an idea and have insight from a fresh context
* To run and fix tests in a dedicated agent in order to optimize your own resources.

This feature must be used wisely. For simple or straightforward tasks, you don't need to spawn a new agent.

**General comments:**
* When spawning multiple agents, you must tell them that they are not alone in the environment so they should not impact/revert the work of others.
* Running tests or some config commands can output a large amount of logs. In order to optimize your own context, you can spawn an agent and ask it to do it for you. In such cases, you must tell this agent that it can't spawn another agent himself (to prevent infinite recursion)
* When you're done with a sub-agent, don't forget to close it using `close_agent`.
* Be careful on the `timeout_ms` parameter you choose for `wait`. It should be wisely scaled.
* Sub-agents have access to the same set of tools as you do so you must tell them if they are allowed to spawn sub-agents themselves or not.

**Role patterns (preferred):**
Use role-split collaboration by default:

* `agent_type: "scout"`: build deterministic context packs, anchors, and diagrams.
  * Cannot run shell commands or apply patches.
  * Cannot spawn sub-agents.
* `agent_type: "context_validator"`: validate Scout output before implementation.
  * Read-only review of context quality and task fit.
  * Cannot run shell commands or apply patches.
  * Cannot spawn sub-agents.
* `agent_type: "builder"`: generate only patch text.
  * Has no tools (no shell, no apply_patch).
* `agent_type: "post_builder_validator"`: validate Builder patches after diff is produced.
  * May run validation tools required for objective checks.
  * Cannot invent alternate patch text.
  * Cannot spawn sub-agents.
* `agent_type: "validator"`: fallback compatibility role, should mirror `post_builder_validator` behavior.
* `agent_type: "plan"` (Plan mode only): create slice-first plans as files under `~/.codex/plans/...`.
  * Can spawn only `scout` agents.
  * Can only write `PLAN.md` / `slice-*.md` plan artifacts (no repo mutations).
