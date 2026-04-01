# Reflective sidecar window

This document captures the **concept** plus the current repo-level emulation.
For the native Codex runtime design, see `docs/native-reflective-window.md`.

## Problem

When the main agent tries to hold:

- the current task,
- user intent,
- architectural constraints,
- failed-path lessons,
- hidden risks,
- alternative hypotheses,
- and every interesting side idea

all in one active lane, one of two bad things happens:

1. the agent becomes shallow because it stops exploring side implications;
2. the agent becomes noisy because it keeps too much in hot context.

Humans usually do not solve this by keeping everything in immediate focus.
They keep a narrow active focus and let side-thoughts surface, consolidate, or
die off-path.

## The right design

The right design is **not** "more context".

The right design is:

- one narrow active focus in the main lane;
- one bounded reflective sidecar with a different attention objective;
- promotion of only high-signal, still-relevant, evidence-backed items into the
  main lane.

This is a cognitive-load design, not a memory-hoarding design.

## Why full-context sidecars are both useful and dangerous

Giving a sidecar the same context can be useful because it sees:

- the same user intent,
- the same recent failures,
- the same artifacts,
- the same hidden tension between local choices and the overall goal.

But naive full-context copying is dangerous because it creates:

- duplicated cost,
- duplicated noise,
- stale side-thought accumulation,
- orchestration theater instead of better intelligence,
- false confidence from quantity of thought rather than quality of distinction.

So the important thing is not full-context copying by itself.
The important thing is **different attention + bounded output + freshness**.

## Recommended architecture

### 1. Main lane

Owns:

- critical path,
- current plan,
- tool execution,
- code/doc changes,
- final synthesis.

The main lane should stay narrow.

### 2. Reflective sidecar

Owns:

- blind-spot detection,
- anomaly spotting,
- hidden-assumption checks,
- alternative hypotheses,
- integration-risk scouting,
- high-upside idea scouting.

It should not try to execute the whole task.

### 3. Reflective window

A bounded working-memory surface that stores only:

- fresh observations,
- open hypotheses,
- promotion candidates,
- discard decisions.

This should behave like an **active set**, not an archive.

## Trigger model

Do not run the sidecar constantly.
Use **event-driven triggers**:

- after planning,
- after a failed attempt,
- before a large change,
- before a costly run,
- after a large diff,
- when ambiguity remains high.

Also use cooldown so the system does not reflexively spawn helpers.

## Output contract

The reflective window should contain small, high-signal items with:

- what was noticed,
- why it matters,
- evidence,
- confidence,
- whether to ignore, watch, verify, or promote.

Anything without action or evidence should decay out.

## Freshness and decay

This is the most important part.

If a side-thought window does not decay, it turns into cognitive sludge.

Every entry should be treated as one of:

- **promoted** — now part of the active plan;
- **watching** — keep briefly;
- **verify** — test before trusting;
- **discarded** — explicitly remove from attention.

That means the window stays dynamic instead of becoming a second permanent
memory dump.

## What should be durable vs transient

### Transient

- fleeting side-thoughts,
- weak hypotheses,
- half-formed opportunities,
- local anomaly notes.

These belong in a transient working-memory surface such as:

```text
.agents/context/reflective-window.md
```

### Durable

- accepted workflow rules,
- validated design principles,
- implementation decisions,
- user-facing behavior changes,
- real architecture changes.

These belong in repo truth:

- `AGENTS.md`
- `.agents/skills/*`
- `docs/*`
- actual code/tests

## Current repo-level emulation

This fork now supports a repo-level emulation of the concept through:

- `.agents/skills/reflective-sidecar/SKILL.md`
- transient gitignored working memory in `.agents/context/`

That gives the agent a practical way to think in two lanes **today** without
first modifying Codex runtime internals.

## If implemented natively in Codex later

The native feature should preserve these invariants:

1. event-driven sidecar activation;
2. one bounded reflective window, not unlimited auxiliary context;
3. freshness / decay;
4. promotion into the main lane rather than permanent duplication;
5. a distinct attention objective from the main lane;
6. no blind fan-out or overlapping helper ownership.

## Bottom line

The best version of this idea is:

> not a bigger mind,
> but a narrower hot path plus a disciplined reflective side-channel.

That is how the system gets deeper without getting cognitively messier.
