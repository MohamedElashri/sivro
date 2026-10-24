import { spawn } from "node:child_process";

// Keep the driver and its descendants in a group owned by this test run.
export function startProcess(command, args, options) {
  const grouped = process.platform !== "win32";
  const child = spawn(command, args, { ...options, detached: grouped });
  const closed = new Promise(resolve => child.once("close", resolve));
  const pause = ms => new Promise(resolve => setTimeout(resolve, ms).unref());
  const signal = name => {
    if (!child.pid) return;
    try {
      if (grouped) process.kill(-child.pid, name);
      else child.kill(name);
    } catch (error) {
      if (error.code !== "ESRCH") throw error;
    }
  };
  return {
    child,
    async stop(graceMs = 1000) {
      try {
        // The driver may already have exited while WebKit still owns its pipes.
        signal("SIGTERM");
        await Promise.race([closed, pause(graceMs)]);
        signal("SIGKILL");
        await Promise.race([closed, pause(graceMs)]);
      } finally {
        for (const stream of child.stdio) stream?.destroy();
        child.unref();
      }
    },
  };
}
