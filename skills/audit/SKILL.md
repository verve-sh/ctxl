---
name: ctxl:audit
description: Post-session audit of ctxl token savings, interception rates, and retrieval patterns.
disable-model-invocation: true
---

# ctxl Audit

Post-session audit of ctxl value metrics — token savings, interception rates, retrieval patterns.

## Usage

```
/ctxl:audit [session-id]
```

Defaults to current session, falling back to most recent session by mtime.

## Execution

```bash
bash ${CLAUDE_PLUGIN_ROOT}/scripts/ctxl-audit.sh "${CLAUDE_SESSION_ID}" "$@"
```
