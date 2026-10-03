Analyze this Claude Code usage data and suggest improvements.

## FEATURES REFERENCE (pick from these for features_to_try):
1. **MCP servers** — /mcps
   - How to use: Run `/mcps` to list and manage Model Context Protocol servers.
   - Good for: connecting to external tools, databases, and APIs
2. **Skills** — /<skill-name>
   - How to use: Run `/skills` to list installed skills, then type `/<skill-name>` to run one.
   - Good for: repetitive workflows and multi-step tasks
3. **Workflows** — /workflow
   - How to use: Run `/workflow` to list and run saved workflows.
   - Good for: deterministic multi-step orchestration
4. **Subagents** — /workflow
   - How to use: Use `/workflow` fan-out — this build's closest analogue to parallel subagents.
   - Good for: parallel exploration of large codebases
5. **Plugins** — /plugins
   - How to use: Run `/plugins` (alias `/plugin`) to manage plugins.
   - Good for: bundling skills, hooks, and commands
6. **Headless** — grok -p "PROMPT" --allow RULE
   - How to use: `grok -p "fix lint errors" --allow "Edit,Read,Bash"` (`--single`, `--deny`, and `--always-approve` are also declared; `--allowedTools`/`--disallowedTools` are compat aliases, `--yolo` aliases `--always-approve`).
   - Good for: CI/CD integration, batch fixes, automated reviews
7. **Hooks** — /hooks
   - How to use: Run `/hooks` to view and manage lifecycle hooks.
   - Good for: auto-formatting, type checks, enforcing conventions

RESPOND WITH ONLY A VALID JSON OBJECT:
{
  "claude_md_additions": [
    {"addition": "A specific line or block to add to AGENTS.md based on workflow patterns. E.g., 'Always run tests after modifying auth-related files'", "why": "1 sentence explaining why this would help based on actual sessions", "prompt_scaffold": "Instructions for where to add this in AGENTS.md. E.g., 'Add under ## Testing section'"}
  ],
  "features_to_try": [
    {"feature": "Feature name from CC FEATURES REFERENCE above", "one_liner": "What it does", "why_for_you": "Why this would help YOU based on your sessions", "example_code": "Actual command or config to copy"}
  ],
  "usage_patterns": [
    {"title": "Short title", "suggestion": "1-2 sentence summary", "detail": "3-4 sentences explaining how this applies to YOUR work", "copyable_prompt": "A specific prompt to copy and try"}
  ]
}

IMPORTANT for claude_md_additions: PRIORITIZE instructions that appear MULTIPLE TIMES in the user data. If user told Claude the same thing in 2+ sessions (e.g., 'always run tests', 'use TypeScript'), that's a PRIME candidate - they shouldn't have to repeat themselves.

IMPORTANT for features_to_try: Pick 2-3 from the FEATURES REFERENCE above. Include 2-3 items for each category.

DATA:
{SESSION_DATA}
