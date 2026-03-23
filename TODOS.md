# TODOS

## Test isolation for global config
**What:** Set `HOME` to a temp dir in integration tests so `~/.auto-push.json` doesn't leak into test results.
**Why:** Once the binary reads global config, test results depend on the runner's home directory. Without isolation, CI passes locally but fails (or vice versa) depending on whether the developer has a global config.
**Depends on:** config.rs implementation.

## Resolve `.gitignore` vs team-shareable config tension
**What:** Decide whether `.auto-push.json` should be gitignored (personal) or committed (team). Consider splitting into `.auto-push.json` (team, committed) + `.auto-push.local.json` (personal, gitignored), similar to git's `.gitconfig` vs `.git/config` pattern.
**Why:** Auto-init currently gitignores the config, but teams who want to share commit style conventions need to commit it. Both the adversarial review and Codex flagged this tension.
**Depends on:** Feedback from real users after initial release.
