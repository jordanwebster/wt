# Phase 1 implementer choices

- Canonical JSON, including `WT_ACTIVATION`, is emitted as compact UTF-8 with recursively sorted object keys.
- Notice, prompt, log, signal, temporary-directory, and tmux-format choices are deferred to their owning later phases.
