# Task Completion

There is **no CI** in this repo (`.github/` contains only `FUNDING.yml`) and no linter or formatter configured — no ESLint, no Prettier, no `.editorconfig`. Nothing will catch mistakes for you; run the checks by hand.

After any client change (`client/`):

```bash
cd client
npm run check              # svelte-check + tsc — the real gate; must be clean
npm run test -- --run      # vitest one-shot
```

`npm run check` is the primary quality gate: it type-checks `.ts`, `.js`, and `.svelte` (`checkJs` is enabled, so JS is checked too).

**Baseline is not clean.** A pristine checkout of upstream `main` (commit 5eeaba1) reports **13 errors and 113 warnings in 57 files**. Judge your change by *delta* against that baseline, not by a zero-error run. Pre-existing error sites (10 files):
`components/base/dnd/DragAndDropContainer.svelte` (x2), `components/dashboard/sidebar/edit/EditWidgetSidebar.svelte`, `components/goal/editor/number/NumberGoalCondition.svelte`, `components/trackable/edit/EditTrackableModal.svelte`, `components/variable/edit/calculation/EditCalculationVariable.svelte`, `model/form/suggestions.ts` (x2), `model/trackable/suggestions.ts`, `services/encryption/encryption.ts`, `services/integration/local.ts` (x2), `stores/dashboard/widget/goal.ts`.
Most warnings are Svelte "Unused CSS selector" noise. Capture a fresh baseline with `npm run check 2>&1 | tail -3` before starting if these numbers look stale.

Tests live in `client/tests/` (**not** colocated in `src/`). Baseline: **19 files, 103 tests, all passing** (~2s). Unlike `npm run check`, this one must stay green.

Coverage is concentrated in two areas and near-zero elsewhere:
- `tests/graph/*` — variable graph: `graph, edit, filter, group, goal, goalStreak, latest, tag`
- `tests/analytics/*` — `basic, correlation, history, insights, raw, tags, weekday`
- plus `primitive`, `time-scope`, `simple-time`, `journal/search`

So: changes to `services/variable/graph.ts`, `services/analytics/*`, `model/primitive`, or time-scope code are covered — run the suite. UI (`components/`, `views/`), stores, sync, encryption, and integrations have **no tests at all**; verify those by hand in the running app.

After any server change (`server/rust/`):

```bash
cd server/rust
cargo fmt --all && cargo clippy --all-targets && cargo test --workspace
```

Baseline: the workspace builds clean, clippy is warning-free, and **72 unit
tests pass** (46 of them in `integration`). These cover the pieces testable
without infrastructure — encryption round-trips, PKCE against the RFC 7636
vector, path aggregators, timezone handling, cron normalisation, uuid shapes,
password hashing, validation.

The unit tests are *not* the real gate. That is the black-box end-to-end suite
in `server/e2e/`: **247 pytest tests** against the running HTTP stack (~3m34s
cold; needs Docker, the Rust toolchain and uv):

```bash
cd server/e2e && uv venv && uv pip install -e .
.venv/bin/pytest                 # all 247; --keep-stack for faster reruns
```

It includes hypothesis property tests, a stateful model of the sync protocol,
and 3 `characterization`-marked tests pinning surprising behaviour. Run it for
any change to auth, sync, integration or gateway *behaviour*. See
`mem:server/e2e_tests`.

After touching `crates/common/` or `crates/proto/`, everything downstream
rebuilds automatically — but the e2e suite is still what tells you whether the
behaviour moved.

Before declaring a UI change done, run `npm run dev` and exercise the flow — most of the app has no test coverage.
