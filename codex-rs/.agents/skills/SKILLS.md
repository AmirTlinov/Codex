[LEGEND]

[CONTENT]
# Skills — project memory (.agents/skills)

Эта папка — “долговременная память” проекта для ИИ‑агентов.

## Как добавить skill (норма)

1) Создай файл: `.agents/skills/<skill>/SKILL.md`.
2) В начале файла добавь YAML front matter:

```yaml
---
name: <skill>
description: "TRIGGER → OUTCOME → POINTERS"
ttl_days: 90   # 0 = evergreen (без требования обновлять по TTL)
---
```

3) В тексте skill обязательно укажи строку:

```text
Last verified: YYYY-MM-DD
```

4) Зарегистрируй skill в списке ниже как ссылку на файл.

## Список

- [orchestrator-role-split-pipeline](.agents/skills/orchestrator_role_split_pipeline/SKILL.md)
- [scout-context-pack](.agents/skills/scout_context_pack/SKILL.md)
