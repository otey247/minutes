# Minutes local OpenAI plugin prototype

The repository marketplace at `.agents/plugins/marketplace.json` packages five
existing Minutes skills: search, prep, debrief, recap and weekly review. The
portable plugin lives at `.agents/plugins/minutes`. It uses the published local
stdio MCP server `minutes-mcp@0.27.0` and keeps helper paths independent of the
user's working directory. Node/npm and a compatible Minutes CLI are required;
v0.27.0 is the qualification target. Older than v0.25.0 fails closed.

From a checkout containing this prototype, register the repository's absolute
path, install the plugin, and restart Codex:

```bash
codex plugin marketplace add /absolute/path/to/minutes --json
codex plugin add minutes@minutes --json
codex plugin list --marketplace minutes --json
```

This opt-in installation connects to the user's local Minutes library, subject
to Minutes' existing meeting-access rules. It does not upload a library to a
hosted Minutes service. When an AI host uses retrieved records as context, its
own provider receives that context according to the host's settings. The
plugin's presence is not consent to external sending or mutation.

For the sample-only qualification below, installation into your normal Codex
profile is unnecessary. It uses a temporary corpus containing the five public
synthetic fixtures, an isolated Minutes config, and the actual published MCP
package. It verifies search, the sourced pricing reversal, and denial of an
outside file. It makes no model calls. The published MCP chooses installed CLI
paths before PATH, so verify the installed engine version. On Linux with
bubblewrap, `MINUTES_QUALIFICATION_BIN` can overlay a separately downloaded,
checksum-verified CLI into the test child without changing the host installation.

```bash
cd integrations/openai-plugin
npm ci --ignore-scripts
npm run qualify
# Optional Linux isolation when the installed CLI is older:
MINUTES_QUALIFICATION_BIN=/absolute/path/to/minutes-v0.27.0 npm run qualify
```

The same demo has a [60-second walkthrough](demo.md) and a
[local illustrated preview](demo.html). The preview is explicitly a prepared
sample answer; it is not a recording of live model output.

Canonical skill sources remain under `tooling/skills/sources`. Regenerate this
package along with the existing host surfaces:

```bash
cd tooling/skills
npm run build
npm run compile
npm run test
npm run check
npm run compile:dry
npm run golden
```

The package is a local distribution prototype. OpenAI's public plugin
submission currently requires a remote HTTPS MCP endpoint; the local stdio
manifest does not establish ChatGPT directory eligibility. A hosted endpoint
would need explicit per-user authorization and preserve restricted-meeting
policy before publication. Current receipts are in
[qualification.md](qualification.md).

Official sources checked September 30, 2026:
[packaging](https://developers.openai.com/plugins/build/plugins) and
[submission](https://developers.openai.com/plugins/deploy/submission).
