// A stand-in for pi's ExtensionAPI: records tools, handlers and sent messages,
// and plays a session the way pi 1.5 does (agent-session.js sendCustomMessage):
// while a turn runs, a message with triggerTurn goes to the follow-up queue,
// which Esc empties without a word; while idle, triggerTurn starts a turn whose
// first message reaches the agent (message_start); without it the message is
// added to the session as is.

import { relevoExtension, type Options } from "../src/index.ts";

export interface Sent {
  message: any;
  options: any;
}

export function loadExtension(options: Options) {
  const tools = new Map<string, any>();
  const commands = new Map<string, any>();
  const renderers = new Map<string, any>();
  /** Each footer status set, by key, in order. */
  const statuses: Array<[string, string | undefined]> = [];
  const handlers = new Map<string, Array<(event: any, ctx: any) => unknown>>();
  const sent: Sent[] = [];
  /** Events the extension emitted on pi's bus, in order (pi-powerbar's among them). */
  const events: Array<[string, any]> = [];
  const notified: string[] = [];
  /** The level of each notice in `notified`. */
  const levels: string[] = [];
  /** Custom messages the agent has in its session: what it actually sees. */
  const seen: any[] = [];
  const followUps: any[] = [];
  let streaming = false;
  /** Compacting or summarizing a branch: not idle, though no turn runs. */
  let compacting = false;
  /** Turns started by a message settle on their own unless a test holds them. */
  let holdTurns = false;
  /** The next turn a message starts ends before showing it (as Esc during its start would). */
  let dropNextStart = false;

  async function emit(event: string, payload: any, context?: any) {
    for (const h of handlers.get(event) ?? []) await h(payload, context);
  }

  async function show(message: any) {
    seen.push(message);
    await emit("message_start", { type: "message_start", message: { role: "custom", ...message } });
  }

  async function settle(aborted: boolean) {
    streaming = false;
    await emit("agent_settled", { type: "agent_settled", aborted });
  }

  async function startTurn(first?: any) {
    streaming = true;
    await emit("agent_start", { type: "agent_start" });
    if (first && dropNextStart) {
      dropNextStart = false;
      return settle(true);
    }
    if (first) await show(first);
    if (first && !holdTurns) setTimeout(() => void session.finishTurn(), 10);
  }

  const session = {
    get streaming() {
      return streaming;
    },
    followUps,
    compacting(on = true) {
      compacting = on;
    },
    holdTurns(on = true) {
      holdTurns = on;
    },
    dropNextStart() {
      dropNextStart = true;
    },
    /** The person sends a prompt: a turn starts and runs until finishTurn or escape. */
    async userPrompt() {
      await startTurn();
    },
    /** The turn ends; queued follow-ups run first, as pi does. */
    async finishTurn() {
      if (!streaming) return;
      while (followUps.length) await show(followUps.shift());
      await settle(false);
    },
    /** Esc: pi clears the queues (restoreQueuedMessagesToEditor) and aborts. */
    async escape() {
      followUps.length = 0;
      if (streaming) await settle(true);
    },
  };

  const pi = {
    registerTool(tool: any) {
      tools.set(tool.name, tool);
    },
    registerCommand(name: string, command: any) {
      commands.set(name, command);
    },
    registerMessageRenderer(customType: string, renderer: any) {
      renderers.set(customType, renderer);
    },
    on(event: string, handler: (event: any, ctx: any) => unknown) {
      handlers.set(event, [...(handlers.get(event) ?? []), handler]);
      return () => {};
    },
    events: {
      emit(name: string, data: any) {
        events.push([name, data]);
      },
      on() {
        return () => {};
      },
    },
    sendMessage(message: any, opts: any) {
      sent.push({ message, options: opts });
      if (opts?.deliverAs === "nextTurn") return;
      if (streaming && opts?.triggerTurn !== false) followUps.push(message);
      else if (opts?.triggerTurn) void startTurn(message);
      else void show(message); // Idle, or added after the turn: in the session either way.
    },
  };
  relevoExtension(options)(pi as any);

  function ctx(cwd: string, mode = "rpc") {
    return {
      cwd,
      mode,
      hasUI: mode !== "print",
      ui: {
        notify: (text: string, level = "info") => {
          notified.push(text);
          levels.push(level);
        },
        setStatus: (key: string, text: string | undefined) => {
          statuses.push([key, text]);
        },
        theme: tagTheme,
      },
      isIdle: () => !streaming && !compacting,
    };
  }

  return {
    tools,
    commands,
    renderers,
    statuses,
    /** The footer's latest `relevo` status, its colours as tags. */
    status(): string | undefined {
      return statuses.filter(([k]) => k === "relevo").at(-1)?.[1];
    },
    events,
    /** The latest pi-powerbar segment the extension sent, as pi-powerbar receives it. */
    powerbar(): any {
      return events.filter(([n, d]) => n === "powerbar:update" && d.id === "relevo").at(-1)?.[1];
    },
    /** Runs `/name args` the way pi does. */
    async command(name: string, args: string, cwd: string, mode = "interactive"): Promise<void> {
      await commands.get(name).handler(args, ctx(cwd, mode));
    },
    sent,
    seen,
    notified,
    levels,
    session,
    ctx,
    emit,
    /** Runs a tool the way pi does and returns its text, or throws its error. */
    async tool(name: string, params: any, cwd: string): Promise<string> {
      const res = await tools.get(name).execute("call-1", params, undefined, undefined, ctx(cwd));
      return res.content[0].text;
    },
  };
}

/** A theme that shows its colours as tags: `<warning>3</warning>`, `<b>Ana</b>`. */
export const tagTheme = {
  fg: (color: string, text: string) => `<${color}>${text}</${color}>`,
  bold: (text: string) => `<b>${text}</b>`,
};

/** A theme without colours. */
export const plainTheme = { fg: (_c: string, text: string) => text, bold: (text: string) => text };

export async function until(check: () => boolean, ms = 5000): Promise<void> {
  const end = Date.now() + ms;
  while (!check()) {
    if (Date.now() > end) throw new Error("timed out");
    await new Promise((r) => setTimeout(r, 25));
  }
}
