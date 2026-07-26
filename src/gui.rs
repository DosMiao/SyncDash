//! GUI（参考 FFS 的核心交互）：任务下拉 → Compare → 差异表（勾选+方向徽章）→ Synchronize。
//! 后台线程跑扫描/执行，界面不卡；conflict/note 行默认不勾且不可勾。

use eframe::egui;
use std::sync::{Arc, Mutex};
use syncdash::compare::{reverse_op, Action, Op, Plan, Side};
use syncdash::config::{self, Job};
use syncdash::run;

#[derive(PartialEq, Clone, Copy)]
enum Phase {
    Idle,
    Comparing,
    Ready,
    Applying,
}

struct Shared {
    phase: Phase,
    status: String,
    plan: Option<Plan>,
    checked: Vec<bool>,
    /// FFS 式逐行翻方向：true = 该行执行 reverse_op 后的动作
    flipped: Vec<bool>,
}

pub struct App {
    jobs: Vec<(String, Job)>,
    sel: usize,
    shared: Arc<Mutex<Shared>>,
    editor: Option<EditorState>,
}

/// 任务编辑器（FFS 的"配置对话框"对应物）。全部字符串编辑，Save 时校验落盘。
struct EditorState {
    is_new: bool,
    name: String,
    mode: String,
    source: String,
    target: String,
    archive: String,
    rigor: String,
    symlinks: String,
    case_sensitive: bool,
    include: String,
    exclude: String,
    remote_host: String,
    remote_root: String,
    remote_exe: String,
    delete_armed: bool,
    error: Option<String>,
}

impl EditorState {
    fn new_blank() -> Self {
        EditorState {
            is_new: true,
            name: String::new(),
            mode: "sync".into(),
            source: String::new(),
            target: String::new(),
            archive: String::new(),
            rigor: "standard".into(),
            symlinks: "exclude".into(),
            case_sensitive: false,
            include: String::new(),
            exclude: String::new(),
            remote_host: String::new(),
            remote_root: String::new(),
            remote_exe: String::new(),
            delete_armed: false,
            error: None,
        }
    }
    fn from_job(name: &str, j: &Job) -> Self {
        EditorState {
            is_new: false,
            name: name.to_string(),
            mode: j.mode.clone(),
            source: j.source.to_string_lossy().into_owned(),
            target: j.target.to_string_lossy().into_owned(),
            archive: j.archive.as_ref().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default(),
            rigor: j.rigor.clone(),
            symlinks: j.symlinks.clone(),
            case_sensitive: j.case_sensitive,
            include: j.include.join("\n"),
            exclude: j.exclude.join("\n"),
            remote_host: j.remote_host.clone().unwrap_or_default(),
            remote_root: j.remote_root.clone().unwrap_or_default(),
            remote_exe: j.remote_exe.clone().unwrap_or_default(),
            delete_armed: false,
            error: None,
        }
    }
    fn to_job(&self) -> Result<Job, String> {
        let name = self.name.trim();
        if name.is_empty() || name.contains(['/', '\\']) {
            return Err("name is empty or contains path separators".into());
        }
        if self.source.trim().is_empty() || self.target.trim().is_empty() {
            return Err("source and target are required".into());
        }
        let lines = |s: &str| s.lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect::<Vec<_>>();
        let opt = |s: &str| {
            let t = s.trim();
            if t.is_empty() { None } else { Some(t.to_string()) }
        };
        Ok(Job {
            mode: self.mode.clone(),
            source: self.source.trim().into(),
            target: self.target.trim().into(),
            archive: opt(&self.archive).map(Into::into),
            include: lines(&self.include),
            exclude: lines(&self.exclude),
            no_hash: false,
            rigor: self.rigor.clone(),
            case_sensitive: self.case_sensitive,
            symlinks: self.symlinks.clone(),
            remote_host: opt(&self.remote_host),
            remote_root: opt(&self.remote_root),
            remote_exe: opt(&self.remote_exe),
        })
    }
}

enum EditorAction {
    Save,
    Cancel,
    Delete,
}

impl App {
    fn new(initial: Option<String>) -> Self {
        let jobs = config::load_all();
        let sel = initial
            .and_then(|n| jobs.iter().position(|(name, _)| name == &n))
            .unwrap_or(0);
        App {
            jobs,
            sel,
            editor: None,
            shared: Arc::new(Mutex::new(Shared {
                phase: Phase::Idle,
                status: format!("jobs dir: {}", config::jobs_dir().display()),
                plan: None,
                checked: Vec::new(),
                flipped: Vec::new(),
            })),
        }
    }
}

fn default_checked(op: &Op) -> bool {
    !matches!(op.action, Action::Conflict | Action::Note)
}

fn action_badge(op: &Op) -> (&'static str, egui::Color32) {
    let to_target = op.side == Side::Target;
    match op.action {
        Action::Copy => (if to_target { "-> copy" } else { "<- copy" }, egui::Color32::from_rgb(0x35, 0xb0, 0x4a)),
        Action::Update => (if to_target { "-> update" } else { "<- update" }, egui::Color32::from_rgb(0xd9, 0x9a, 0x22)),
        Action::Move => ("mv", egui::Color32::from_rgb(0x3a, 0x8f, 0xd9)),
        Action::Delete | Action::DeleteDir => ("DEL", egui::Color32::from_rgb(0xd9, 0x3a, 0x3a)),
        Action::Conflict => ("CONFLICT", egui::Color32::from_rgb(0xaa, 0x66, 0xcc)),
        Action::Note => ("note", egui::Color32::GRAY),
    }
}

fn human_size(b: u64) -> String {
    if b >= 1 << 30 {
        format!("{:.2} GB", b as f64 / (1u64 << 30) as f64)
    } else if b >= 1 << 20 {
        format!("{:.1} MB", b as f64 / (1u64 << 20) as f64)
    } else if b >= 1 << 10 {
        format!("{:.1} KB", b as f64 / (1u64 << 10) as f64)
    } else {
        format!("{b} B")
    }
}

fn setup_fonts(ctx: &egui::Context) {
    let candidates = [
        r"C:\Windows\Fonts\msyh.ttc",
        r"C:\Windows\Fonts\simhei.ttf",
        "/System/Library/Fonts/PingFang.ttc",
        "/System/Library/Fonts/Hiragino Sans GB.ttc",
    ];
    for c in candidates {
        if let Ok(bytes) = std::fs::read(c) {
            let mut fonts = egui::FontDefinitions::default();
            fonts.font_data.insert("cjk".into(), egui::FontData::from_owned(bytes).into());
            for fam in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
                fonts.families.entry(fam).or_default().push("cjk".into());
            }
            ctx.set_fonts(fonts);
            break;
        }
    }
}

pub fn run_gui(initial: Option<String>) -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([1150.0, 680.0]),
        ..Default::default()
    };
    eframe::run_native(
        "SyncDash",
        options,
        Box::new(move |cc| {
            setup_fonts(&cc.egui_ctx);
            Ok(Box::new(App::new(initial.clone())))
        }),
    )
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let mut do_compare = false;
        let mut do_apply = false;
        let mut open_editor: Option<Option<usize>> = None; // Some(None)=新建, Some(Some(i))=编辑第 i 个

        {
            let mut sh = self.shared.lock().unwrap();
            let busy = sh.phase == Phase::Comparing || sh.phase == Phase::Applying;

            egui::TopBottomPanel::top("top").show(ctx, |ui| {
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.heading("SyncDash");
                    ui.separator();
                    if self.jobs.is_empty() {
                        ui.label(format!("no jobs — put <name>.toml under {}", config::jobs_dir().display()));
                        if ui.button("New").clicked() {
                            open_editor = Some(None);
                        }
                    } else {
                        egui::ComboBox::from_id_salt("job")
                            .selected_text(self.jobs[self.sel].0.clone())
                            .show_ui(ui, |ui| {
                                for (i, (name, _)) in self.jobs.iter().enumerate() {
                                    ui.selectable_value(&mut self.sel, i, name);
                                }
                            });
                        let job = &self.jobs[self.sel].1;
                        ui.label(format!("[{}]", job.mode));
                        ui.separator();
                        if ui.add_enabled(!busy, egui::Button::new("Compare")).clicked() {
                            do_compare = true;
                        }
                        let can_sync = sh.phase == Phase::Ready && sh.plan.is_some();
                        if ui.add_enabled(can_sync, egui::Button::new("Synchronize")).clicked() {
                            do_apply = true;
                        }
                        ui.separator();
                        if ui.button("Edit").clicked() {
                            open_editor = Some(Some(self.sel));
                        }
                        if ui.button("New").clicked() {
                            open_editor = Some(None);
                        }
                        if busy {
                            ui.spinner();
                        }
                        if let Some(p) = &sh.plan {
                            let sel_n = sh.checked.iter().filter(|c| **c).count();
                            let bytes: u64 = p
                                .ops
                                .iter()
                                .zip(&sh.checked)
                                .filter(|(o, c)| **c && matches!(o.action, Action::Copy | Action::Update))
                                .filter_map(|(o, _)| o.size)
                                .sum();
                            ui.label(format!(
                                "{} op(s) | {} selected | {} to transfer | {} conflict(s)",
                                p.header.op_count,
                                sel_n,
                                human_size(bytes),
                                p.header.conflict_count
                            ));
                        }
                    }
                });
                if !self.jobs.is_empty() {
                    let job = &self.jobs[self.sel].1;
                    ui.monospace(format!("{}   ->   {}", job.source.display(), job.target.display()));
                }
                ui.add_space(4.0);
            });

            egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
                ui.label(sh.status.clone());
            });

            egui::CentralPanel::default().show(ctx, |ui| {
                let Shared { plan, checked, flipped, .. } = &mut *sh;
                if let Some(plan) = plan {
                    use egui_extras::{Column, TableBuilder};
                    TableBuilder::new(ui)
                        .striped(true)
                        .column(Column::exact(26.0))
                        .column(Column::exact(80.0))
                        .column(Column::remainder().at_least(280.0).clip(true))
                        .column(Column::remainder().at_least(160.0).clip(true))
                        .column(Column::exact(80.0))
                        .column(Column::remainder().clip(true))
                        .header(22.0, |mut h| {
                            for t in ["", "action", "path", "from", "size", "reason"] {
                                h.col(|ui| {
                                    ui.strong(t);
                                });
                            }
                        })
                        .body(|body| {
                            body.rows(20.0, plan.ops.len(), |mut row| {
                                let i = row.index();
                                let op = &plan.ops[i];
                                let is_flipped = flipped.get(i).copied().unwrap_or(false);
                                let eff = if is_flipped { reverse_op(op) } else { None };
                                let shown: &Op = eff.as_ref().unwrap_or(op);
                                row.col(|ui| {
                                    let enabled = default_checked(op);
                                    ui.add_enabled(enabled, egui::Checkbox::without_text(&mut checked[i]));
                                });
                                row.col(|ui| {
                                    // FFS 式：点动作徽章翻转方向
                                    let (txt, color) = action_badge(shown);
                                    let can_flip = reverse_op(op).is_some();
                                    let mut rich = egui::RichText::new(txt).color(color);
                                    if is_flipped {
                                        rich = rich.underline();
                                    }
                                    let resp = ui.add_enabled(can_flip, egui::Button::new(rich).small().frame(false));
                                    if can_flip {
                                        if resp.on_hover_text("click to flip direction").clicked() {
                                            flipped[i] = !flipped[i];
                                        }
                                    }
                                });
                                row.col(|ui| {
                                    ui.monospace(&op.path);
                                });
                                row.col(|ui| {
                                    ui.monospace(op.from.as_deref().unwrap_or(""));
                                });
                                row.col(|ui| {
                                    ui.label(op.size.map(human_size).unwrap_or_default());
                                });
                                row.col(|ui| {
                                    ui.label(&shown.reason);
                                });
                            });
                        });
                } else {
                    ui.centered_and_justified(|ui| {
                        ui.label("Select a job and press Compare");
                    });
                }
            });
        }

        if do_compare && !self.jobs.is_empty() {
            let (name, job) = self.jobs[self.sel].clone();
            let shared = self.shared.clone();
            let ctx2 = ctx.clone();
            {
                let mut sh = shared.lock().unwrap();
                sh.phase = Phase::Comparing;
                sh.plan = None;
                sh.checked.clear();
                sh.status = format!("comparing '{name}' (scanning both sides, hashing changed files)...");
            }
            std::thread::spawn(move || {
                let res = run::compare_job(&job);
                let mut sh = shared.lock().unwrap();
                match res {
                    Ok(plan) => {
                        sh.checked = plan.ops.iter().map(default_checked).collect();
                        sh.flipped = vec![false; plan.ops.len()];
                        sh.status = format!(
                            "'{name}': {} op(s), {} conflict(s). Review and press Synchronize.",
                            plan.header.op_count, plan.header.conflict_count
                        );
                        sh.plan = Some(plan);
                        sh.phase = Phase::Ready;
                    }
                    Err(e) => {
                        sh.status = format!("compare failed: {e}");
                        sh.phase = Phase::Idle;
                    }
                }
                ctx2.request_repaint();
            });
        }

        if do_apply {
            let (name, job) = self.jobs[self.sel].clone();
            let shared = self.shared.clone();
            let ctx2 = ctx.clone();
            let taken = {
                let mut sh = shared.lock().unwrap();
                let plan = sh.plan.take();
                let checked = std::mem::take(&mut sh.checked);
                let flipped = std::mem::take(&mut sh.flipped);
                sh.phase = Phase::Applying;
                sh.status = format!("synchronizing '{name}'...");
                plan.map(|p| (p, checked, flipped))
            };
            if let Some((plan, checked, flipped)) = taken {
                std::thread::spawn(move || {
                    let mut ops: Vec<Op> = Vec::new();
                    for (i, o) in plan.ops.iter().enumerate() {
                        if !checked.get(i).copied().unwrap_or(false) || !default_checked(o) {
                            continue;
                        }
                        if flipped.get(i).copied().unwrap_or(false) {
                            if let Some(r) = reverse_op(o) {
                                ops.push(r);
                                continue;
                            }
                        }
                        ops.push(o.clone());
                    }
                    let (done, skipped, errors) = run::apply_job(&job, &plan, &ops, None, false);
                    let mut sh = shared.lock().unwrap();
                    sh.status = format!(
                        "'{name}' done: {done} applied, {skipped} skipped, {errors} error(s). Press Compare to verify.",
                    );
                    sh.phase = Phase::Idle;
                    ctx2.request_repaint();
                });
            }
        }

        // ---------- 任务编辑器（FFS 配置对话框的对应物） ----------
        if let Some(which) = open_editor {
            self.editor = Some(match which {
                None => EditorState::new_blank(),
                Some(i) => self
                    .jobs
                    .get(i)
                    .map(|(n, j)| EditorState::from_job(n, j))
                    .unwrap_or_else(EditorState::new_blank),
            });
        }

        let mut action: Option<EditorAction> = None;
        if let Some(ed) = &mut self.editor {
            let mut open = true;
            egui::Window::new(if ed.is_new { "New Job" } else { "Edit Job" })
                .open(&mut open)
                .resizable(true)
                .default_width(640.0)
                .show(ctx, |ui| {
                    egui::Grid::new("jobform").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
                        ui.label("name");
                        ui.add_enabled(ed.is_new, egui::TextEdit::singleline(&mut ed.name).desired_width(420.0));
                        ui.end_row();
                        ui.label("mode");
                        ui.horizontal(|ui| {
                            for m in ["mirror", "sync", "enrich"] {
                                ui.selectable_value(&mut ed.mode, m.to_string(), m);
                            }
                        });
                        ui.end_row();
                        ui.label("source");
                        ui.add(egui::TextEdit::singleline(&mut ed.source).desired_width(480.0));
                        ui.end_row();
                        ui.label("target");
                        ui.add(egui::TextEdit::singleline(&mut ed.target).desired_width(480.0));
                        ui.end_row();
                        ui.label("archive");
                        ui.add(egui::TextEdit::singleline(&mut ed.archive).desired_width(480.0).hint_text("sync 模式的存档路径；留空=无"));
                        ui.end_row();
                        ui.label("rigor");
                        ui.horizontal(|ui| {
                            for r in ["quick", "standard", "paranoid"] {
                                ui.selectable_value(&mut ed.rigor, r.to_string(), r);
                            }
                        });
                        ui.end_row();
                        ui.label("symlinks");
                        ui.horizontal(|ui| {
                            for s in ["exclude", "direct"] {
                                ui.selectable_value(&mut ed.symlinks, s.to_string(), s);
                            }
                            ui.checkbox(&mut ed.case_sensitive, "case sensitive");
                        });
                        ui.end_row();
                        ui.label("include");
                        ui.add(egui::TextEdit::multiline(&mut ed.include).desired_rows(2).desired_width(480.0).hint_text("FFS 过滤器语法，每行一条；留空 = 全部"));
                        ui.end_row();
                        ui.label("exclude");
                        ui.add(egui::TextEdit::multiline(&mut ed.exclude).desired_rows(2).desired_width(480.0).hint_text("默认垃圾/可重建排除已内置"));
                        ui.end_row();
                        ui.label("remote host");
                        ui.add(egui::TextEdit::singleline(&mut ed.remote_host).desired_width(480.0).hint_text("ssh 别名如 mac；留空 = 本地/挂载盘"));
                        ui.end_row();
                        ui.label("remote root");
                        ui.add(egui::TextEdit::singleline(&mut ed.remote_root).desired_width(480.0).hint_text("远端本地绝对路径"));
                        ui.end_row();
                        ui.label("remote exe");
                        ui.add(egui::TextEdit::singleline(&mut ed.remote_exe).desired_width(480.0).hint_text("默认当 syncdash 在远端 PATH 里"));
                        ui.end_row();
                    });
                    if let Some(err) = &ed.error {
                        ui.colored_label(egui::Color32::from_rgb(0xd9, 0x3a, 0x3a), err);
                    }
                    ui.separator();
                    ui.horizontal(|ui| {
                        if ui.button("Save").clicked() {
                            action = Some(EditorAction::Save);
                        }
                        if ui.button("Cancel").clicked() {
                            action = Some(EditorAction::Cancel);
                        }
                        if !ed.is_new {
                            if ed.delete_armed {
                                if ui.button("Confirm delete?").clicked() {
                                    action = Some(EditorAction::Delete);
                                }
                            } else if ui.button("Delete").clicked() {
                                ed.delete_armed = true;
                            }
                        }
                    });
                });
            if !open {
                action = Some(EditorAction::Cancel);
            }
        }
        match action {
            None => {}
            Some(EditorAction::Cancel) => self.editor = None,
            Some(EditorAction::Save) => {
                let outcome = self.editor.as_ref().map(|ed| (ed.name.trim().to_string(), ed.to_job()));
                if let Some((name, res)) = outcome {
                    match res {
                        Ok(job) => match config::save_job(&name, &job) {
                            Ok(path) => {
                                self.jobs = config::load_all();
                                self.sel = self.jobs.iter().position(|(n, _)| n == &name).unwrap_or(0);
                                self.shared.lock().unwrap().status = format!("saved {}", path.display());
                                self.editor = None;
                            }
                            Err(e) => {
                                if let Some(ed) = &mut self.editor {
                                    ed.error = Some(format!("save failed: {e}"));
                                }
                            }
                        },
                        Err(e) => {
                            if let Some(ed) = &mut self.editor {
                                ed.error = Some(e);
                            }
                        }
                    }
                }
            }
            Some(EditorAction::Delete) => {
                let name = self.editor.as_ref().map(|ed| ed.name.trim().to_string());
                if let Some(name) = name {
                    let msg = match config::delete_job(&name) {
                        Ok(_) => format!("deleted job '{name}'"),
                        Err(e) => format!("delete failed: {e}"),
                    };
                    self.jobs = config::load_all();
                    self.sel = 0;
                    self.shared.lock().unwrap().status = msg;
                    self.editor = None;
                }
            }
        }
    }
}
