---
description: Relevo · what is waiting on you, and answer it from here
argument-hint: "[N <your answer> | N | seen N… | @name <note>]"
allowed-tools: mcp__relevo__waiting mcp__relevo__thread mcp__relevo__send
---
This is my /relevo command. Arguments: `$ARGUMENTS`

Use the relevo tools. A number refers to the last `waiting` list of this session: pass it to the tool as it is (`thread: "1"`), never a thread id, and never guess one.

The tools answer in markdown. When I say "print it", reply with the tool's text exactly as it is, character for character (backslashes, blank lines and `*` included), as markdown: NOT inside a code block, with nothing before or after it, except, after an answer or `seen`, at most one short line if something I asked is left unanswered. Never show your reasoning, comment on what you did or recap other threads.

- No arguments: call `waiting` and print it.
- `N <text>`: the text is my answer to thread N. Write it as my note: in the first person, complete, adding nothing I did not say. Send it with `send(thread: "N", text: …)` and print its answer. If the text asks you to do something first (check, look up, find out) rather than answer, do that, then show me the draft and ask before sending.
- `N` alone: call `thread(id: "N")`, then tell me in one or two lines what it needs from me and the next step you propose, and nothing else. Send nothing.
- `seen N M…`: call `send(thread: "N M…", seen: true)` once, and print its answer.
- `@name …`: a new note: send it with `send(text: …)`, written as the rules say, then print its answer and, under it, the exact text sent.
- Anything else: reply `/relevo · /relevo N <your answer> · /relevo N · /relevo seen N · /relevo @name <note>`.

When a tool says nothing was sent, or fails, print what it said and stop.
