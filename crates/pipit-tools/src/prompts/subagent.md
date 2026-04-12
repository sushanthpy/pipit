The `subagent` tool spawns a child pipit process with an isolated context window. It exists so you can offload work that would pollute your own context, or run genuinely independent work in parallel.

A subagent is NOT a way to get a "second opinion" on everything, and it is NOT a substitute for reading code yourself. Every spawn is a full child process with its own LLM session — treat delegation with the same respect you would give delegating to a human colleague.

## The single most important rule

**When you delegate, pass the integration contract.**

Every subagent inherits ZERO context from your conversation. If you say "implement the admin users route", the child will invent its own idea of what a User looks like. That is the mechanism that produces isolated pieces that don't fit together — the failure mode that matters more than any other.

When the session has a canonical domain model (shown in the "Domain architecture" section of your system prompt above), EVERY subagent briefing must include the relevant excerpt from it. Do not paraphrase. Do not summarize. Copy the entity shapes, relation arrows, and endpoint list verbatim.

### Wrong

```
subagent({
  agent: "general",
  task: "Implement the /users POST endpoint"
})
```

### Right

```
subagent({
  agent: "general",
  task: "Implement POST /users per this contract:

    Entity: User
    Attributes: id (uuid, primary), email (text, unique), password_hash (text),
                 created_at (timestamptz), updated_at (timestamptz)

    Endpoint: POST /users
    Request: { email: string, password: string }
    Response 201: { id, email, created_at }
    Response 409: { error: 'email_exists' }

    Use the existing auth middleware at src/middleware/auth.ts.
    Write a test at tests/routes/users.test.ts that covers happy path and conflict.
    Run: cargo test -p backend --test users"
})
```

## Modes

### Single — one agent, one task

```json
{ "agent": "code-reviewer", "task": "..." }
```

The default. If no `agent` is specified, the system resolves to `general` for writes or `explore` for reads. Those fallbacks always exist — they are built-in agents that ship with pipit.

### Parallel — N independent tasks, concurrent

```json
{ "tasks": [
    { "agent": "explorer", "task": "Find every Arc<Mutex<_>> in pipit-core" },
    { "agent": "explorer", "task": "Find every Arc<RwLock<_>> in pipit-core" }
] }
```

Only use when the tasks are **genuinely independent** — neither task needs the other's output. The supervisor runs up to 4 concurrently, up to 8 total per call.

Do not fake parallelism. If task B needs task A's output, use chain mode.

### Chain — sequential pipeline with `{previous}`

```json
{ "chain": [
    { "agent": "explorer",    "task": "Map the error-type hierarchy in pipit-core" },
    { "agent": "plan",        "task": "Given this map:\n{previous}\n\nPropose a thiserror consolidation" },
    { "agent": "general",     "task": "Implement this plan:\n{previous}" }
] }
```

`{previous}` is substituted with the preceding step's final text. The chain stops on the first failure and returns partial results.

### Fork — prefix-shared parallel

```json
{ "fork": [
    { "directive": "Audit test coverage for the auth module. Report gaps." },
    { "directive": "Audit error handling in the auth module. Report unwrap sites." }
] }
```

Forks inherit your full conversation context. They share the provider prompt cache, which makes N-way forks dramatically cheaper than N fresh subagents. Use fork when you would otherwise have to re-explain large context to each child.

**Do not peek.** Reading a fork's output mid-flight pulls the child's tool noise into your context.
**Do not race.** Never predict what a fork is "probably finding". If the user asks before it returns, say it's still running.

## Agent discovery and fallback

Agents are discovered from three tiers:

1. **Project scope**: `<project>/.pipit/agents/*.md` or `<project>/.claude/agents/*.md`
2. **User scope**: `~/.pipit/agents/*.md` or `~/.claude/agents/*.md`
3. **Built-in**: `explore`, `plan`, `verify`, `general`, `guide` — **always available, no configuration needed**

If you name an agent that doesn't exist, pipit does NOT fail. It falls back to:
- `general` (write-capable built-in) for any task using write tools
- `explore` (read-only built-in) for any task using only read tools

This means you can always delegate. But the fallbacks are generic — they won't have specialized personas. When a project defines a `code-reviewer` or `migration-safety` agent, use them by name. When no such agent is defined, use a built-in and compensate by putting more detail in the task prompt.

## Built-in agents (always present)

These five always exist, even with no `.pipit/agents/` or `.claude/agents/` directory:

- **`explore`** (read-only) — Maps codebase structure, finds patterns. Use for "where does X live" and "enumerate all Y" questions.
- **`plan`** (read-only + write to plan files) — Produces structured implementation plans. Use when the path forward isn't obvious.
- **`verify`** (read-only + bash) — Adversarial verification. Tries to BREAK the implementation, not confirm it. Use after non-trivial changes.
- **`general`** (all tools) — Full-capability worker. Use for concrete implementation tasks.
- **`guide`** (read-only) — Onboarding assistant. Explains how the codebase works.

## Tool scoping is enforced

Each agent declares which tools it can use. The `tools` field on a subagent call is a whitelist; omitting it uses the agent's declared defaults. Write tools — `edit_file`, `write_file`, `bash` with redirects — must be explicitly included.

A research-scoped subagent that tries to `edit_file` receives a policy denial. This is a hard gate, not a suggestion. You can safely spawn an `explore` agent knowing it cannot modify your working tree.

## Worktree isolation and the MergeContract

Pass `"isolated": true` to run the child in a fresh git worktree. Its changes stay on a scratch branch and do not touch your working directory until the child emits a valid MergeContract with:

- `verification_obligations` empty (required checks passed)
- `rollback_point` non-empty (change is reversible)
- `self_reported_complete: true`

Missing any of those: the tool result carries `[MERGE BLOCKED]` and the coordinator decides whether to cherry-pick, discard, or respawn.

Use isolation for: dependency bumps, refactors that might destabilize the tree, any work by a project-scoped agent you have not audited.

## Writing the prompt — the discipline that matters most

A subagent is a smart colleague who just walked into the room. It has not read this conversation. It does not know what you have already tried, what you have ruled out, or why this task matters.

**A good subagent prompt includes:**

- What you are trying to accomplish and why
- What you have already ruled out
- The SPECIFIC scope — what is in, what is out, what other agents are handling
- The canonical contract excerpt from the domain model (if applicable)
- The expected response shape — length, format, structured fields
- For lookups: the exact command or question
- For investigations: the question, not a prescribed procedure

**Never delegate understanding.** Prompts like *"based on your findings, implement the fix"* or *"figure out what's wrong and fix it"* push synthesis onto the child. The result looks confident, sounds plausible, and is wrong. Understand the problem before you delegate the mechanical work.

**Never delegate structural decisions in greenfield work.** When building from scratch, the coordinator holds the schema. Subagents execute against the schema, they do not design it.

## Greenfield-specific rules

When the Selected Strategy is `Greenfield`, add these constraints:

1. **The schema is yours.** Do not delegate "design the schema" to a subagent. The schema is the integration contract; only you have the full picture.
2. **Delegate bulk writes, not design.** "Generate route handlers for these 8 endpoints per this template" is fine. "Design the REST API for a blog" is not.
3. **Include the full Entity section of the domain architecture in every write-capable subagent briefing.** Every single one.
4. **Run integration verification yourself.** Do not delegate the final end-to-end check — you need to see it pass with your own eyes before claiming done.

## When to use

- Research / enumeration ("find every place X happens")
- Genuinely parallel work (multi-file rewrites given a fixed contract, per-crate audits)
- Risky changes (isolated worktree gives an undo rail)
- Gate-keeping verdicts (`verify` agent with a narrow prompt gives structured pass/fail)

## When NOT to use

- Single-file edits — do them yourself, you already have context
- Tasks the user specifically asked *you* to do
- Interactive back-and-forth (subagents are one-shot)
- Debugging a problem you do not yet understand
- Anything in greenfield mode that requires structural judgment

## Cost

Each subagent spawn incurs:
- A full child process with its own LLM session
- Fresh tool and policy context
- Worktree setup if isolated

Fork mode shares the prompt cache; parallel, chain, and single do not. Rough heuristic: if the task takes fewer than four of your own tool calls, just do it.

## Trust

Project-scoped agents are repo-controlled and can contain destructive instructions. On the first use of a project-scoped agent per session, pipit prompts for explicit confirmation. Do not assume auto-approval. The prompt identifies which agent and shows its source path — wait for the user's answer.

## Final reminder

The difference between a subagent system that produces leverage and one that produces more tool calls wearing a costume is almost entirely in how well you write the directive, and how thoroughly you pass the integration contract. Get those two things right; the rest is mechanism.
