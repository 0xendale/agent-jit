# Claude Code integration templates

These files are the *source* of the plugin agent-jit materializes into private application data.
They are never used from this directory and are never copied into a user's `~/.claude/` or into an
observed repository: `agent-jit claude materialize` renders them into
`<app-data>/claude-plugin/<version>/` and `agent-jit claude run` passes that directory to Claude
with a single `--plugin-dir` argument.

`{{AGENT_JIT_BIN}}` is replaced with the absolute path of the running `agent-jit` binary. The
substitution happens inside JSON string values, so a path containing spaces, quotes, or shell
metacharacters stays one JSON string and one argv element — there is no shell in the path from a
hook to the recorder.

Every hook command is exactly `agent-jit hook ingest --event <fixed-event>`, and the one MCP server
is exactly `agent-jit mcp serve`. Neither ever carries user text.
