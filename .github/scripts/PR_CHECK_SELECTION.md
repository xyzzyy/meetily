PR checks use the files changed from the PR branch's merge base. The selector
uses a local Git diff with NUL-separated paths. It includes deleted paths and
both paths of a move. It does not use GitHub's limited changed-file filter.

| Changed files | Selected checks |
|---|---|
| Root README, AGENTS, licence, docs, issue templates | Quick Node checks |
| Benchmark code or data | Quick checks and benchmark tests |
| Frontend unit tests | Quick checks and frontend unit tests |
| Frontend source | Frontend unit tests, production build, browser tests and walkthrough |
| Browser tests or Playwright configuration | Frontend build, browser tests and walkthrough |
| Native app source | Frontend build, all helper artifacts, Rust tests, app build and launch test |
| Helper source | Native app checks and that helper's unit tests |
| Dependencies, build scripts, CI, or an unknown path | All checks |

Quick Node checks always run. They include the selection tests. Mixed changes
select the union of their checks. A native build always gets the frontend and
all helper artifacts. Browser checks always get the frontend artifact.

Pushes to main and manual runs select all checks. If the diff cannot be read,
the selector also selects all checks. If selection itself fails, the final
result fails. The `PR checks` job runs after the other jobs and requires each
selected job to succeed. A skipped or cancelled selected job is a failure.
Use `PR checks` as the required repository check when enabling this selection.
Individual matrix checks can be absent when their job is not selected.
Repository branch protection is not changed by this code.

This reduces the checks for known isolated changes. It does not select individual
Rust or frontend test cases, and does not estimate saved runner minutes. New
paths select all checks until a mapping is added. The manual build and AI review
workflows are unchanged.

Run the selection tests and workflow check:

```sh
node --test .github/scripts/select-pr-checks.test.cjs
actionlint .github/workflows/pr-build.yml
```
