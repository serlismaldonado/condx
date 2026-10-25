// Git y CI del proyecto elegido: un resumen del repo, los archivos cambiados
// por sección (con su letra de estado y sus líneas, piezas de ely-git), las
// ramas con su distancia al remoto, los últimos commits y, en heskala-erp,
// los submódulos frente a su remoto y las corridas de GitHub Actions.

use ely_core::data_display::Tone;
use ely_core::lists::GitStatus;
use ely_core::tables::{Cell, Column, DataTable, Row};
use ely_git::{Branch, BranchList, Commit, CommitList, DiffStat, GitStatusBadge};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::{h_flex, v_flex, ActiveTheme, IconName, Sizable};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use serde_json::Value;

use crate::app::{DevtoolApp, Load};
use crate::ui;

fn status_of(code: &str) -> GitStatus {
    match code {
        "A" => GitStatus::Added,
        "D" => GitStatus::Deleted,
        "R" | "C" => GitStatus::Renamed,
        "?" => GitStatus::Untracked,
        "U" => GitStatus::Conflicted,
        _ => GitStatus::Modified,
    }
}

impl DevtoolApp {
    pub fn load_git(&mut self, cx: &mut Context<Self>) {
        let project = self.project.clone();
        if !matches!(self.git.get(&project), Some(Load::Ready(_))) {
            self.git.insert(project.clone(), Load::Loading);
        }
        self.get(format!("/api/git?project={project}"), move |app, res| {
            app.git.insert(project, res);
        }, cx);
    }
}

/// Archivos cambiados de una sección: letra de estado, ruta y líneas.
fn change_section(title: &str, files: &[Value], cx: &App) -> Option<Div> {
    if files.is_empty() {
        return None;
    }
    let t = crate::theme::Tokens::current(cx);
    let mut col = v_flex().child(div().px_2().pt_2().pb_1().child(ui::section(format!("{title} · {}", files.len()), cx)));
    for (i, f) in files.iter().enumerate().take(60) {
        let path = f["path"].as_str().unwrap_or_default().to_string();
        let lines = f["lines"].as_array().map(|l| (l[0].as_u64().unwrap_or(0) as usize, l[1].as_u64().unwrap_or(0) as usize));
        col = col.child(
            h_flex()
                .gap_2()
                .px_2()
                .h(px(24.))
                .items_center()
                .child(GitStatusBadge::new(SharedString::from(format!("gs-{title}-{i}")), status_of(f["status"].as_str().unwrap_or("M"))))
                .child(div().flex_1().min_w_0().truncate().font_family(crate::theme::MONO_FONT).text_size(px(12.)).text_color(t.fg).child(path))
                .when_some(lines, |d, (a, r)| d.child(DiffStat::new(a, r))),
        );
    }
    if files.len() > 60 {
        col = col.child(div().px_2().pb_1().child(ui::muted(t!("and {a} more", a = files.len() - 60), cx)));
    }
    Some(col)
}

pub fn render(app: &mut DevtoolApp, _: &mut Window, cx: &mut Context<DevtoolApp>) -> impl IntoElement {
    let erp = app.project == "heskala-erp";
    let data = match app.git.get(&app.project) {
        None => {
            app.load_git(cx);
            return super::empty(t!("Reading the repository…").to_string(), cx).into_any_element();
        }
        Some(Load::Loading) => return super::empty(t!("Reading the repository…").to_string(), cx).into_any_element(),
        Some(Load::Failed(e)) => return super::empty(e.clone(), cx).into_any_element(),
        Some(Load::Ready(v)) => v.clone(),
    };
    let t = crate::theme::Tokens::current(cx);
    let changes = &data["changes"];
    let sections = [("En conflicto", "conflicted"), (t!("Staged"), "staged"), (t!("Unstaged"), "unstaged"), (t!("Untracked"), "untracked")];
    let total: usize = sections.iter().map(|(_, k)| changes[*k].as_array().map(Vec::len).unwrap_or(0)).sum();
    let mut changes_col = v_flex();
    for (title, key) in sections {
        if let Some(sec) = change_section(title, changes[key].as_array().map(Vec::as_slice).unwrap_or(&[]), cx) {
            changes_col = changes_col.child(sec);
        }
    }
    if total == 0 {
        changes_col = changes_col.child(div().p_3().child(ui::muted(t!("No changes: the working tree is clean."), cx)));
    }

    let branches: Vec<Branch> = data["branches"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|b| Branch {
            name: ui::txt(&b["name"]),
            remote: false,
            current: b["current"] == true,
            ahead: b["ahead"].as_u64().unwrap_or(0) as usize,
            behind: b["behind"].as_u64().unwrap_or(0) as usize,
            subject: ui::txt(&b["subject"]),
            when: ui::txt(&b["when"]),
        })
        .collect();
    let current = branches.iter().find(|b| b.current).map(|b| b.name.clone()).unwrap_or_default();
    let commits: Vec<Commit> = data["commits"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|c| Commit {
            id: ui::txt(&c["id"]),
            parents: c["parents"].as_array().into_iter().flatten().map(ui::txt).collect(),
            subject: ui::txt(&c["subject"]),
            author: ui::txt(&c["author"]),
            when: ui::txt(&c["when"]),
            refs: c["refs"].as_array().into_iter().flatten().map(ui::txt).collect(),
        })
        .collect();
    let head = commits.first().map(|c| c.subject.clone()).unwrap_or_default();
    // Los commits a todo su alto: sin scroll propio, la página los recorre.
    let commit_count = commits.len();

    let toolbar = h_flex()
        .gap_2()
        .items_center()
        .child(ely_core::primitives::Icon::new(ely_core::primitives::IconName::GitBranch).size(ely_core::theme::IconSize::Sm).color(t.subtle))
        .child(div().text_color(t.fg).child(current.clone()))
        .child(div().flex_1().min_w_0().truncate().child(ui::muted(head, cx)))
        .child(ui::muted(t!("{total} changes", total = total), cx))
        .child(Button::new("git-reload").xsmall().ghost().icon(IconName::RefreshCw).label(t!("Reload")).on_click(cx.listener(|this, _, _, cx| this.load_git(cx))));

    let mut branch_list = BranchList::new("branches", branches);
    if !current.is_empty() {
        branch_list = branch_list.selected(current);
    }
    // Como en los entornos: cada sección plegada (Cambios abierta al entrar), las listas con scroll
    // propio que pasa la rueda a la página en el borde, y todo en una columna que se adapta al ancho.
    let branch_count = app.git.get(&app.project).and_then(|l| match l { Load::Ready(v) => v["branches"].as_array().map(Vec::len), _ => None }).unwrap_or(0);
    let mut details = ui::Folds::new("git-details")
        .first_open()
        .item(
            "changes",
            t!("Changes · {total}", total = total),
            ui::chained(app.repo_scroll.0.clone()).child(div().id("changes-scroll").max_h(px(360.)).overflow_y_scroll().track_scroll(&app.repo_scroll.0).child(changes_col)),
        )
        .item(
            "branches",
            t!("Branches · {branch_count}", branch_count = branch_count),
            ui::chained(app.repo_scroll.1.clone()).child(div().id("branches-scroll").max_h(px(320.)).overflow_y_scroll().track_scroll(&app.repo_scroll.1).child(branch_list)),
        )
        .item("commits", t!("Latest commits · {commit_count}", commit_count = commit_count), div().h(px((commit_count * 36 + 8) as f32)).child(CommitList::new("commits", std::rc::Rc::new(commits))));
    let mut col = v_flex().gap_3().p_4().child(toolbar);
    // Submódulos y CI: sondas del ERP.
    if erp {
        let git = app.probes.get("git").map(|p| p["data"].clone()).unwrap_or(Value::Null);
        let short = |v: &Value| -> SharedString { v.as_str().map(|s| s[..s.len().min(8)].to_string().into()).unwrap_or("—".into()) };
        let subs: Vec<Row> = git["submodules"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|s| {
                let ok = s["matchesRemote"] == true;
                Row::new(
                    ui::txt(&s["path"]).to_string(),
                    [
                        Cell::Text(ui::txt(&s["path"])),
                        Cell::Text(short(&s["recorded"])),
                        Cell::Text(short(&s["remote"])),
                        Cell::Tag(if ok { t!("up to date") } else { t!("different") }.into(), if ok { Tone::Success } else { Tone::Warning }),
                    ],
                )
            })
            .collect();
        if !subs.is_empty() {
            let distintos = git["submodules"].as_array().map(|l| l.iter().filter(|s| s["matchesRemote"] != true).count()).unwrap_or(0);
            let title = if distintos == 0 { t!("Submodules · {a} up to date", a = subs.len()) } else { t!("Submodules · {distintos} differ from remote", distintos = distintos) };
            details = details.item(
                "submodules",
                title,
                DataTable::new("submodules", [Column::new("path", t!("Submodule")), Column::new("rec", t!("Pointer")), Column::new("remote", t!("Remote")), Column::new("state", t!("Status"))]).rows(subs),
            );
        }
        let ci = app.probes.get("ci").cloned().unwrap_or(Value::Null);
        for r in ci["data"]["repos"].as_array().cloned().unwrap_or_default() {
            let repo = ui::txt(&r["repo"]).to_string();
            let runs = r["runs"].as_array().cloned().unwrap_or_default();
            // El título ya dice cómo está la última corrida.
            let last = runs.first().map(|run| match (run["status"].as_str(), run["conclusion"].as_str()) {
                (Some("completed"), Some("success")) => t!("green"),
                (Some("completed"), Some("failure")) => t!("failed"),
                (Some("completed"), _) => t!("finished"),
                _ => t!("running"),
            });
            let title = match last {
                Some(l) => format!("CI · {repo} · {l}"),
                None => format!("CI · {repo}"),
            };
            let mut card = v_flex().gap_2();
            if runs.is_empty() {
                card = card.child(ui::muted(t!("No recent valid runs."), cx));
            } else {
                let urls: Vec<(String, String)> = runs.iter().take(12).enumerate().map(|(i, run)| (format!("{repo}-{i}"), run["url"].as_str().unwrap_or_default().to_string())).collect();
                let rows: Vec<Row> = runs
                    .iter()
                    .take(12)
                    .enumerate()
                    .map(|(i, run)| {
                        let (label, tone) = match (run["status"].as_str(), run["conclusion"].as_str()) {
                            (Some("completed"), Some("success")) => (t!("passed"), Tone::Success),
                            (Some("completed"), Some("failure")) => (t!("failed"), Tone::Danger),
                            (Some("completed"), Some(c)) => (c, Tone::Warning),
                            _ => (t!("running"), Tone::Info),
                        };
                        Row::new(
                            format!("{repo}-{i}"),
                            [
                                Cell::Tag(label.to_string().into(), tone),
                                Cell::Text(ui::txt(&run["workflowName"])),
                                Cell::Text(ui::txt(&run["headBranch"])),
                                Cell::Text(ui::txt(&run["displayTitle"])),
                                Cell::Text(ui::txt(&run["updatedAt"])),
                            ],
                        )
                    })
                    .collect();
                card = card.child(ui::muted(t!("Click a run to open it on GitHub."), cx)).child(
                    DataTable::new(SharedString::from(format!("ci-{repo}")), [
                        Column::new("state", t!("Status")).width(rems(7.)),
                        Column::new("wf", t!("Workflow")).width(rems(10.)),
                        Column::new("branch", t!("Branch")).width(rems(6.)),
                        Column::new("title", t!("Title")),
                        Column::new("when", t!("When")).width(rems(11.)),
                    ])
                    .rows(rows)
                    .selected(None::<SharedString>)
                    .on_select(move |keys, _, cx| {
                        if let Some(url) = keys.last().and_then(|k| urls.iter().find(|(id, _)| id == k.as_ref())).map(|(_, u)| u.clone()) {
                            if !url.is_empty() {
                                cx.open_url(&url);
                            }
                        }
                    }),
                );
            }
            details = details.item("ci", title, card);
        }
        if let Some(err) = ci["error"].as_str() {
            col = col.child(ui::muted(err.to_string(), cx).text_color(cx.theme().danger));
        }
    }
    col.child(details.render(app, cx)).into_any_element()
}
