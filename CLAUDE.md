@AGENTS.md
## Shared Agent Skills

- The canonical shared catalog is `/Users/xuanbomiao/Code/Experience/agent-skills` on macOS and `D:\Code\Experience\agent-skills` on Windows. Do not infer its location relative to this project.
- Read `CATALOG.md` in that catalog and refer to workflows by canonical skill name; `skills/` is source, while `dist/` is generated host output.
- Codex discovers user skills in `~/.agents/skills` on macOS or `%USERPROFILE%\.agents\skills` on Windows, and project skills in `<project-root>/.agents/skills`.
- Claude Code discovers user skills in `~/.claude/skills` on macOS or `%USERPROFILE%\.claude\skills` on Windows, and project skills in `<project-root>/.claude/skills`.
- Git synchronization updates the catalog checkout only; it does not install discovery entries. Use `python3 scripts/install_skills.py --host <codex|claude> --mode symlink` on macOS and `py -3 scripts\install_skills.py --host <codex|claude> --mode copy --update` on Windows from the catalog root.
- Invoke a shared skill explicitly as `$skill-name` in Codex or `/skill-name` in Claude Code.
- A documented path or skill mention is not standing authorization to install, generate, commit, merge, rebase, push, deploy, or perform another mutation.
