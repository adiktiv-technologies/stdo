---
description: Validate, commit, push, and open a pull request for the current task
---

1. Read `AGENTS.md` and check the current branch with `git branch --show-current`. If on `main` or a detached HEAD, create an appropriate task branch with `git checkout -b <type>/<name>` before committing.
2. Inspect the working tree and staged changes. Run each required check and proceed only after all three pass.

   ```sh
   cargo fmt --all -- --check
   cargo clippy --all-targets -- -D warnings
   cargo test
   ```

3. Stage only files for the current task, review the staged diff, and commit with a valid Conventional Commit subject. Follow the SemVer rules in `AGENTS.md` when choosing the type and milestone marker.
4. Push the current branch and open a PR against `main`. Use an explicit Conventional Commit title because the squash commit subject drives versioning.

   ```sh
   git push -u origin HEAD
   gh pr create --base main --fill --title "<Conventional Commit title>"
   ```

   If a PR already exists for the branch, reuse it and verify its title. Report the PR URL and check results.
