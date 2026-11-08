// A stand-in for pi's ExtensionAPI: records tools, handlers and sent messages.

import { relevoExtension, type Options } from "../src/index.ts";

export interface Sent {
  message: any;
  options: any;
}

export function loadExtension(options: Options) {
  const tools = new Map<string, any>();
  const handlers = new Map<string, Array<(event: any, ctx: any) => unknown>>();
  const sent: Sent[] = [];
  const notified: string[] = [];
  const pi = {
    registerTool(tool: any) {
      tools.set(tool.name, tool);
    },
    on(event: string, handler: (event: any, ctx: any) => unknown) {
      handlers.set(event, [...(handlers.get(event) ?? []), handler]);
      return () => {};
    },
    sendMessage(message: any, opts: any) {
      sent.push({ message, options: opts });
    },
  };
  relevoExtension(options)(pi as any);

  function ctx(cwd: string, mode = "rpc") {
    return { cwd, mode, hasUI: mode !== "print", ui: { notify: (text: string) => notified.push(text) } };
  }

  return {
    tools,
    sent,
    notified,
    ctx,
    async emit(event: string, payload: any, context: any) {
      for (const h of handlers.get(event) ?? []) await h(payload, context);
    },
    /** Runs a tool the way pi does and returns its text, or throws its error. */
    async tool(name: string, params: any, cwd: string): Promise<string> {
      const res = await tools.get(name).execute("call-1", params, undefined, undefined, ctx(cwd));
      return res.content[0].text;
    },
  };
}

export async function until(check: () => boolean, ms = 5000): Promise<void> {
  const end = Date.now() + ms;
  while (!check()) {
    if (Date.now() > end) throw new Error("timed out");
    await new Promise((r) => setTimeout(r, 25));
  }
}
