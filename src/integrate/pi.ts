// Written by `herdr-app integrate pi`; `herdr-app integrate pi --remove` deletes it.
// Tells Herdr this terminal's pi session, so a terminal lost to a reboot or
// a core crash comes back with `pi --session <file>`.
import { spawn } from "node:child_process";

const HERDR_APP = "__HERDR_APP__";

// pi quits on SIGTERM/SIGHUP too (logout, reboot): that session should come back.
let signaled = false;
for (const signal of ["SIGTERM", "SIGHUP"]) {
  try {
    process.on(signal as NodeJS.Signals, () => {
      signaled = true;
    });
  } catch {}
}

function report(input: Record<string, unknown>): Promise<void> {
  if (!process.env.HC_TERMINAL_ID) return Promise.resolve();
  return new Promise((resolve) => {
    try {
      const child = spawn(HERDR_APP, ["agent-session", "pi"], { stdio: ["pipe", "ignore", "ignore"], windowsHide: true });
      child.on("error", () => resolve());
      child.on("close", () => resolve());
      child.stdin.end(JSON.stringify(input));
    } catch {
      resolve();
    }
  });
}

export default function (pi: any) {
  pi.on("session_start", async (_event: any, ctx: any) => {
    let file: unknown;
    try {
      file = ctx?.sessionManager?.getSessionFile?.();
    } catch {}
    // No file (--no-session): nothing to come back to.
    if (typeof file !== "string" || !file) return;
    await report({ hook_event_name: "SessionStart", session_path: file });
  });
  pi.on("session_shutdown", async (event: any) => {
    if (event?.reason === "quit" && !signaled) {
      await report({ hook_event_name: "SessionEnd", reason: "prompt_input_exit" });
    }
  });
}
