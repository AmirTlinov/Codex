---
name: reflective-sidecar
description: Use when a task is cognitively heavy and the main lane may miss subtle details. It maintains a bounded transient side-thought window in `.agents/context/reflective-window.md` instead of forcing the main agent to carry every side idea in active focus.
---

# Reflective sidecar

## Objective

Emulate a human-like side-thought channel without turning the main agent into a
giant context accumulator.

The goal is **not** to keep more text in active memory.
The goal is to keep the main lane narrow while letting a bounded helper or
separate pass inspect the same situation for:

- hidden assumptions,
- non-obvious details,
- contradictions,
- integration risks,
- alternative hypotheses,
- interesting opportunities that may matter later.

## Core law

Do not treat "copy the whole context into another agent" as the actual win.

The win comes from:
1. a different attention objective,
2. a bounded output contract,
3. freshness and expiry,
4. promotion of only verified high-signal items back into the main lane.

Without those, this becomes orchestration theater and context bloat.

## Working memory surfaces

- **Main lane** = current critical path, active plan, immediate execution.
- **Reflective window** = transient side-thought store in:

```text
.agents/context/reflective-window.md
```

This file is intentionally gitignored. It is working memory, not durable
product truth.

Promote only proven items from the reflective window into durable repo truth or
into the actual implementation plan.

## Trigger conditions

Use this skill only when at least one of these is true:

- the task is ambiguous or high-stakes;
- the task has many moving parts;
- a failure happened and root cause is still unclear;
- the main lane is about to make an expensive move;
- there may be subtle user-intent or architecture implications;
- the task has gone long enough that blind spots are likely.

Do **not** run it continuously on a timer.
Prefer event-based triggers with cooldown.

Good trigger moments:
- after plan formation,
- after a failed implementation attempt,
- before a meaningful commit,
- after a large diff,
- before a costly test or migration,
- when the user says "глубоко подумай", "что мы упускаем?", or similar.

## Sidecar prompt shape

Whether you use a subagent or a separate local pass, give it one job:

> Ignore the main execution path. Look for subtle details, blind spots,
> contradictions, hidden assumptions, integration risks, alternative
> hypotheses, and high-upside ideas that the main lane may miss. Write only the
> smallest set of observations that could materially improve the outcome.

## Output contract

Write or refresh `.agents/context/reflective-window.md` in this shape:

```md
# Reflective window

## Goal
- one short bullet

## Active focus
- what the main lane is doing right now

## Fresh observations
- [kind] observation
  - why it matters
  - evidence
  - confidence: low|medium|high
  - action: ignore|watch|promote|verify

## Open hypotheses
- hypothesis
  - what would falsify it

## Promotion candidates
- item that should move into the main lane now

## Stale / discard
- items that should not keep occupying attention
```

Rules:
- cap the file to a **small active set**;
- prefer 3-7 live observations, not a giant essay;
- every item must either be actionable, watch-worthy, or explicitly discarded;
- stale items must be pruned, not accumulated forever.

## Main-lane behavior after refresh

After the reflective pass:
1. read only the active observations,
2. promote at most a few high-signal items into the current plan,
3. do not dump the whole reflective file into the main prompt,
4. verify before turning a hypothesis into a code or product claim.

## Anti-patterns

- running a sidecar on every turn,
- carrying the whole reflective file in active prompt forever,
- storing vague philosophical thoughts with no evidence or action,
- duplicating the same observation in AGENTS, docs, and transient context,
- using the sidecar as a substitute for real validation,
- spawning multiple overlapping sidecars with the same job.

## Design stance for future native implementation

If this concept is implemented natively in Codex later, preserve these
properties:

- event-driven, not constant;
- bounded output window, not raw context duplication;
- explicit freshness / decay;
- promotion-based integration into the main lane;
- different attention objective from the main lane;
- one narrow active focus on the hot path, broader storage off-path.
