//! Pane's quicklinks, a default extension in Raycast's shape: named targets
//! (a link of any scheme, a file, a folder or an application, ADR 0037),
//! each with an optional application to open it with, which root search
//! finds and opens. Four commands share this component:
//!
//! - **Search Quicklinks** (`quicklinks`, view): each quicklink an item with
//!   its favicon (or its file's or application's icon), its name and its
//!   target, and the actions Open, Open With…, Copy Link, Edit, Duplicate and
//!   Delete. It also supplies the quicklinks to root search as indexed
//!   results that Pane opens itself.
//! - **Create Quicklink** (`create`, designed): the form. Edit and Duplicate
//!   launch it with the quicklink in their context, so it opens filled in.
//! - **Export Quicklinks** (`export`, no-view): copies them as JSON.
//! - **Import Quicklinks** (`import`, no-view): adds those the clipboard's
//!   JSON holds that are new.
//!
//! Quicklinks are kept in the package's content, its durable extension
//! data, so they survive restarts, updates, disabling and clearing its
//! cache (see [`links`]).
#![no_std]

mod links;
mod transfer;

use pane_extension::alloc::{format, string::String, vec::Vec};
use pane_extension::commands::{self, CommandRef, LaunchType};
use pane_extension::feedback::{self, Confirmation, HudStyle, Toast};
use pane_extension::form::{self, FormValues};
use pane_extension::icon::{self, Accessory, Icon, Tone};
use pane_extension::indexed::{IndexedAction, IndexedResult, OpenTarget};
use pane_extension::system::{self, Clip};
use pane_extension::view::{Cx, IntoAnswer, View, text};
use pane_extension::window;
use pane_extension::{
    Action, Command, Item, LaunchRecord, List, Modifier, Shortcut, actions, applications,
};

use links::Quicklink;

struct Quicklinks;
pane_extension::export!(Quicklinks);
pane_extension::indexed::export!(Quicklinks);

/// The commands' ids in `pane.json`.
const SEARCH: &str = "quicklinks";
const CREATE: &str = "create";
const IMPORT: &str = "import";
const EXPORT: &str = "export";

/// The form's fields.
const NAME: &str = "name";
const LINK: &str = "link";
const OPEN_WITH: &str = "application";

/// Why a submission was refused: the field whose value was refused (empty
/// for the whole form) and the message saying why.
type Refusal = (String, String);

/// The Create Quicklink form, a designed view since forms moved onto the
/// designed tree (pane-app/pane#241). Its state is the quicklink it edits
/// (a new one when none), the values its fields start from, and why its
/// last submission was refused. A submission checks the values, saves the
/// quicklink, tells the user with a toast and returns to where the form
/// came from.
struct QuicklinkForm {
    /// The quicklink being edited, by its id; `None` for a new one.
    editing: Option<String>,
    /// The view's title, naming the quicklink when it edits one.
    title: String,
    /// The values the fields start from.
    name: String,
    link: String,
    open_with: String,
    /// Why the last submission was refused, shown on its field.
    refused: Option<Refusal>,
}

impl QuicklinkForm {
    /// The form `launch` asks for: a new quicklink's, one filled in to edit
    /// the quicklink its context names, or a new one filled in as a copy of
    /// it.
    fn of(launch: &LaunchRecord) -> Result<QuicklinkForm, String> {
        let Some((purpose, id)) = transfer::purpose(launch.context.as_deref()) else {
            return Ok(QuicklinkForm {
                editing: None,
                title: "Create Quicklink".into(),
                name: String::new(),
                link: String::new(),
                open_with: String::new(),
                refused: None,
            });
        };
        let saved = links::load()?;
        let link = links::with_id(&saved, &id)
            .map(|index| &saved[index])
            .ok_or("That quicklink no longer exists")?;
        // An installed application by its name, one named by its path by the
        // path: what the field takes back.
        let application = link.application.as_ref().map_or("", |application| {
            if links::is_path(&application.id) {
                application.id.as_str()
            } else {
                application.name.as_str()
            }
        });
        let (editing, title, name) = if purpose == "edit" {
            (
                Some(link.id.clone()),
                format!("Edit “{}”", link.name),
                link.name.clone(),
            )
        } else {
            (
                None,
                "Duplicate Quicklink".into(),
                links::copy_name(&saved, &link.name),
            )
        };
        Ok(QuicklinkForm {
            editing,
            title,
            name,
            link: link.target.clone(),
            open_with: application.into(),
            refused: None,
        })
    }

    /// Saves the quicklink the form was submitted with, the one edited or a
    /// new one, and answers what to tell the user; or why it was refused.
    fn save(&self, values: &FormValues) -> Result<String, Refusal> {
        let value = |field: &str| values.text(field).unwrap_or_default().trim();
        let (name, target) = (value(NAME), value(LINK));
        let mut saved = links::load().map_err(form_error)?;
        let editing = match &self.editing {
            Some(id) => Some(
                links::with_id(&saved, id)
                    .ok_or_else(|| form_error("This quicklink no longer exists".into()))?,
            ),
            None => None,
        };
        check(&saved, editing, name, target)?;
        let application = links::application(value(OPEN_WITH), &installed())
            .map_err(|problem| field_error(OPEN_WITH, problem))?;
        let message = match editing {
            Some(index) => {
                let link = &mut saved[index];
                link.name = name.into();
                link.target = target.into();
                link.application = application;
                format!("Saved “{name}”")
            }
            None => {
                let id = links::next_id(&saved);
                saved.push(Quicklink {
                    id,
                    name: name.into(),
                    target: target.into(),
                    application,
                });
                format!("Created “{name}”")
            }
        };
        links::save(&saved).map_err(form_error)?;
        Ok(message)
    }

    /// Runs a submission: saves the quicklink, tells the user and returns
    /// to where the form came from, or keeps the form open saying why it
    /// was refused.
    fn submit(&mut self, values: &FormValues) {
        let message = match self.save(values) {
            Ok(message) => message,
            Err(refusal) => {
                self.refused = Some(refusal);
                return;
            }
        };
        self.refused = None;
        feedback::show_toast(Toast::success(message));
        if self.editing.is_some() {
            // Back to the quicklinks, as the edit came from there.
            let search = CommandRef {
                source: None,
                command: SEARCH.into(),
            };
            if let Err(problem) = commands::launch(&search, LaunchType::UserInitiated, &[], None) {
                self.refused = Some(form_error(problem));
            }
        } else {
            // Back to root search, where the new quicklink is found.
            window::pop_to_root(false);
        }
    }

    /// The message the last submission was refused with for `field` (empty
    /// for the whole form), or none.
    fn refusal(&self, field: &str) -> Option<&str> {
        match &self.refused {
            Some((refused, message)) if refused == field => Some(message),
            _ => None,
        }
    }
}

impl View for QuicklinkForm {
    fn render(&mut self, cx: &mut Cx<Self>) -> impl IntoAnswer {
        let submit = cx.form_listener(QuicklinkForm::submit);
        let error = |field: &str| String::from(self.refusal(field).unwrap_or_default());
        let mut form = form::Form::new()
            .key("form")
            .navigation_title(self.title.clone())
            .submit_title(if self.editing.is_some() {
                "Save Quicklink"
            } else {
                "Create Quicklink"
            })
            .on_submit(submit);
        // A refusal of the whole form, which no field's error shows, draws
        // over the fields. The form stays the tree's root, so the window
        // keeps what the user typed in them.
        if let Some(message) = self.refusal("") {
            form = form.child(text(message).color(Tone::Danger));
        }
        form.child(
            form::text_field(NAME)
                .title("Name")
                .placeholder("Pane issues")
                .default_value(self.name.clone())
                .error(error(NAME))
                .auto_focus(),
        )
        .child(
            form::text_field(LINK)
                .title("Link")
                .placeholder("https://…, mailto:…, or the path of a file, folder or application")
                .default_value(self.link.clone())
                .error(error(LINK)),
        )
        .child(
            form::text_field(OPEN_WITH)
                .title("Open With")
                .placeholder("Default application")
                .default_value(self.open_with.clone())
                .error(error(OPEN_WITH)),
        )
        .into_answer()
    }
}

/// Launches Create Quicklink with `context`, as the user would.
fn open_form(context: Option<&str>) -> Result<(), String> {
    let create = CommandRef {
        source: None,
        command: CREATE.into(),
    };
    commands::launch(&create, LaunchType::UserInitiated, &[], context)
}

/// The icon of `link`'s row: its site's favicon, the system's icon of its
/// file, folder or application, or a link.
fn icon_of(link: &Quicklink) -> Icon {
    if links::is_web(&link.target) {
        icon::favicon(&link.target)
    } else if links::is_path(&link.target) {
        icon::file_icon(&link.target)
    } else {
        Icon::builtin("link").tint(Tone::Secondary)
    }
}

/// `key` with Ctrl held, and Shift with `shift`.
fn ctrl(shift: bool, key: &str) -> Shortcut {
    if shift {
        Shortcut::new([Modifier::Ctrl, Modifier::Shift], key)
    } else {
        Shortcut::new([Modifier::Ctrl], key)
    }
}

/// `link` as an item of Search Quicklinks.
fn item(link: &Quicklink) -> Item {
    let mut open = actions::open(link.target.clone());
    if let Some(application) = &link.application {
        open = open.application(application.id.clone());
    }
    let copy = Action::from(actions::copy(Clip::Text(link.target.clone())).title("Copy Link"))
        .shortcut(ctrl(true, "c"));
    let edit = {
        let id = link.id.clone();
        Action::new("Edit", move || async move {
            open_form(Some(&transfer::context("edit", &id)))
        })
        .shortcut(ctrl(false, "e"))
    };
    let duplicate = {
        let id = link.id.clone();
        Action::new("Duplicate", move || async move {
            open_form(Some(&transfer::context("duplicate", &id)))
        })
    };
    let delete = {
        let (id, name) = (link.id.clone(), link.name.clone());
        Action::new("Delete", move || async move { delete(&id, &name).await }).destructive()
    };
    let mut item = Item::new(link.id.clone(), link.name.clone())
        .subtitle(link.target.clone())
        .icon(icon_of(link))
        .action(open.into())
        .action(actions::open_with(link.target.clone()).into())
        .action(copy)
        .action(edit)
        .action(duplicate)
        .action(delete);
    if let Some(application) = &link.application {
        item = item.accessory(
            Accessory::text(application.name.clone())
                .tooltip(format!("Opens with {}", application.name)),
        );
    }
    item
}

/// Deletes the quicklink `id`, named `name`, once the user confirms it.
async fn delete(id: &str, name: &str) -> Result<(), String> {
    let asked = Confirmation::new(format!("Delete “{name}”?"))
        .message("The quicklink is deleted for good.")
        .primary("Delete")
        .destructive();
    if !feedback::confirm(asked).await? {
        return Ok(());
    }
    let mut saved = links::load()?;
    if let Some(index) = links::with_id(&saved, id) {
        saved.remove(index);
        links::save(&saved)?;
    }
    feedback::show_toast(Toast::success(format!("Deleted “{name}”")));
    Ok(())
}

/// A refusal of `field`'s value, saying why.
fn field_error(field: &str, message: String) -> Refusal {
    (field.into(), message)
}

/// A refusal of the whole form, saying why.
fn form_error(message: String) -> Refusal {
    (String::new(), message)
}

/// Checks `name` and `target` for the quicklink at `index` (a new one when
/// `None`) among `saved`.
fn check(
    saved: &[Quicklink],
    index: Option<usize>,
    name: &str,
    target: &str,
) -> Result<(), Refusal> {
    if let Some(problem) = links::name_problem(name) {
        return Err(field_error(NAME, problem));
    }
    if let Some(other) = links::find(saved, name).filter(|&other| Some(other) != index) {
        let existing = &saved[other].name;
        return Err(field_error(
            NAME,
            format!("A quicklink named “{existing}” already exists"),
        ));
    }
    if let Some(problem) = links::target_problem(target) {
        return Err(field_error(LINK, problem));
    }
    Ok(())
}

/// The installed applications, for naming one to open with; none when Pane
/// cannot list them.
fn installed() -> Vec<applications::Application> {
    applications::installed().unwrap_or_default()
}

/// The plural of "quicklink" for `count`.
fn quicklinks(count: usize) -> String {
    match count {
        1 => "1 quicklink".into(),
        count => format!("{count} quicklinks"),
    }
}

/// Export Quicklinks: copies them as JSON and says how many in a HUD.
fn export() -> Result<(), String> {
    let saved = links::load()?;
    system::copy(&Clip::Text(transfer::export(&saved)), false)?;
    feedback::show_hud(
        &format!("Copied {} as JSON", quicklinks(saved.len())),
        HudStyle::Success,
    );
    Ok(())
}

/// Import Quicklinks: adds those the clipboard's JSON holds whose name no
/// quicklink has yet, and says how many it added and skipped in a toast; a
/// failure toast says why when the clipboard holds no such JSON.
fn import() -> Result<(), String> {
    let text = match system::read_clipboard()? {
        Some(Clip::Text(text)) => text,
        _ => {
            feedback::show_toast(
                Toast::failure("Could not import quicklinks")
                    .message("The clipboard holds no text: copy the quicklinks' JSON first"),
            );
            return Ok(());
        }
    };
    let given = match transfer::read(&text) {
        Ok(given) => given,
        Err(why) => {
            feedback::show_toast(Toast::failure("Could not import quicklinks").message(why));
            return Ok(());
        }
    };
    let mut saved = links::load()?;
    let installed = installed();
    let (mut added, mut skipped) = (0, 0);
    for given in given {
        match given.and_then(|given| new_quicklink(&saved, given, &installed)) {
            Some(link) => {
                saved.push(link);
                added += 1;
            }
            None => skipped += 1,
        }
    }
    if added > 0 {
        links::save(&saved)?;
    }
    feedback::show_toast(Toast::success(format!(
        "Added {}, skipped {skipped}",
        quicklinks(added)
    )));
    Ok(())
}

/// The quicklink `given` adds to `saved`: `None` when its name or link is
/// not one a quicklink can have, or a quicklink already has its name. An
/// application this system does not have is kept as given: opening says
/// why it cannot be used.
fn new_quicklink(
    saved: &[Quicklink],
    given: transfer::Given,
    installed: &[applications::Application],
) -> Option<Quicklink> {
    if links::name_problem(&given.name).is_some()
        || links::find(saved, &given.name).is_some()
        || links::target_problem(&given.link).is_some()
    {
        return None;
    }
    let application = match given.open_with {
        None => None,
        Some(open_with) => match links::application(&open_with, installed) {
            Ok(found) => found,
            Err(_) if open_with.chars().any(char::is_control) => return None,
            Err(_) => Some(links::Application {
                id: open_with.clone(),
                name: open_with,
            }),
        },
    };
    Some(Quicklink {
        id: links::next_id(saved),
        name: given.name,
        target: given.link,
        application,
    })
}

impl Command for Quicklinks {
    type DesignedView = QuicklinkForm;

    async fn render() -> Result<List, String> {
        let saved = links::load()?;
        if saved.is_empty() {
            let create = Item::new("create", "Create Quicklink")
                .subtitle("No quicklinks yet")
                .icon(Icon::builtin("add").tint(Tone::Secondary))
                .action(Action::new("Create Quicklink", || async {
                    open_form(None)
                }));
            return Ok(List::new("Quicklinks").item(create));
        }
        Ok(List::new("Quicklinks").items(saved.iter().map(item)))
    }

    async fn run(command: String, _launch: LaunchRecord) -> Result<(), String> {
        match command.as_str() {
            EXPORT => export(),
            IMPORT => import(),
            other => Err(format!(
                "`{other}` opens a screen; it has no run entry point"
            )),
        }
    }

    /// Create Quicklink's form, a designed view: opened empty, or filled
    /// in by Edit and Duplicate.
    async fn open_designed_view(
        command: String,
        launch: LaunchRecord,
    ) -> Result<QuicklinkForm, String> {
        match command.as_str() {
            CREATE => QuicklinkForm::of(&launch),
            other => Err(format!("`{other}` opens no designed view")),
        }
    }
}

impl pane_extension::indexed::Guest for Quicklinks {
    /// One result per quicklink: its name, found in root search like a
    /// command's title, its target as the subtitle, and Pane opening the
    /// target (with its application, if it has one) when invoked.
    async fn results() -> Result<Vec<IndexedResult>, String> {
        Ok(links::load()?
            .into_iter()
            .map(|link| IndexedResult {
                id: link.id,
                title: link.name,
                subtitle: Some(link.target.clone()),
                alternate_titles: Vec::new(),
                keywords: Vec::new(),
                action: IndexedAction::Open(OpenTarget {
                    target: link.target,
                    application: link.application.map(|application| application.id),
                }),
            })
            .collect())
    }
}
