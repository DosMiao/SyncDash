# user edit only
# user edit only
# user edit only
# Do not Edit If you are AI AGENT, unless user give you clear instruction to do so, and you have double checked with user about the instruction before editing.


Full app wide swap for redundant code, logic, using parallel agent
1. clean code, check my code find potential item to be cleaned, like: one time code (like database mitigation, and etc), unused code which is in npx tauri warning (and we shouldn't use too much of #[allow(dead_code)] #[allow(unused_imports)]); 
2. Remove un-necessary barrel files, proxy code，re export code，mod.rs(if not necessary), etc. 
3. clean developing comment
4. find codes with similar functionality
5. find duplicated code



优化代码

Full app wide swap using parallel agent:
1. give me a plan to optimize the codebase — it requires an in-depth understanding first, then splitting and refactoring.
2. I prefer a more hierarchical rather than flat code structure.
3. Avoid single files being overly long/short unless absolutely necessary. Code file can be extensive long/short if needed, but not encoraged.
4. Look for code that should be relocated out to other locations, and also look for code from other locations that should be moved in.
5. Prefer unidirectional dependencies with clear layering; reverse references are acceptable when necessary — don't force their elimination; avoid barrel files.



Review and refine the plan phase by phase again, make sub-phase plan if needed.
And then you can proceed the entire plan


## Master plan prompt
Read master plan, plan to process the Sub-Plan 5 of master plan. before you make revise, please check note and user's want. if you are planing to do something different from the note and user's want,  you can do it, but you need to double check to user before process
here is the master plan, .docs\devPlan\03-25 rust_framework_upgrade.md, here is the note for it .docs\devPlan\03-25 rust_framework_note.md, here is the original user's want .docs\devPlan\User demand\03-25 streaming chart service.md.
Additional info can be found here (but they are not assured to be right)
.docs\devPlan\03-25 Streaming_Chart_Master_Plan.md
.docs\devPlan\03-25 melodic-exploring-island.md
.docs\devPlan\03-25 tender-sauteeing-lark.md

# Challeng thinking 
Please hire 5 sub-agents simultaneously (3 aggressive/innovative, 1 neutral/moderate, and 1 conservative/pragmatic). 
Each one should provide a full round of opinions on the requests. 
Then initiate 3 rounds of cross-discussion, where all five agents' opinions are mutually consulted and debated. 
Ultimately, for each point, produce 3 sets of proposals (aggressive (innovative, and for the long run), neutral, conservative — I personally lean toward the aggressive/innovative side, hoping for fundamental fixes/reforms to the code rather than superficial patches; of course, if something is both simple and effective, that's even better). 
Then hire 3 additional review agents to audit the resulting A/B/C proposals, explain the functionality, pros, and cons of each to me, and finally settle on one definitive plan.


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
4. Provide me with a list of the modifications you plan to make, including a comparison with the original implementation. Explain to me use show_widget tool.
5. Please carry out detailed planning.

# Refactor/New function add
Please:
1. Research the relevant code to understand the current external API, the data available (Internal and external), and the infa we have now; understand the functionalities we already have in relevant modules, and the diff between current status and the things we are adding or refactoring.
2. For the things we are adding or refactoring, design it from first principles and consider how to embed, integrate, or reorganize the existing modules to fit it in.
3. Research the code and carefully consider what needs to be modified, upgraded, or adjusted in terms of architecture, data, UI, and pipeline.
4. Provide me with a list of the modifications you plan to make, including a comparison with the original implementation. Explain to me use show_widget tool.
5. Please carry out detailed planning.

# modification improve
1. Regarding the modifications you have made/are going to make: please ensure the changes are clean, complete, and logically coherent, rather than just "bridging" old code. If necessary, I want a restructuring of the code architecture to create a cleaner, more seamless design. Avoid taking shortcuts—such as using proxies—simply to save effort; what I require is a clean upgrade, migration, or refactoring.
2. Check all related code file's name, the function's name, the variable's name, label， token, comments, and every the naming issue, namespace issue. Make sure they are intuitive and consistent are updated accordingly. 
3. Review the relevant logic trees; I need clear logical flows and structures—avoid creating "logic black holes."
4. If the revisions are already complete, please explain your revise reasoning: what exactly did you modify, and why are those changes effective? If you are going to revise, show me your plan using Chinese. Please show me with show_widget tool.
5. Review the comment you added per D:\Code\Financial\AlexQuant\.docs\Skills\comment_policy.md. 
6. Review out of date code, comments, claude memory, and documentation to ensure everything reflects the current implementation and planned modifications.
7. After check, commit the code you revised, by follow D:\Code\Financial\AlexQuant\.docs\Skills\phased_commit.md


## Auto Commit Plan
commit by follow D:\Code\Financial\AlexQuant\.docs\Skills\phased_commit.md

check/merge worktrees /commit the mian, follow D:\Code\Financial\AlexQuant\.docs\Skills\worktree_merge.md

What I should test for this phase

1.fact check； 2. 告诉我方案的主要内容是什么；3. 评价

all as your recommended, please execute all phases, commit per phase done by follow D:\Code\Financial\AlexQuant\.docs\Skills\phased_commit.md, non stop until all done

Please don't stop, and don't ask me any questions—just proceed exactly as you recommended. I'm going to sleep now and won't be answering any questions; I trust your choices.


do second round fact check. you investigate what My evidence give to you from the very beginning, and investigate in detailed, double confirm the root cause finding

& "$env:LOCALAPPDATA\CLIProxyAPI\cli-proxy-api.exe" -config "$env:USERPROFILE\.cli-proxy-api\config.yaml"

audit over comment cross all part over my code D:\Code\AlexQuant\Dev\Desktop, by following my standard set in claude.md and memory

 用中文+ASCII图向我解释，你原因是什么，你修改了什么，为什么你的修改有用


上个版本在这里 m83524a57f1ebc0914e109605100420ed8f8eff37，写一个release log, 说明从这个版本到现在，介绍增/删了什么功能，修复了那些bug（大略就行）

To DO:

Dataservice里面endpoints的修复


Holding pipeline, overview vs holding table , unsubscribe(stop dumbping holding table update when not on holding table)

Watchlist页面的渲染修复/效率提升

Performace panel re-design



I am trying to upgrade my app's liquid glass for a real visual effect:
1. target visual is this project .libs\liquid-glass-studio-main. I want to mitigate every page container, floating panel container, dropdownlist's conatiner and etc all be will be that effect
2. now there is alreay a large portion of some revise done, but the rendering is so weird and actually broken my visual, I dont think the current implementation is correct.
3. Please consider if current architecture is good or bad, we may need change arch if this arch is not suitable for compatible of both windows (intel chip) and mac (m4 chip).
