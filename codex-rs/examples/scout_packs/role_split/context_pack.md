# role_split: Scout context pack (quote-backed example)

Minimal-noise example: Scout handoff with anchor-first CODE_REF entries and auto-expanded verbatim excerpts.

## Quality gates
- G1 Coverage
- G2 Determinism
- G3 Evidence-first
- G7 Quote-backed claims

## Just recipes: scout-pack and scout-pack-check

Single-command validation and rendering flow used by Scout.

**CODE_REF:** `CODE_REF::codex-rs::justfile#L75-L81` — justfile: scout-pack + scout-pack-check

Runtime handoff requires stdout-first rendering and preflight validation.

Must include:
- `scout-pack *args:`
- `scout-pack-check spec:`

```
# Generate a markdown context pack from a Scout excerpt spec (YAML/JSON).
scout-pack *args:
    python3 "{{justfile_directory()}}/codex-rs/scripts/scout_pack.py" "$@"

# Validate a Scout excerpt spec and render to stdout (discarded).
scout-pack-check spec:
    python3 "{{justfile_directory()}}/codex-rs/scripts/scout_pack.py" "{{spec}}" -o - >/dev/null
```

## Runtime Scout prompt contract

Prompt-level requirements for inline artifacts and quote-backed claims.

**CODE_REF:** `CODE_REF::codex-rs::codex-rs/core/templates/agents/scout.md#L12-L45` — Required artifacts + stdout-first render

Defines required artifacts and mandatory stdout render path.

Must include:
- `inline all three artifacts`
- `version: 2`
- `just scout-pack <excerpt_spec.yml> -o -`

```markdown
## Required output artifacts (inline in final answer)
1) `ScoutReport.md`
2) `excerpt_spec.yml` (or `.json`) — anchor-first spec (`code_ref` обязателен по умолчанию)
3) `context_pack.md` (generated from excerpt spec; must contain verbatim code quotes)

Your final answer MUST inline all three artifacts as fenced blocks.
Do not replace artifacts with prose summaries.

## Excerpt spec (required)
Your `excerpt_spec.yml` MUST be compatible with `scripts/scout_pack.py`.
Use the v2 shape:
- `version: 2`
- `sections[] -> excerpts[]` (or `sections[] -> anchors[]` alias)
- Required default entry form: `code_ref: CODE_REF::<crate>::<path>#L<start>-L<end>`
- `anchors[]` may use compact string form: `- "CODE_REF::<crate>::<path>#L<start>-L<end>"`
- Explicit ranges (`path + start_line/end_line`) are legacy-only and require `allow_explicit_ranges: true`
- A single heading may include multiple anchors from multiple files
- Mermaid may be inline (`mermaid`) or from file (`mermaid_file`)
- No placeholders (`Lx-Ly`, `<start>-<end>`, `...`)

Important: Scout does NOT hand-write verbatim code excerpts.
Scout provides anchors (`code_ref`) + heading/description/why; `scripts/scout_pack.py` expands anchors into real quoted code in `context_pack.md`.

Recommended starting points (copy + edit):
- `.agents/skills/scout_context_pack/templates/excerpt_spec.example.yml`
- `examples/scout_packs/role_split/excerpt_spec.yml`

Before handoff, keep Mermaid map incremental + validate output:
1) `just scout-map-init <map_state.json> --title "<slice map>" --direction LR` (once per slice)
2) `just scout-map-merge <map_state.json> <delta.yml>` after each new dependency finding
3) `just scout-map-render <map_state.json> --output "<map.mmd>"`
4) Set `mermaid_file: "<map.mmd>"` in `excerpt_spec.yml` sections that need diagrams
5) `just scout-pack-check <excerpt_spec.yml>`
6) `just scout-pack <excerpt_spec.yml> -o -`
```

**CODE_REF:** `CODE_REF::codex-rs::codex-rs/core/templates/agents/scout.md#L62-L69` — CODE_REF + quote requirement

Ensures Main receives claim evidence, not only line ranges.

Must include:
- `CODE_REF without a quote is invalid`
- `placeholder anchors`

```markdown
## CODE_REF contract
- Every key claim MUST include at least one `CODE_REF`.
- Format: `CODE_REF::<crate>::<repo_relative_path>#L<start>-L<end>`
- Line ranges are 1-indexed and inclusive.
- Avoid duplicate or overlapping anchors in the same code area.
- Every key claim MUST also include a short verbatim quote from `context_pack.md`.
- CODE_REF without a quote is invalid.
- Do not use placeholder anchors (`Lx-Ly`, `<start>-<end>`, `...`).
```

## Skill-level scout contract

Patch-ready quality gates and anti-noise guarantees.

**CODE_REF:** `CODE_REF::codex-rs::codex-rs/.agents/skills/scout_context_pack/SKILL.md#L25-L60` — Required sections + CODE_REF contract + gates

Keeps role prompt and skill docs aligned on quote-backed evidence.

Must include:
- `Evidence quotes`
- `CODE_REF`
- `G7 Quote-backed claims`

```markdown
## Required ScoutReport sections (strict order)
1) Scope snapshot
2) Patch target contract
3) Key invariants / constraints
4) Anchor map
5) Excerpt specs
6) Evidence quotes (verbatim, from `context_pack.md`)
7) Dependency map
8) High-confidence risks / edge cases
9) Missing context items needed before patching
10) Patch readiness gates

## CODE_REF contract
- Каждый ключевой тезис во всех секциях (инвариант/контракт/риск/гейт/gap) обязан иметь минимум один `CODE_REF`.
- Формат:
  - `CODE_REF::<crate>::<repo_relative_path>#L<start>-L<end>`
- Пример:
  - `CODE_REF::codex-core::core/src/tools/router.rs#L140-L260`
- Диапазоны строк: 1-indexed, `end` включительно.
- Каждый тезис обязан иметь короткую verbatim-цитату (1–8 строк) из `context_pack.md`.
- `CODE_REF` без цитаты считается невалидным доказательством.
- Placeholder anchors (`Lx-Ly`, `<start>-<end>`, `...`) запрещены.
- Если файл/диапазон не найден — FAIL и явный `BLOCKER`.
- Для multi-anchor under single heading: несколько anchors в одной секции разрешены, включая разные файлы.
- Mermaid в секции может быть inline (`mermaid`) или из файла (`mermaid_file`), но не одновременно.
- Legacy fallback (`path/start_line/end_line`) разрешён только с `allow_explicit_ranges: true`.

## Quality gates (all required)
- G1 Coverage: все предполагаемые patch touchpoints покрыты anchors.
- G2 Determinism: нет duplicate/overlap anchors в одной зоне.
- G3 Evidence-first: каждый ключевой тезис имеет `CODE_REF` + quote.
- G4 Actionability: из пакета выводится bounded file list для патча.
- G5 Unknowns explicit: неизвестности перечислены как falsifiable gaps.
- G6 Noise budget: нет нерелевантных дампов и лишних файлов.
- G7 Quote-backed claims: нет claim-ов без цитаты из `context_pack.md`.
```

## Scout report template expectations

Canonical report shape consumed by Main without extra digging.

**CODE_REF:** `CODE_REF::codex-rs::codex-rs/.agents/skills/scout_context_pack/templates/ScoutReport.md#L22-L46` — Invariants + Evidence quotes section

Shows required Claim/CODE_REF/Quote/Excerpt-id tuple.

Must include:
- `Evidence quotes`
- `Quote:`
- `Excerpt id:`

```markdown
- Verify command (single repro): `…`

## 3) Key invariants / constraints
Every item MUST have at least one `CODE_REF` and one short verbatim quote from `context_pack.md`.

- `<invariant>` (`CODE_REF::<crate>::<path>#Lx-Ly`)
- `<constraint>` (`CODE_REF::<crate>::<path>#Lx-Ly`)

## 4) Anchor map
- `CODE_REF::<crate>::<path>#Lx-Ly` — why this anchor matters.
- `CODE_REF::<crate>::<path>#Lx-Ly` — …

## 5) Excerpt specs
- Attach or inline `excerpt_spec.yml`.
- Preferred: anchor-first entries with `code_ref` + `label/why`; no ручной копипасты кода.
- Multi-anchor under single heading is allowed (including anchors from different files).
- Mermaid in section: inline (`mermaid`) or file-backed (`mermaid_file`), exactly one mode.
- Attach or inline generated `context_pack.md` (rendered from the same spec by `scripts/scout_pack.py`).
- Must cover all planned patch touchpoints.

## 6) Evidence quotes (verbatim, minimal)
- Claim: `<claim text>`
  - CODE_REF: `CODE_REF::<crate>::<path>#Lx-Ly`
  - Quote: `"<1-8 verbatim lines from context_pack excerpt>"`
  - Excerpt id: `<excerpt-id>`
```

**CODE_REF:** `CODE_REF::codex-rs::codex-rs/.agents/skills/scout_context_pack/templates/ScoutReport.md#L77-L84` — Patch readiness gates

Adds explicit gate for quote-backed claims.

Must include:
- `G7 Quote-backed claims`

```markdown
## 10) Patch readiness gates
- G1 Coverage: PASS|FAIL
- G2 Determinism: PASS|FAIL
- G3 Evidence-first: PASS|FAIL
- G4 Actionability: PASS|FAIL
- G5 Unknowns explicit: PASS|FAIL
- G6 Noise budget: PASS|FAIL
- G7 Quote-backed claims: PASS|FAIL
```

## Generator fail-closed checks

Renderer rejects duplicate/overlapping ranges and missing must_include tokens.

**CODE_REF:** `CODE_REF::codex-rs::codex-rs/scripts/scout_pack.py#L334-L454` — code_ref parsing + overlap checks + must_include enforcement

Prevents noisy or stale context by failing closed.

Must include:
- `code_ref cannot be combined`
- `overlapping range`
- `must_include`
- `token not found in excerpt body`

```python
        for j, excerpt in enumerate(excerpts):
            range_ctx = f"spec.sections[{i}].excerpts[{j}]"
            if isinstance(excerpt, str):
                excerpt = {"code_ref": excerpt}
            elif not isinstance(excerpt, dict):
                _die(f"{range_ctx}: expected mapping or CODE_REF string")
            _validate_no_unknown_fields(
                excerpt,
                {
                    "id",
                    "code_ref",
                    "crate",
                    "path",
                    "start_line",
                    "end_line",
                    "language",
                    "label",
                    "why",
                    "must_include",
                },
                range_ctx,
            )

            ex_id = excerpt.get("id")
            if ex_id is not None:
                ex_id = _expect_optional_str(ex_id, f"{range_ctx}.id")
                if ex_id in excerpt_ids:
                    _die(f"{range_ctx}.id: duplicate id: {ex_id}")
                excerpt_ids.add(ex_id)

            code_ref_raw = excerpt.get("code_ref")
            if code_ref_raw is not None:
                code_ref = _expect_str(code_ref_raw, f"{range_ctx}.code_ref")
                conflicting_fields = [
                    field
                    for field in ("crate", "path", "start_line", "end_line")
                    if excerpt.get(field) is not None
                ]
                if conflicting_fields:
                    _die(
                        f"{range_ctx}: code_ref cannot be combined with {', '.join(conflicting_fields)}"
                    )
                crate, rel_path, start_line, end_line = _parse_code_ref(
                    code_ref, f"{range_ctx}.code_ref"
                )
            else:
                if not allow_explicit_ranges:
                    _die(
                        f"{range_ctx}: explicit path/start_line/end_line requires spec.allow_explicit_ranges=true; use code_ref"
                    )
                rel_path = _expect_str(excerpt.get("path"), f"{range_ctx}.path")
                start_line = _expect_int(
                    excerpt.get("start_line"),
                    f"{range_ctx}.start_line",
                )
                end_line = _expect_int(
                    excerpt.get("end_line"),
                    f"{range_ctx}.end_line",
                )
                if start_line < 1:
                    _die(f"{range_ctx}: start_line must be >= 1")
                if end_line < start_line:
                    _die(f"{range_ctx}: end_line must be >= start_line")

                crate_raw = excerpt.get("crate")
                if crate_raw is None:
                    crate = default_crate
                else:
                    crate = _expect_crate(crate_raw, f"{range_ctx}.crate")

            range_key = (crate, rel_path)
            prior_ranges = ranges_by_path.setdefault(range_key, [])
            for prev_start, prev_end, prev_ctx in prior_ranges:
                if start_line == prev_start and end_line == prev_end:
                    _die(
                        f"{range_ctx}: duplicate range for {crate}::{rel_path}: {start_line}-{end_line} (already used by {prev_ctx})"
                    )
                if not (end_line < prev_start or start_line > prev_end):
                    _die(
                        f"{range_ctx}: overlapping range for {crate}::{rel_path}: {start_line}-{end_line} overlaps {prev_start}-{prev_end} ({prev_ctx})"
                    )
            prior_ranges.append((start_line, end_line, range_ctx))

            language = excerpt.get("language")
            if language is not None and not isinstance(language, str):
                _die(f"spec.sections[{i}].excerpts[{j}].language: expected string")
            if language is not None and language not in {"", "auto"} and not LANG_RE.fullmatch(language.strip()):
                _die(f"spec.sections[{i}].excerpts[{j}].language: invalid value")

            label = _expect_optional_str(excerpt.get("label"), f"{range_ctx}.label")
            why = _expect_optional_str(excerpt.get("why"), f"{range_ctx}.why")

            must_include = _expect_optional_str_list(
                excerpt.get("must_include"),
                f"spec.sections[{i}].excerpts[{j}].must_include",
            )

            file_path = (repo_root / rel_path).resolve()
            if not file_path.is_relative_to(repo_root):
                _die(f"excerpt path escapes repo_root: {rel_path}")
            if not file_path.exists() or not file_path.is_file():
                _die(f"excerpt file not found: {rel_path}")

            try:
                text = file_path.read_text(encoding="utf-8")
            except UnicodeDecodeError:
                _die(f"excerpt file is not valid UTF-8: {rel_path}")

            text = text.replace("\r\n", "\n")
            lines = text.splitlines(keepends=True)
            if end_line > len(lines):
                _die(
                    f"excerpt range out of bounds for {rel_path}: {start_line}-{end_line} (file has {len(lines)} lines)"
                )

            body = "".join(lines[start_line - 1 : end_line])
            for token in must_include:
                if token not in body:
                    _die(
                        f"spec.sections[{i}].excerpts[{j}].must_include: token not found in excerpt body: {token}"
                    )
```
