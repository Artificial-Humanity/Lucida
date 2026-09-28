## What this changes

<!-- The change and why. Name anything a user or an agent session will notice:
     CLI flags, exit codes, MCP tool schemas or descriptions. -->

## Verification

- [ ] `cargo test`
- [ ] `cargo clippy --all-targets -- -D warnings`
- [ ] `scripts/smoke.sh <binary>`
- [ ] `scripts/canary.sh <binary>`, if a provider lane changed
- [ ] Review dispatched and its findings addressed ([WORKFLOW.md](../WORKFLOW.md))
