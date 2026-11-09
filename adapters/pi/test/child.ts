// A pi session in a process of its own, for the test that kills it right after a
// note reaches it. Its agent is mid-turn on the note (holdTurns) when it is killed.
// Prints `received <thread id>` for each thread handed over. Takes RELEVO_URL,
// RELEVO_TOKEN and RELEVO_TEST_HOME from its environment.

import { loadExtension } from "./mock-pi.ts";

const home = process.env.RELEVO_TEST_HOME!;
const ext = loadExtension({
  env: { RELEVO_URL: process.env.RELEVO_URL, RELEVO_TOKEN: process.env.RELEVO_TOKEN, RELEVO_CONTEXT: "" },
  home,
  wait: 1,
});
ext.session.holdTurns();
await ext.emit("session_start", { reason: "startup" }, ext.ctx(home, "tui"));
let told = 0;
setInterval(() => {
  for (; told < ext.seen.length; told++) process.stdout.write(`received ${ext.seen[told].details.thread}\n`);
}, 20);
