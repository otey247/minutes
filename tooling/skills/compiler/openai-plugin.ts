import path from "node:path";
import { readFile } from "node:fs/promises";
import type { CanonicalSkillSource } from "../schema.js";
import { codexHost } from "../hosts/codex.js";
import { renderSkillForHost } from "./render.js";
import { resolveSkillAssetSourcePath } from "./validate.js";

export const OPENAI_PLUGIN_ROOT = ".agents/plugins/minutes";
export const OPENAI_SKILLS = ["minutes-search", "minutes-prep", "minutes-debrief", "minutes-recap", "minutes-weekly"];

export async function renderOpenAIPlugin(rootDir: string, skills: CanonicalSkillSource[]): Promise<Map<string, string>> {
  const artifacts = new Map<string, string>();
  const json = (value: unknown) => `${JSON.stringify(value, null, 2)}\n`;
  artifacts.set(`${OPENAI_PLUGIN_ROOT}/plugin.json`, json({
    $schema: "https://agent-plugins.org/schemas/1.0.0/plugin.schema.json",
    name: "minutes", version: "0.1.0",
    description: "Conversation memory you own. Prepare, find decisions, and draft follow-ups from authorized local Minutes records.",
    author: { name: "Mat Silverstein", url: "https://github.com/silverstein" },
    homepage: "https://useminutes.app", repository: "https://github.com/silverstein/minutes", license: "MIT",
    keywords: ["meetings", "conversation-memory", "local-first", "minutes"],
    extensions: { "com.openai": { interface: { displayName: "Minutes", shortDescription: "Your conversations. Your memory. Ready for the AI you use." } } },
  }));
  artifacts.set(`${OPENAI_PLUGIN_ROOT}/mcp.json`, json({
    $schema: "https://agent-plugins.org/schemas/1.0.0/mcp.schema.json",
    mcpServers: { minutes: { type: "stdio", command: "npx", args: ["-y", "minutes-mcp@0.27.0"] } },
  }));
  artifacts.set(".agents/plugins/marketplace.json", json({
    name: "minutes", interface: { displayName: "Minutes · local conversation memory" },
    plugins: [{ name: "minutes", source: { source: "local", path: `./${OPENAI_PLUGIN_ROOT}` },
      policy: { installation: "AVAILABLE", authentication: "ON_INSTALL" }, category: "Productivity" }],
  }));

  for (const name of OPENAI_SKILLS) {
    const source = skills.find(skill => skill.frontmatter.name === name);
    if (!source) throw new Error(`OpenAI package requires canonical skill ${name}`);
    const rendered = renderSkillForHost(source, codexHost);
    const outputDir = `${OPENAI_PLUGIN_ROOT}/skills/${name}`;
    // A downloaded plugin need not be inside a Git repository. Resolve helpers
    // from the installed skill location, never from the user's working tree.
    const rootNote = `## Skill Path\n\nResolve this skill's installed SKILL.md path from the host's skill metadata.\nSet MINUTES_SKILL_ROOT to that file's absolute parent directory and\nMINUTES_SKILLS_ROOT to the absolute parent of MINUTES_SKILL_ROOT. Do this before\nrunning a helper. Do not infer either path from the current working directory\nor a Git checkout. The bundled runtime is under MINUTES_SKILLS_ROOT/_runtime.\n\n`;
    const body = rendered.body.replace(/## Skill Path\n\nBefore running helper scripts or opening bundled references, set:\n\n```bash\n[\s\S]*?```\n\n/, rootNote);
    if (body.includes('$(git rev-parse --show-toplevel)')) throw new Error(`Unresolved repository path in packaged skill ${name}`);
    artifacts.set(`${outputDir}/SKILL.md`, `${body.trimEnd()}\n`);
    for (const sidecar of rendered.sidecarFiles) {
      artifacts.set(path.join(outputDir, path.relative(path.dirname(rendered.outputPath), sidecar.relativePath)), sidecar.content);
    }
    for (const asset of rendered.assetFiles) {
      const absolute = await resolveSkillAssetSourcePath(source, asset.sourceRelativePath);
      artifacts.set(path.join(outputDir, asset.sourceRelativePath), await readFile(absolute, "utf8"));
    }
  }
  for (const name of ["minutes-learn.mjs", "minutes-learn-cli.mjs"]) {
    artifacts.set(`${OPENAI_PLUGIN_ROOT}/skills/_runtime/hooks/lib/${name}`, await readFile(path.join(rootDir, "..", "..", ".claude/plugins/minutes/hooks/lib", name), "utf8"));
  }
  return artifacts;
}
