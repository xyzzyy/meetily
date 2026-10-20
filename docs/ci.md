# Pull request build checks

`.github/workflows/pr-build.yml` runs for pull requests to `main`, pushes to
`main`, and manual requests from the Actions tab.

The workflow has two checks:

- **Frontend build and tests** installs the locked dependencies, runs each test
  file in its own process, and builds the production frontend on Ubuntu.
- **macOS release build and test compilation** builds the summary helper, the
  speaker helper when present, and the app on an Apple Silicon Mac. It also
  compiles the Rust unit tests.

Rust tests are compiled but are not run by this workflow. Some existing tests
need audio devices or system permissions. The workflow does not test Windows
or Linux native builds, app packaging, signing, or notarization. Use the
existing manual build workflows for those build steps.

The checks use a read-only GitHub token and no repository secrets. They do not
publish releases or install the app. Frontend output is kept for three days
so the native job can use it. A new commit cancels an older run for the same
pull request.

To run the frontend checks locally, install Node.js 22, pnpm 9.15.9, and
Bun 1.3.14. Then run:

```sh
cd frontend
pnpm install --frozen-lockfile
cd ..
bash scripts/test-frontend.sh
cd frontend
pnpm build
```

To compile the native app and tests, use the macOS job steps in the workflow.
The frontend output and the helper binaries must be built first.
