//! GUI（参考 FFS 的核心交互）：任务下拉 → Compare → 差异表（勾选+方向徽章）→ Synchronize。
//! 后台线程跑扫描/执行，界面不卡；conflict/note 行默认不勾且不可勾。

use crate::compare::{Action, Op, Plan, Side};
use crate::config::{self, Job};
use crate::run;
use eframe::egui;
use std::sync::{Arc, Mutex};

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
}

pub struct App {
    jobs: Vec<(String, Job)>,
    sel: usize,
    shared: Arc<Mutex<Shared>>,
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
            shared: Arc::new(Mutex::new(Shared {
                phase: Phase::Idle,
                status: format!("jobs dir: {}", config::jobs_dir().display()),
                plan: None,
                checked: Vec::new(),
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
                let Shared { plan, checked, .. } = &mut *sh;
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
                                row.col(|ui| {
                                    let enabled = default_checked(op);
                                    ui.add_enabled(enabled, egui::Checkbox::without_text(&mut checked[i]));
                                });
                                row.col(|ui| {
                                    let (txt, color) = action_badge(op);
                                    ui.colored_label(color, txt);
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
                                    ui.label(&op.reason);
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
                sh.phase = Phase::Applying;
                sh.status = format!("synchronizing '{name}'...");
                plan.map(|p| (p, checked))
            };
            if let Some((plan, checked)) = taken {
                std::thread::spawn(move || {
                    let ops: Vec<Op> = plan
                        .ops
                        .iter()
                        .zip(&checked)
                        .filter(|(o, c)| **c && default_checked(o))
                        .map(|(o, _)| o.clone())
                        .collect();
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
    }
}
