# OpenAI distribution qualification — September 30, 2026

Owner: Codex working for Mat. Beads epic: `minutes-rt6k`; prototype tasks
`minutes-rt6k.1` and `.2`; live acceptance task `.3`.

Owned checkout: `/home/mat/Sites/minutes-worktrees/openai-distribution`, branch
`feat/openai-distribution`, from `origin/main` at
`5fbe77b6d1f406286e8fa985deb65a9d6affdea0`. Purpose: the first bounded OSS
ChatGPT-plan/plugin distribution tranche. The checkout is retained for review
and live qualification. The dirty canonical checkout was preserved.

| Proof | Current evidence |
| --- | --- |
| OAuth source/protocol | Standalone prototype; 24 tests pass. Tokens, transport responses and signing keys in tests are synthetic. |
| Skill source/compiler | Five canonical skills packaged; 36 compiler tests pass, with routing, resolver, ownership, generated-output and golden checks. |
| Codex discovery | CLI 0.159.1 lists `minutes@minutes` v0.1.0 as available using a local marketplace config override. |
| Codex installation | `plugin add` succeeds and `plugin list` shows installed/enabled in an isolated bubblewrap profile. The user's normal Codex profile was not enabled. |
| Published MCP runtime | `minutes-mcp@0.27.0` plus checksum-verified released Linux CLI v0.27.0 passes a real stdio sample-corpus check: five meetings, two pricing sources, current reversal, outside-corpus denial. No model calls or real meeting reads. |
| Existing Silvercloud profile | Initial MCP qualification fails closed on installed CLI v0.18.0. A v0.27.0 child using existing host state then reports unconfirmed legacy QMD cleanup. No host engine upgrade or QMD repair was performed. |
| Sample sandbox | Qualification overlays separate sample config/state and released CLI only inside the child filesystem. This is an isolated sample-install receipt, not proof the existing host profile's QMD issue is resolved. |
| Browser sign-in | Launched on silverbook in a separate prototype directory. The ten-minute window expired without a verified callback. No live identity or inference receipt; a fresh browser attempt is required when the user is available. |
| Attention asset | Prepared sample illustration and 60-second walkthrough. Browser rendering, both source disclosures, source link destinations and mobile width checked with agent-browser. The illustration explicitly labels its answer as prepared. No public posting or attention result. |
| Native desktop / public directory | Neither is activated by this tranche. A public submission needs a remote HTTPS MCP endpoint and acceptance; the local package does not meet that distribution gate. |

The separate browser-qualification copy is
`/Users/silverbook/Sites/minutes-openai-qualification.4FCLga`; its credentials, if
consent completes, stay in the documented prototype store. It uses Node 22.23.2
and loopback port 18765. The expired listener has stopped. Do not collect tokens
or callback URLs as evidence.

Live acceptance requires verified sign-in, catalog discovery, one completed
sample response with correct dated citations, successful logout/revocation, and
restart/refresh behavior. Account eligibility and quota errors remain failures;
no provider/key substitution establishes this proof. Native meeting access
requires separate parity with Minutes' existing restricted-meeting and capture
isolation rules before desktop integration.

Public attention should be measured as attributable repository visits, completed
installs, first sourced answers and repeat use. No reach or adoption claim is
supported by the technical receipts above.
