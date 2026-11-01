# TICKETING.md — trusty-git-analytics ticketing standard

Standard of record for every issue in this repository. The `ticketing` agent
reads this before any create, label, comment, or transition, in any tracker.
It overrides the `tm-ticketing` skill defaults and the per-machine
`agents.ticketing` config block. Generated 2026-11-01; edit any value.

## Tracker

- Tracker: gh (GitHub Issues)
- Repository: bobmatnyc/trusty-git-analytics
- Every issue operation routes through the `ticketing` agent.

## Labels

- Type — exactly one: `bug`, `enhancement`, `refactor`, `chore`, `documentation`, `epic`
- Component — one or more, naming this project's stack unit (Cargo package):
  `tga` (the root crate, `src/`), `trusty-audit` (`crates/trusty-audit/`).
  `trusty-audit-ui` also exists as a label but no `crates/trusty-audit-ui/`
  directory is present; apply it only when a finding cites code that belongs to it.
  The `website/` (SvelteKit, pnpm) has no component label: waive with
  `no-component-label: no Cargo crate owns website/`.
- Priority — optional, only when the issue text asserts severity: `P0`, `P1`, `P2`, `P3`
- Workstream — `ws/<session>`: the filing session's label, e.g. `ws/tm-tga`
- Lifecycle — `status:in-progress`, `status:coded`, `status:merged`, `status:tested`
  (mutually exclusive; see Lifecycle)
- Other in use: `blocked`, `blocked:external`, `backlog`, `paused`, `wont-do`,
  `regression`, `security`, `deps`, `ci`, `release`, `breaking-change`, `tech-debt`,
  `performance`, `database`, `cli`, `test`, `dx`, `ops`
- Not part of this standard: `trusty-mpm` is a component label for code under
  `crates/trusty-mpm/` only and is never a lifecycle or provenance label (nothing
  in this repository lives there, so it does not apply); the PR-tier and blast
  labels (`T2`-`T4`, `approval:level-*`, `blast:*`, `do-not-merge`) belong to pull
  requests; the legacy `milestone-1`..`milestone-6` phase labels, `porting`,
  `rust`, `api-compat`, `tga` as a provenance marker, `in-progress` (unprefixed),
  `ws/tm-tga-01`, `closed:sweep`, and GitHub's default labels (`duplicate`,
  `invalid`, `question`, `wontfix`, `good first issue`, `help wanted`).

## Milestones

- Live titles: `Backlog · tga`, `Backlog · audit`, and the six phase milestones
  `Phase 1: Core Infrastructure`, `Phase 2: Collection Pipeline`,
  `Phase 3: Classification Engine`, `Phase 4: Report Generation`,
  `Phase 5: Integrations & CLI`, `Phase 6: Performance & Polish`
- Selection rule, in order: the parent's milestone when the issue is a sub-issue;
  else `Backlog · tga` for the `tga` component and `Backlog · audit` for
  `trusty-audit`; else the one `tm issue standard` names. The phase milestones
  are not chosen for new issues.
- Unset milestone: post `no-milestone: <reason>` plus a proposal in the same dispatch.

## Projects

- Live projects, by number: #46 (owner bobmatnyc). Status field options: Backlog,
  Ready for Development, In Development, In PR, On Preview, Merged, On Prod,
  Published, Won't Do. Crate field options: `tga`, `trusty-audit`.
- Selection rule: every issue goes on #46; set its Crate field to match the
  component label.
- Attach with: `gh project item-add 46 --owner bobmatnyc --url <issue-url>`

## Relationships

- Parent/child: native sub-issue
- Sequencing: native blocked-by (a link to an issue in another repository is
  recorded only while that issue is open)
- PR → issue keyword: `Refs #N` (fixed)

## Lifecycle

- States: open → `status:in-progress` → `status:coded` → `status:merged` →
  `status:tested` → closed. Exactly one `status:*` label at a time, moved in
  step with the board's Status field:

| Board Status | `status:*` label |
|---|---|
| Backlog | none (plain open) |
| Ready for Development | none (plain open) |
| In Development | `status:in-progress` |
| In PR | `status:coded` |
| On Preview | `status:coded` (a preview deploy is a pre-merge PR build; no label of its own) |
| Merged | `status:merged` |
| On Prod | `status:merged` until live verification evidence exists, then `status:tested` (a deploy is not a verification) |
| Published | none (the issue is closed; it keeps `status:tested`) |
| Won't Do | none: the issue closes as not planned and no work happened, so no lifecycle label applies. `wont-do` marks a proposed close pending owner review |

- State model file: none in this repository. `tm issue transition` reads the
  Unicorn model and refuses `status:*` targets here; the agent uses
  `gh issue edit N --add-label <new> --remove-label <old>` in ONE call and
  reports the refusal.
- Transition command: `gh issue edit N --add-label status:X --remove-label status:Y`
- Close bar: closes only from `status:tested`, with the live-verification
  evidence in the closing comment. A docs-only issue may close from
  `status:merged` on a squash SHA plus confirmation the text is on `origin/main`.
  A merged fix that fails live verification stays open at `status:merged`
  (back to `status:coded` once a follow-up PR is open).
- Project conventions in force: every PR clearance, merge, release, and deploy
  goes through the Architect. `ci.yml` runs only on release tags
  (`trusty-git-analytics-v*`, `trusty-audit-v*`) and `workflow_dispatch`, so it
  never appears in a PR's own check list: dispatch it on every PR head and read
  the run for that head before asking for clearance. A cleared PR is squashed
  with `--match-head-commit`.

## Comment conventions

| Event | Comment posted | It carries |
|---|---|---|
| Claim at dispatch | yes | session name and date |
| `status:coded` | yes | the PR link |
| `status:merged` | yes | the squash SHA |
| `status:tested` | yes | what was run against the installed artifact and what it printed |
| Close | yes | the same evidence |
| Blocked | yes | the blocker, its impact, the unblock criteria |
| Review verdict | only when it changes the issue's state | the verdict |

## Behaviour settings

| Setting | Value |
|---|---|
| comment_on | claim, `status:coded`, `status:merged`, `status:tested`, close, blocked |
| transitions | automatic: the agent that observed the event owes the label pass |
| assign | `--assignee @me` on creation |
| set_milestone | yes |
| set_project | yes (#46) |
| create_missing_labels | yes, after `gh label list` confirms absence |
| dedupe_strictness | search open and closed on every key before filing |
| comment_verbosity | pointer-first |
| rollup_issue | none |
| pr_issue_link | `Refs #N` (fixed) |
| status_on_creation | plain open |

## Epics — trackers and phase issues

- Pattern reference: none committed in this repository
- Epic title: `[EPIC <epic#>] <the outcome, in plain words>` (created `[EPIC]`, renamed once the number is known)
- Phase title: `[EPIC_<epic#> PHASE_<n>] <what this phase does>`
- Phase linkage: native sub-issue
- Tracker markers: `<!-- phases:start -->` / `<!-- phases:end -->`, and
  `<!-- deferred:start -->` / `<!-- deferred:end -->`
- tracker_autoupdate: true
- update_triggers: phase opens, phase closes, phase blocks/unblocks, item deferred or landed
- Phase numbering: assigned once, never renumbered, never reused

## Research

- research_docs_path: `docs/research/<effort>/` (existing: `docs/research/`)
- Issue bodies carry: a link to the committed research doc, never the findings

## Follow-ups

| Setting | Value |
|---|---|
| budget_per_phase | 2 |
| severity_floor_for_standalone | HIGH |
| tracker | epic, or none |
| due_within_days | 14 |
| standalone_requires | trigger link, epic sub-issue, severity signal, milestone or due-by |

## Staleness

| Setting | Value |
|---|---|
| stale_after_days | 30 |
| close_stale_after_days | 60 |
| exempt_labels | `keep` |
| exempt_when_milestoned | true |
| decision_request | digest |
| dispositions | CLOSE / SUPERSEDE / KEEP, each with its evidence |

## Coverage and metrics

- Every open issue belongs to: an epic, or a `Backlog · tga` / `Backlog · audit` milestone
- Board filter: board #46 grouped by Status
- metrics: open count per epic, follow-ups per phase issue, stale count, median
  age of open follow-ups

## Dedupe and promotion

- Search keys: test name, panic or error text, affected symbol, subcommand
  (`tga audit`, `taudit run`), crate label
- Dispositions: `COMMENT` / `REOPEN` / `NEW REGRESSION` / `NO TICKET`
- Component waiver reason shape: `no-component-label: no Cargo crate owns <path>`
- What never becomes an issue here: a finding fixed in the current PR; an
  upstream defect in another repository (file it there and link it)

## Title and body

- Title: `<type>: <what is wrong or wanted>`, under ~70 characters
- Body: defect, evidence, 1-4 observable closure conditions; no structured headings

## Attribution

- Every issue body and comment ends with:
  `🤖🤖🤖 Generated with trusty-mpm — https://github.com/bobmatnyc/trusty-tools`

## Fixed regardless of this file

- `Refs #N`, never `Closes #N`.
- `trusty-mpm` is a component label only, never a lifecycle label.
