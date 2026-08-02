# user edit only
# user edit only
# user edit only
# Do not Edit If you are AI AGENT, unless user give you clear instruction to do so, and you have double checked with user about the instruction before editing.

## Shared Agent Skills

- Refer to reusable workflows by canonical skill name, never by a filesystem path.
- Explicit invocation uses `$skill-name` in Codex and `/skill-name` in Claude Code.
- This note uses `plan-complex-work`, `refine-phase-plan`, `execute-phase-plan`, `review-change`, `create-phased-commits`, and `integrate-git-worktrees`.
- A persistent note or a skill mention does not grant standing permission to implement, commit, integrate, rebase, push, clean up, or perform another side effect. The current user request must authorize each mutation.


Full app wide swap for redundant code, logic, using parallel agent
1. clean code, check my code find potential item to be cleaned, like: one time code (like database mitigation, and etc), unused code which is in npx tauri warning (and we shouldn't use too much of #[allow(dead_code)] #[allow(unused_imports)]); 
2. Remove un-necessary barrel files, proxy code，re export code，mod.rs(if not necessary), etc. 
3. clean developing comment
4. find codes with similar functionality
5. find duplicated code



优化代码

For a full-app optimization plan, use `plan-complex-work` in adaptive parallel-decomposition mode:
1. give me a plan to optimize the codebase — it requires an in-depth understanding first, then splitting and refactoring.
2. I prefer a more hierarchical (tree-like) rather than flat code structure.
3. Avoid single files being overly long/short unless absolutely necessary. Code file can be extensive long/short if needed, but not encoraged.
4. Look for code that should be relocated out to other locations, and also look for code from other locations that should be moved in.
5. Prefer unidirectional dependencies with clear layering; reverse references are acceptable when necessary — don't force their elimination; avoid barrel files.

Use `refine-phase-plan` to review and refine the selected plan phase by phase; add subphases only when needed.
Use `execute-phase-plan` only after the current user request explicitly authorizes implementation of that identified plan.


## Master plan prompt
Use `refine-phase-plan` for the specific master plan selected in the current request.
Resolve the master plan, its note, and the original user request relative to the repository root, and verify each path exists before relying on it.
Treat additional plans as unverified context until fact-checked; never reuse a path copied from another project.

# Complex planning
Use `plan-complex-work`.
Scale independent lanes and challenge rounds to problem uncertainty, blast radius, ownership boundaries, and available capacity.
When materially different strategies exist, compare aggressive/innovative, neutral/moderate, and conservative/pragmatic options. I generally prefer fundamental fixes, while accepting a simple approach when it is both clean and effective.
Use independent review only when it can materially change the recommendation.


## Deep Dive
Please:
1. Consider deeper and wider, understand the idea, and double confirm with me if you have any question or suggestion.
2. Consider from a first-principles perspective. What i need is a clean, well-structured code revision, that lead to a more maintainable and scalable codebase. 
3. This is not fix or patch, you can do a fundamental revise or totally rebuild if needed.
4. Don't afraid of revise more, please make the logic more straightforward so we max the maintainable, don't use those confusing proxy.

# redesign the realized code
Please:
1. Research the relevant code to understand the already realized functionalities
2. Set the goal as the functionality to present, dont consider what we have implemented now, but focus on the desired outcome: redisgn it from the first principles, to achieve the best result, what we will organize it?
3. reseach the the current external API, the data available (Internal and external), and the infa we have now. consider the job more realistic, and the diff between current status and the things we are adding or refactoring.
4. Provide me with a list of the modifications you plan to make, including a comparison with the original implementation. Use an available visualization, Mermaid diagram, or ASCII diagram only when it materially clarifies the comparison.
5. Please carry out detailed planning.

# Refactor/New function add
Please:
1. Research the relevant code to understand the current external API, the data available (Internal and external), and the infa we have now; understand the functionalities we already have in relevant modules, and the diff between current status and the things we are adding or refactoring.
2. For the things we are adding or refactoring, design it from first principles and consider how to embed, integrate, or reorganize the existing modules to fit it in.
3. Research the code and carefully consider what needs to be modified, upgraded, or adjusted in terms of architecture, data, UI, and pipeline.
4. Provide me with a list of the modifications you plan to make, including a comparison with the original implementation. Use an available visualization, Mermaid diagram, or ASCII diagram only when it materially clarifies the comparison.
5. Please carry out detailed planning.

# modification improve
1. Regarding the modifications you have made/are going to make: please ensure the changes are clean, complete, and logically coherent, rather than just "bridging" old code. If necessary, I want a restructuring of the code architecture to create a cleaner, more seamless design. Avoid taking shortcuts—such as using proxies—simply to save effort; what I require is a clean upgrade, migration, or refactoring.
2. Check all related code file's name, the function's name, the variable's name, label， token, comments, and every the naming issue, namespace issue. Make sure they are intuitive and consistent are updated accordingly. 
3. Review the relevant logic trees; I need clear logical flows and structures—avoid creating "logic black holes."
4. If the revisions are already complete, explain what you modified, why, and why the result is effective. If you are planning revisions, explain the plan in Chinese and use an available visualization only when it materially improves clarity.
5. Review every comment added or touched using the `review-change` skill's comment-audit policy.
6. Review outdated code, comments, persistent agent memory, and documentation to ensure everything reflects the current implementation and planned modifications.
7. After review, create only the currently authorized scoped local commits using `create-phased-commits`.


## Commit and worktree plan
Create explicitly authorized scoped local commits using `create-phased-commits`.

Audit and integrate committed worktree branches using `integrate-git-worktrees`; handle any explicitly authorized uncommitted scope separately with `create-phased-commits`.

What I should test for this phase

1.fact check； 2. 告诉我方案的主要内容是什么；3. 评价

Before execution, re-read the current repository state and fact-check the plan; an idle interval does not authorize a rebase or prove that the worktree is stable.
When the current request explicitly authorizes full implementation, use `execute-phase-plan` to complete the approved phases and their validation gates.
When the current request explicitly authorizes commits, create scoped commits for completed phases using `create-phased-commits`.

Proceed autonomously within the current request's explicit scope; stop when new authority or a material user decision is required.


do second round fact check. you investigate what My evidence give to you from the very beginning, and investigate in detailed, double confirm the root cause finding

## Windows-only local command

& "$env:LOCALAPPDATA\CLIProxyAPI\cli-proxy-api.exe" -config "$env:USERPROFILE\.cli-proxy-api\config.yaml"

Audit comments across the current repository's `Dev` tree using the applicable `AGENTS.md` or `CLAUDE.md` instructions and the `review-change` comment-audit policy.

 用中文+ASCII图向我解释，你原因是什么，你修改了什么，为什么你的修改有用


上个版本在这里 m83524a57f1ebc0914e109605100420ed8f8eff37，写一个release log, 说明从这个版本到现在，介绍增/删了什么功能，修复了那些bug（大略就行）
