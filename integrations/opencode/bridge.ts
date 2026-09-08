// The agent-jit recorder bridge for OpenCode.
//
// Materialized by `agent-jit opencode materialize` and loaded through the config fragment the
// launcher injects with OPENCODE_CONFIG. It forwards documented plugin-hook facts to the
// recorder's stdin and never lets a recorder failure disturb the runtime. A marker file records
// each observed session so the launcher can synthesize session end once the runtime exits.

import type { Plugin } from "@opencode-ai/plugin"

const BIN = "{{AGENT_JIT_BIN}}"
const DELIVERY_TIMEOUT_MS = 2_000

type Context = { directory: string }
type Record_ = Record<string, any>

const directories = new Map<string, string>()
const pendingPrompt = new Map<string, { messageId: string; text: string }>()
const emittedPrompt = new Set<string>()

// Delivery is awaited, not fire-and-forget: an undelivered event would be lost the moment the
// runtime process exits, and the launcher's synthesized session-end must arrive last.
async function forward(kind: string, payload: Record_): Promise<void> {
  try {
    const child = Bun.spawn([BIN, "hook", "ingest", "--event", kind], {
      stdin: "pipe",
      stdout: "ignore",
      stderr: "ignore",
      timeout: DELIVERY_TIMEOUT_MS,
    })
    child.stdin.write(JSON.stringify(payload))
    child.stdin.end()
    await child.exited
  } catch {
    // A recorder that cannot be spawned must never disturb the runtime.
  }
}

function directoryOf(sessionID: string, ctx: Context): string {
  return directories.get(sessionID) ?? ctx.directory
}

async function flushPrompt(sessionID: string, ctx: Context): Promise<void> {
  const pending = pendingPrompt.get(sessionID)
  if (!pending || emittedPrompt.has(pending.messageId)) return
  emittedPrompt.add(pending.messageId)
  await forward("user-prompt-submit", {
    event: "user.prompt",
    session_id: sessionID,
    directory: directoryOf(sessionID, ctx),
    prompt: pending.text,
  })
}

function markerPath(sessionID: string): URL {
  const safe = sessionID.replace(/[^A-Za-z0-9_-]/g, "_")
  return new URL(`./sessions/${safe}.json`, import.meta.url)
}

export default (async (ctx: Context) => {
  return {
    event: async ({ event }: { event: Record_ }) => {
      try {
        const type = event.type as string
        const properties = (event.properties ?? {}) as Record_
        if (type === "session.created") {
          const info = properties.info as Record_
          directories.set(info.id, info.directory)
          await forward("session-start", {
            event: "session.created",
            session_id: info.id,
            directory: info.directory,
            source: "created",
          })
          await Bun.write(markerPath(info.id), JSON.stringify({
            directory: info.directory,
            runtime_version: process.env.AGENT_JIT_RUNTIME_VERSION ?? null,
          }))
        } else if (type === "message.updated") {
          const info = properties.info as Record_
          if (info.role === "user") {
            pendingPrompt.set(info.sessionID, { messageId: info.id, text: "" })
          } else {
            // An assistant turn beginning means the user's prompt is complete.
            await flushPrompt(info.sessionID, ctx)
          }
        } else if (type === "message.part.updated") {
          const part = properties.part as Record_
          const pending = pendingPrompt.get(part.sessionID)
          if (pending && part.messageID === pending.messageId && part.type === "text") {
            pending.text = String(part.text ?? "")
          }
        } else if (type === "session.idle") {
          const sessionID = properties.sessionID as string
          await flushPrompt(sessionID, ctx)
          await forward("stop", {
            event: "session.idle",
            session_id: sessionID,
            directory: directoryOf(sessionID, ctx),
          })
        }
      } catch {
        // Bridging must never disturb the runtime.
      }
    },
    "tool.execute.before": async (input: Record_, output: Record_) => {
      try {
        await forward("pre-tool-use", {
          event: "tool.execute.before",
          session_id: input.sessionID,
          directory: directoryOf(input.sessionID, ctx),
          tool_name: input.tool,
          tool_input: output.args ?? {},
        })
      } catch {
        // Ignored: the runtime must continue regardless.
      }
    },
    "tool.execute.after": async (input: Record_, output: Record_) => {
      try {
        await forward("post-tool-use", {
          event: "tool.execute.after",
          session_id: input.sessionID,
          directory: directoryOf(input.sessionID, ctx),
          tool_name: input.tool,
          tool_input: input.args ?? {},
          tool_response: {
            title: output.title ?? "",
            output: output.output ?? "",
          },
        })
      } catch {
        // Ignored: the runtime must continue regardless.
      }
    },
  }
}) satisfies Plugin
