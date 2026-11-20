// tx-ide spike plugin for OpenCode v2 (2.3.x): default export { id, setup(api) }.
// Logs every bus event (type + JSON) and spawns a subprocess per event.
import { appendFileSync } from "node:fs";
import { spawn } from "node:child_process";

const LOG = process.env.TX_OC_EVENT_LOG || "/tmp/tx-oc-events.jsonl";
const SPAWN_LOG = process.env.TX_OC_SPAWN_LOG || "/tmp/tx-oc-spawn.log";

function log(obj) {
  try { appendFileSync(LOG, JSON.stringify({ t: new Date().toISOString(), ...obj }) + "\n"); } catch {}
}
function shell(name) {
  try {
    const p = spawn("sh", ["-c", 'echo "$1" >> "$2"', "--", String(name), SPAWN_LOG], { stdio: "ignore", detached: true });
    p.unref();
  } catch (e) { log({ kind: "spawn-error", err: String(e) }); }
}

export default {
  id: "tx.events",
  setup: async (api) => {
    log({ kind: "init", apiKeys: Object.keys(api), app: api.app, location: api.location, options: api.options, pid: process.pid, argv: process.argv, OPENCODE_SESSION_ID: process.env.OPENCODE_SESSION_ID });
    const sub = api.event.subscribe();
    log({ kind: "subscribe-result", type: typeof sub, keys: sub && Object.keys(sub), asyncIter: !!(sub && sub[Symbol.asyncIterator]), then: !!(sub && sub.then) });
    let stopped = false;
    (async () => {
      try {
        const it = sub && sub.then ? await sub : sub;
        for await (const ev of it) {
          if (stopped) break;
          log({ kind: "event", type: ev?.type, event: ev });
          shell(ev?.type);
        }
        log({ kind: "event-stream-ended" });
      } catch (e) { log({ kind: "event-stream-error", err: String(e), stack: e?.stack }); }
    })();
    try {
      await api.permission.hook("ask", (input) => { log({ kind: "hook", hook: "permission.ask", input }); });
    } catch (e) { log({ kind: "hook-error", hook: "permission.ask", err: String(e) }); }
    try {
      await api.tool.hook("execute.before", (input) => { log({ kind: "hook", hook: "tool.execute.before", input }); });
    } catch (e) { log({ kind: "hook-error", hook: "tool.execute.before", err: String(e) }); }
    return () => { stopped = true; log({ kind: "dispose" }); };
  },
};
