// Fornax opencode plugin (FORNX-161). An @opencode-ai/plugin `Plugin`:
// opencode's own runtime loads this in-process and invokes the returned
// `Hooks` synchronously around real events (see
// @opencode-ai/plugin's Hooks interface). This is the genuinely distinct
// integration mechanism FORNX-161 exists to test — not an external
// hook-script process (Claude Code) and not a file tail (Codex).
//
// Spawns `fornax-hook-opencode` once at plugin init and keeps it alive for
// the life of the opencode process, piping one NDJSON line per hook
// invocation to its stdin: `{"hook": "<name>", "at": "<ISO8601>", "payload":
// <the hook's real input/output>}` — the exact wire contract
// `fornax_adapter_opencode::translate` parses. `fornax-hook-opencode` must
// be on PATH.
//
// Enable by adding to an opencode project's opencode.json:
//   { "plugin": ["<path to this file>"] }
import { spawn } from "node:child_process";

export const FornaxCapture = async () => {
  const child = spawn("fornax-hook-opencode", [], { stdio: ["pipe", "ignore", "ignore"] });

  function send(hook, payload) {
    const line = JSON.stringify({ hook, at: new Date().toISOString(), payload });
    try {
      child.stdin.write(line + "\n");
    } catch {
      // Best-effort: a dead/missing fornax-hook-opencode process must never
      // fail or slow down the agent's own turn.
    }
  }

  return {
    dispose: async () => {
      try {
        child.stdin.end();
      } catch {
        // ignore
      }
    },
    event: async ({ event }) => {
      send("event", event);
    },
    "tool.execute.before": async (input, output) => {
      send("tool.execute.before", { input, output });
    },
    "tool.execute.after": async (input, output) => {
      send("tool.execute.after", { input, output });
    },
    "chat.message": async (input, output) => {
      send("chat.message", { input, output });
    },
    "permission.ask": async (input, output) => {
      send("permission.ask", { input, output });
    },
  };
};
