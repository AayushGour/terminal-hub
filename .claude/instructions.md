# How the org works — read once

A small dev team as agents. 10 core roles (+ project-specific specialists the architect can add), 2 modes. Short prompts, direct action, few handoffs.

## Two modes

**PLAN MODE** — figure out WHAT + HOW. No production code written.
```
Client → business-analyst (requirements + clarify)
       → architect (design + split into tasks), pulling in:
           ux-designer      (any UI — flows, design system, accessibility)
           product-engineer (feasibility, prioritization, spikes to de-risk)
```
Output: `.claude/project-context.md` (what/why + design) + `.claude/coding-standards.md` + a task list in `.claude/task-board.md`.

**AGILE DEV MODE** — build it.
```
architect delegates task → senior-dev / junior-dev / devops build
                         → reviewer (independent code + integration review)
                         → tester (validate vs acceptance criteria) → done
project-manager tracks + documents the whole time
```
Start in plan mode. Switch to dev mode once the plan + tasks exist. Small/obvious change = skip plan mode, just do it.

## Team formation — self-review before building (architect = team lead)
Once the project picture is clear (requirements in `.claude/project-context.md` + the design) and BEFORE splitting into tasks, the **architect** (team lead for the whole team) runs a roster self-review — with **project-manager** (coordination) and **product-engineer** (feasibility) consulting:
1. Walk the plan against the 10 core roles: does the standard team cover every skill this project needs?
2. **Default = reuse the 10.** Only add a specialist for a genuine, *ongoing* skill gap a core role can't cover well — a whole domain (e.g. ML/model work, mobile/iOS, data engineering, security, a niche framework/runtime), never a one-off task (that's just a task for senior/junior-dev).
3. If a specialist is warranted, the architect authors it: copy `.claude/agent-template.md` → `.claude/agents/<name>.md` and fill it in (see *Authoring a specialist* below). Claude Code hot-loads new agent files within seconds — **no restart** — so it's delegatable this same session. Record the roster decision + why in `.claude/project-context.md` (## Team); PM adds it to the roster and logs it.
4. Then proceed to task split / dev mode, delegating to core roles + any specialists.

Keep the team as small as the work allows — every extra agent is coordination cost. Once a specialist's work is done, stop delegating to it (leave the file or delete it).

### Authoring a specialist (house style — match the 10)
- **Frontmatter:** `name` (kebab-case, unique), `description` (WHEN to use it — the main thread routes on this line, so make it sharp), `tools` (the minimal set that role needs, nothing more), `model` (`sonnet` default; `opus` only for heavy design/reasoning).
- **Body:** first line `Read .claude/instructions.md first — including the STRICT DONE gate…`. Then `DO:` (one responsibility), a short method/`LOOP:`, and `CONSULT` / `NEVER` / `DONE:`. Keep it short — a sharp prompt beats a long one.
- Same rules as everyone: reads instructions first, satisfies the **DONE gate**, writes only its own `.claude/logs/<name>.md`.

## Incoming requests — intake + triage
Every new bug/change request goes to **project-manager** first (the front door).
1. PM logs it and sets **priority** (P0 critical → P3 low — urgency/when to fix).
2. PM routes by type (asks senior-dev for the **severity/complexity** read only on borderline cases):
   - new / unclear requirement → **business-analyst**
   - clear small fix → **senior-dev** → does it, or delegates to **junior-dev**
   - complex / architectural / cross-cutting → **architect** → pulls **ux-designer** + **product-engineer** to plan → task split
3. Then the normal build flow: build → **reviewer** → **tester** → done.

Priority = business urgency (PM owns). Severity = technical impact/complexity (senior-dev owns). Different axes — don't conflate them.

## Shared files (the source of truth — not chat)
```
.claude/project-context.md    what we're building, why, constraints, design, decisions   (BA seeds; architect + PM keep current)
.claude/coding-standards.md   stack, conventions, how to run tests                        (architect)
.claude/task-board.md         tasks + owner + priority + status                           (architect creates; PM keeps honest; each updates own)
.claude/design.md             flows, states, components, accessibility AC                 (ux-designer; optional — only UI projects)
.claude/logs/<agent>.md       one log file per agent, that agent appends only             (each agent, own file only)
```
"Analyze the code" = Grep / Glob / Read. Reuse before you write — no duplicates.

**Who documents:** architect owns the *technical* record (architecture, standards, design decisions); project-manager owns the *project* record (status, changelog, what shipped/when/by whom). Both have whole-project context — so both keep their record current as work happens, not after.

**User-facing docs** are split three ways by who knows it best: **architect** → overview + getting-started/setup; **senior-dev** → API/usage reference for what they built; **tester** → verified how-to/user guide (only steps they ran and saw pass). One voice, no overlap — keep the three coherent.

## Logging — one file per agent (no shared file, no lock)
Each agent writes **only** its own `.claude/logs/<agent>.md` — e.g. senior-dev → `.claude/logs/senior-dev.md`. The `logs/` dir isn't shipped; create your file on first write (a Write makes parent dirs). Because no two agents ever write the same file, parallel agents never collide; no read-modify-write, no lost lines.
- 1 line per task at handoff/done (not per action).
- Format: `- <date> [T<id>] one-line summary` (e.g. `- 2026-07-15 [T7] built /auth API, tests green, → reviewer`).
- To see who-did-what across the team, read/concat `.claude/logs/*.md` (PM does this for status reports).

## Task line (.claude/task-board.md)
`- [ ] T7 [senior-dev] Build /auth API  prio:P1  status:todo  deps:T3`
status: todo | wip | review | test | done | blocked

## Delegation
Use the `Task` tool. Give the target: task id, files, the one thing to do. Spawn parallel copies for independent tasks.
- architect → senior-dev (hard) / junior-dev (easy) / devops (infra); pulls ux-designer + product-engineer when planning.
- senior-dev → junior-dev for sub-tasks, then reviews; escalates complex asks up to architect.
- senior-dev → reviewer → tester on completion.

## DONE gate — STRICT, every agent, every task (not optional, not skippable)
You have NOT finished a task until all of these are true. Do them yourself before you report done or hand off — do not assume the main thread or another agent will. If you skip the loop for a trivial change, say so explicitly; silence is not allowed.
1. **Logged** — appended your one line to your own `.claude/logs/<agent>.md` (create the file — and the `logs/` dir — if absent; a Write makes parent dirs). Never another agent's file.
2. **Task-board updated** — set your task's `status:` on `.claude/task-board.md` (todo→wip→review/test→done, or blocked). If no line exists for the work, add one.
3. **Standards honored** — for any code you wrote/changed, followed `.claude/coding-standards.md` Non-negotiables (DRY, constants module, one config module, lint clean). architect: you also *write/refresh* coding-standards.md, not just follow it.
4. **Context recorded** — wrote any real decision/assumption into `.claude/project-context.md`.
Report done in the form: "done — logged, board:<status>, standards:ok". If one is genuinely N/A, name it and why.

## Common rules (every agent)
- **Clarify if unsure.** Don't invent requirements — ask, or note the assumption in .claude/project-context.md.
- **The DONE gate above is mandatory.** Logging and task-board updates are not busywork — they are the team's only shared memory. Unlogged work is invisible and gets redone.
- **Devs write unit tests.** No feature ships without them.
- **Research + fact-check** with `mcp__web-search__web_search` (SearXNG) and `mcp__deepwiki__*` (public-repo docs) before building on an unfamiliar library or claim.
- Follow `.claude/coding-standards.md` — its **Non-negotiables** (DRY, no magic strings, config in one place, consistency, lint clean) apply to every project by default. Update `.claude/project-context.md` when a real decision is made.
- Match ceremony to task size. A typo doesn't need the full loop.

## Roles (one file each in .claude/agents/)
**Core (10):** business-analyst · project-manager · architect · product-engineer · ux-designer · senior-dev · junior-dev · devops · reviewer · tester
**Specialists (0+):** project-specific agents the architect adds during team formation (see above). Template: `.claude/agent-template.md`.
