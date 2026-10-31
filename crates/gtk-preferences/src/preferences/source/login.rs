use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use rufin_core::runtime::source::{EditableSource, SourceHandle, SourceOperation};

use sources::{
    CredentialInput, CredentialPreset, SubsonicAuthentication, SubsonicFlavor,
};

use crate::Preferences;
use gtk_widgets::controls::text_button;
use gtk_widgets::layout::large_popup_content_width;
use gtk_widgets::popup::present_light_dismiss_dialog;
use rufin_core::runtime::source::source_operation_text;

const ADD_SERVER_CLAMP_WIDTH: i32 = 560;
const SETUP_FORM_PAGE: &str = "form";
const SETUP_PROGRESS_PAGE: &str = "progress";

type SetupFlowFactory =
    fn(&Rc<Preferences>, &'static SourcePresentation) -> Rc<dyn SourceSetupFlow>;
type SettingsGroupFactory = fn(
    &Rc<Preferences>,
    &EditableSource,
    &'static SourcePresentation,
) -> Result<gtk::Widget, String>;

#[derive(Clone, Copy)]
struct SourcePresentation {
    kind: &'static str,
    setup_flow: SetupFlowFactory,
    settings_group: Option<SettingsGroupFactory>,
}

static NAVIDROME: SourcePresentation = SourcePresentation {
    kind: "navidrome",
    setup_flow: navidrome_setup_flow,
    settings_group: Some(navidrome_settings_group),
};
static SUBSONIC: SourcePresentation = SourcePresentation {
    kind: "subsonic",
    setup_flow: subsonic_setup_flow,
    settings_group: Some(subsonic_settings_group),
};
static SOURCE_PRESENTATIONS: [&SourcePresentation; 2] = [&NAVIDROME, &SUBSONIC];

fn source_presentations() -> &'static [&'static SourcePresentation] {
    &SOURCE_PRESENTATIONS
}

fn source_presentation(kind: &str) -> Option<&'static SourcePresentation> {
    source_presentations()
        .iter()
        .copied()
        .find(|presentation| presentation.kind == kind)
}

pub fn source_settings_group(
    shell: &Rc<Preferences>,
    saved: &EditableSource,
) -> Option<Result<gtk::Widget, String>> {
    let presentation = source_presentation(&saved.source.kind)?;
    presentation
        .settings_group
        .map(|factory| factory(shell, saved, presentation))
}

pub trait SourceSetupFlow {
    fn view(&self, shell: &Rc<Preferences>, context: &SetupViewContext) -> gtk::Widget;
}

pub struct SourceSetupViewHandle {
    onboarding: bool,
    context: Option<SetupViewContext>,
    flow: Rc<RefCell<Rc<dyn SourceSetupFlow>>>,
}

impl SourceSetupViewHandle {
    fn new(context: SetupViewContext) -> Self {
        Self {
            onboarding: context.onboarding,
            flow: Rc::clone(&context.flow),
            context: Some(context),
        }
    }
}

#[derive(Clone)]
pub struct SetupViewContext {
    onboarding: bool,
    content: gtk::Box,
    surface: gtk::Stack,
    form: gtk::Box,
    progress_status: gtk::Label,
    flow: Rc<RefCell<Rc<dyn SourceSetupFlow>>>,
    actions: Rc<RefCell<Option<SetupActions>>>,
    retry_focus: Rc<RefCell<Option<gtk::Widget>>>,
    preferences_dialog: Option<adw::Dialog>,
}

struct SetupActions {
    status: gtk::Label,
    connect: gtk::Button,
    ready: Rc<dyn Fn() -> bool>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SetupSurfaceState<'a> {
    Form,
    Progress,
    Failed(&'a str),
}

impl SetupViewContext {
    fn is_mounted(&self) -> bool {
        self.content.root().is_some()
    }
}

#[derive(Clone, Debug)]
struct CredentialHostDraft {
    name: String,
    url: String,
    username: String,
    password: String,
    cert_verify: bool,
}

struct CredentialSetupFlow {
    presentation: &'static SourcePresentation,
    draft: Rc<RefCell<CredentialHostDraft>>,
    authentication: Rc<Cell<SubsonicAuthentication>>,
    offer_authentication: bool,
    submit: Rc<dyn Fn(&SourceHandle, CredentialInput, SubsonicAuthentication)>,
}

struct SourceChoiceFlow;

impl SourceSetupFlow for SourceChoiceFlow {
    fn view(&self, shell: &Rc<Preferences>, context: &SetupViewContext) -> gtk::Widget {
        source_choice_selector(shell, context)
    }
}

#[derive(Clone)]
struct CredentialHost {
    widget: gtk::Box,
    fields_group: adw::PreferencesGroup,
    name: adw::EntryRow,
    url: adw::EntryRow,
    username: adw::EntryRow,
    password: adw::PasswordEntryRow,
    cert_verify: adw::SwitchRow,
    authentication: Option<Rc<Cell<SubsonicAuthentication>>>,
    authentication_toggles: Option<[adw::SwitchRow; 2]>,
    save: gtk::Button,
}

impl CredentialHost {
    fn input(&self) -> CredentialInput {
        CredentialInput {
            source_name: trimmed_optional_text(&self.name),
            server_url: self.url.text().to_string(),
            username: self.username.text().to_string(),
            secret: self.password.text().to_string(),
            trust_invalid_cert: !self.cert_verify.is_active(),
        }
    }

    fn ready(&self) -> bool {
        remote_login_ready(
            &self.url,
            &self.username,
            &self.password,
            self.authentication.as_ref().is_some_and(|authentication| {
                authentication.get() == SubsonicAuthentication::ApiKey
            }),
        )
    }
}

impl Preferences {
    pub fn present_onboarding(self: &Rc<Self>) {
        let context = self
            .source
            .add_server
            .borrow()
            .as_ref()
            .and_then(|handle| handle.context.clone())
            .filter(|context| context.onboarding);
        if let Some(context) = context {
            if let Some(dialog) = context.preferences_dialog {
                dialog.present(Some(&self.window));
                return;
            }
        }
        let resource = crate::ui_resource::ONBOARDING_RESOURCE;
        let builder = gtk_widgets::ui_resource::builder(resource);
        gtk_widgets::objects!(builder, resource, {
            dialog: adw::Dialog, toolbar: adw::ToolbarView,
        });
        let flow = self
            .source
            .add_server
            .borrow()
            .as_ref()
            .filter(|handle| handle.onboarding)
            .map(|handle| Rc::clone(&handle.flow));
        let context = self.setup_view_context(Some(dialog.clone()), flow, true);
        mount_setup_flow(self, &context);
        toolbar.set_content(Some(&context.content));
        *self.source.add_server.borrow_mut() = Some(SourceSetupViewHandle::new(context));
        let weak = Rc::downgrade(self);
        dialog.connect_closed(move |_| {
            if let Some(shell) = weak.upgrade() {
                shell.release_inactive_add_server_form();
            }
        });
        dialog.present(Some(&self.window));
    }

    pub fn add_server_navigation_page(
        self: &Rc<Self>,
        preferences_dialog: &adw::Dialog,
    ) -> adw::NavigationPage {
        // Keep only the flow draft across page replacement. A failed connection
        // rebuilds the form from those values so the user can correct and retry.
        let flow = self
            .source
            .add_server
            .borrow()
            .as_ref()
            .filter(|handle| !handle.onboarding)
            .map(|handle| Rc::clone(&handle.flow));
        let context = self.setup_view_context(Some(preferences_dialog.clone()), flow, false);
        mount_setup_flow(self, &context);
        *self.source.add_server.borrow_mut() = Some(SourceSetupViewHandle::new(context.clone()));
        adw::NavigationPage::new(&context.content, &"Add Source".to_string())
    }

    pub fn update_add_server_dialog(self: &Rc<Self>) {
        let Some(context) = self.mounted_add_server_context() else {
            return;
        };
        update_setup_surface(self, &context);
    }

    pub fn complete_add_server_dialog(&self) {
        let preferences_dialog = {
            let mut handle = self.source.add_server.borrow_mut();
            handle
                .take()
                .and_then(|handle| handle.context)
                .and_then(|context| context.preferences_dialog)
        };
        if let Some(dialog) = preferences_dialog {
            dialog.close();
        }
    }

    pub fn clear_retained_add_server_form(&self) {
        self.source.add_server.borrow_mut().take();
    }

    fn mounted_add_server_context(&self) -> Option<SetupViewContext> {
        let context = self.source.add_server.borrow().as_ref()?.context.clone()?;
        context.is_mounted().then_some(context)
    }

    pub fn release_inactive_add_server_form(&self) {
        let mut handle = self.source.add_server.borrow_mut();
        if !matches!(
            *self.source.operation.borrow(),
            SourceOperation::Adding { .. }
        ) {
            handle.take();
            return;
        }
        let Some(handle) = handle.as_mut() else {
            return;
        };
        handle.context.take();
    }

    pub fn has_retained_add_server_form(&self) -> bool {
        self.source.add_server.borrow().is_some()
    }

    pub fn restore_add_server_dialog_after_failure(self: &Rc<Self>) -> bool {
        let context = self
            .source
            .add_server
            .borrow()
            .as_ref()
            .and_then(|handle| handle.context.clone());
        let Some(context) = context else {
            if self.has_retained_add_server_form() {
                if self
                    .source
                    .add_server
                    .borrow()
                    .as_ref()
                    .is_some_and(|handle| handle.onboarding)
                {
                    self.present_onboarding();
                } else {
                    crate::preferences::present_add_server_preferences_dialog(self);
                }
                return true;
            }
            return false;
        };
        let Some(dialog) = context.preferences_dialog.clone() else {
            return false;
        };
        if context.onboarding {
            dialog.present(Some(&self.window));
        } else {
            self.state.set_active_dialog(&dialog);
            present_light_dismiss_dialog(&dialog, &self.window);
        }
        update_setup_surface(self, &context);
        true
    }

    fn setup_view_context(
        self: &Rc<Self>,
        preferences_dialog: Option<adw::Dialog>,
        flow: Option<Rc<RefCell<Rc<dyn SourceSetupFlow>>>>,
        onboarding: bool,
    ) -> SetupViewContext {
        let resource = crate::ui_resource::CONNECTION_PROGRESS_RESOURCE;
        let builder = gtk_widgets::ui_resource::builder(resource);
        gtk_widgets::objects!(builder, resource, {
            content: gtk::Box,
            surface: gtk::Stack,
            form: gtk::Box,
            status: gtk::Label,
        });
        status.set_label(&self.source_connection_progress_text());
        let flow = flow.unwrap_or_else(|| Rc::new(RefCell::new(self.default_source_setup_flow())));
        SetupViewContext {
            onboarding,
            content,
            surface,
            form,
            progress_status: status,
            flow,
            actions: Rc::new(RefCell::new(None)),
            retry_focus: Rc::new(RefCell::new(None)),
            preferences_dialog,
        }
    }

    fn default_source_setup_flow(self: &Rc<Self>) -> Rc<dyn SourceSetupFlow> {
        Rc::new(SourceChoiceFlow)
    }

    fn source_connection_progress_text(&self) -> String {
        source_operation_text(&self.source.operation.borrow())
            .unwrap_or_else(|| "Preparing library...".to_string())
    }


}

fn subsonic_setup_flow_for(
    _shell: &Rc<Preferences>,
    presentation: &'static SourcePresentation,
    kind: SubsonicFlavor,
) -> Rc<dyn SourceSetupFlow> {
    let preset = None;
    let authentication = SubsonicAuthentication::Password;
    let offer_authentication = kind == SubsonicFlavor::Subsonic;
    Rc::new(CredentialSetupFlow {
        presentation,
        draft: Rc::new(RefCell::new(credential_draft(preset))),
        authentication: Rc::new(Cell::new(authentication)),
        offer_authentication,
        submit: Rc::new(move |source, input, authentication| {
            let source = source.clone();
            gtk::glib::spawn_future_local(async move {
                let _ = source
                    .save_subsonic(None, kind, authentication, input)
                    .await;
            });
        }),
    })
}

fn navidrome_setup_flow(
    shell: &Rc<Preferences>,
    presentation: &'static SourcePresentation,
) -> Rc<dyn SourceSetupFlow> {
    subsonic_setup_flow_for(shell, presentation, SubsonicFlavor::Navidrome)
}

fn subsonic_setup_flow(
    shell: &Rc<Preferences>,
    presentation: &'static SourcePresentation,
) -> Rc<dyn SourceSetupFlow> {
    subsonic_setup_flow_for(shell, presentation, SubsonicFlavor::Subsonic)
}

fn subsonic_settings_group_for(
    shell: &Rc<Preferences>,
    saved: &EditableSource,
    presentation: &'static SourcePresentation,
    kind: SubsonicFlavor,
) -> Result<gtk::Widget, String> {
    let source_id = saved.source.id.clone();
    let authentication = (kind == SubsonicFlavor::Subsonic).then_some(
        saved
            .credentials
            .open_subsonic_authentication
            .unwrap_or(SubsonicAuthentication::Password),
    );
    let snapshot = credential_draft(Some(saved.credentials.clone()));
    let authentication = authentication.map(|value| Rc::new(Cell::new(value)));
    let host = credential_host_view(&snapshot, true, authentication);
    host.fields_group.set_title(&"Server Settings".to_string());
    host.fields_group
        .set_description(Some(&String::from(gtk_widgets::source_labels::source_kind_title(
            presentation.kind,
        )
        .expect("registered source presentation"))));
    host.cert_verify
        .set_subtitle(&"Off only for a server you control".to_string());

    let save = &host.save;
    save.set_visible(true);

    let source = shell.products.source.clone();
    let name = host.name.clone();
    let url = host.url.clone();
    let username = host.username.clone();
    let password = host.password.clone();
    let cert_verify = host.cert_verify.clone();
    let authentication = host.authentication.clone();
    let weak_shell = Rc::downgrade(shell);
    save.connect_clicked(move |button| {
        let authentication = authentication
            .as_ref()
            .map(|authentication| authentication.get());
        let source = source.clone();
        let source_id = source_id.clone();
        let credentials = CredentialInput {
            source_name: Some(name.text().trim().to_string()),
            server_url: url.text().trim().to_string(),
            username: username.text().trim().to_string(),
            secret: password.text().to_string(),
            trust_invalid_cert: !cert_verify.is_active(),
        };
        if let Some(shell) = weak_shell.upgrade() {
            complete_source_save(&shell, button, async move {
                source
                    .save_subsonic(
                        Some(source_id),
                        kind,
                        authentication.unwrap_or(SubsonicAuthentication::Password),
                        credentials,
                    )
                    .await
                    .map_err(|error| error.to_string())
            });
        }
    });

    Ok(host.widget.upcast())
}

fn navidrome_settings_group(
    shell: &Rc<Preferences>,
    saved: &EditableSource,
    presentation: &'static SourcePresentation,
) -> Result<gtk::Widget, String> {
    subsonic_settings_group_for(shell, saved, presentation, SubsonicFlavor::Navidrome)
}

fn subsonic_settings_group(
    shell: &Rc<Preferences>,
    saved: &EditableSource,
    presentation: &'static SourcePresentation,
) -> Result<gtk::Widget, String> {
    subsonic_settings_group_for(shell, saved, presentation, SubsonicFlavor::Subsonic)
}

fn complete_source_save(
    shell: &Rc<Preferences>,
    button: &gtk::Button,
    result: impl std::future::Future<Output = Result<(), String>> + 'static,
) {
    button.set_sensitive(false);
    let button = button.downgrade();
    let shell = Rc::downgrade(shell);
    gtk::glib::spawn_future_local(async move {
        let result = result.await;
        match result {
            Ok(()) => {
                if let Some(button) = button.upgrade() {
                    button.set_label(&"Saved...".to_string());
                }
                gtk::glib::timeout_future(std::time::Duration::from_secs(2)).await;
            }
            Err(error) => {
                if let Some(shell) = shell.upgrade() {
                    shell.control_feedback.show_feedback_toast(error);
                }
            }
        }
        if let Some(button) = button.upgrade() {
            button.set_label(&"Save".to_string());
            button.set_sensitive(true);
        }
    });
}

impl SourceSetupFlow for CredentialSetupFlow {
    fn view(&self, shell: &Rc<Preferences>, context: &SetupViewContext) -> gtk::Widget {
        let (scroller, content, actions, status) =
            setup_scaffold(shell, context, self.presentation);
        let host = credential_host(
            &self.draft,
            true,
            self.offer_authentication
                .then(|| Rc::clone(&self.authentication)),
        );
        content.append(&host.widget);
        let submit = Rc::clone(&self.submit);
        let authentication = Rc::clone(&self.authentication);
        append_credential_connect(
            shell,
            context,
            &content,
            &actions,
            status,
            host,
            move |source, input| {
                submit(source, input, authentication.get());
            },
        );
        scroller.upcast()
    }
}

fn setup_scaffold(
    shell: &Rc<Preferences>,
    _context: &SetupViewContext,
    registration: &'static SourcePresentation,
) -> (gtk::ScrolledWindow, gtk::Box, gtk::Box, gtk::Label) {
    let compact = true;
    let resource = crate::ui_resource::SETUP_SCAFFOLD_RESOURCE;
    let builder = gtk_widgets::ui_resource::builder(resource);
    gtk_widgets::objects!(builder, resource, {
        scroller: gtk::ScrolledWindow,
        clamp: adw::Clamp,
        content: gtk::Box,
        actions: gtk::Box,
        status: gtk::Label,
        source_summary: gtk::Box, source_icon: gtk::Image, source_title: gtk::Label, change_source: gtk::Button,
    });
    clamp.set_maximum_size(large_popup_content_width(ADD_SERVER_CLAMP_WIDTH));
    clamp.set_margin_top(if compact { 8 } else { 36 });
    clamp.set_margin_bottom(if compact { 20 } else { 36 });
    content.set_spacing(if compact { 10 } else { 18 });
    if compact {
        content.add_css_class("add-server-compact-content");
    }
    source_icon.set_icon_name(Some(
        gtk_widgets::source_labels::source_kind_icon_name(registration.kind)
            .expect("registered source presentation"),
    ));
    source_title.set_label(&String::from(gtk_widgets::source_labels::source_kind_title(
        registration.kind,
    )
    .expect("registered source presentation")));
    let weak = Rc::downgrade(shell);
    change_source.connect_clicked(move |_| {
        let Some(shell) = weak.upgrade() else { return };
        let Some(context) = shell.mounted_add_server_context() else {
            return;
        };
        *context.flow.borrow_mut() = Rc::new(SourceChoiceFlow);
        mount_setup_flow(&shell, &context);
    });
    content.append(&source_summary);
    if let SourceOperation::Failed {
        message,
        add_form: true,
        ..
    } = &*shell.source.operation.borrow()
    {
        status.set_text(message);
        status.add_css_class("error-text");
        status.set_visible(true);
    }
    (scroller, content, actions, status)
}

fn source_choice_selector(shell: &Rc<Preferences>, context: &SetupViewContext) -> gtk::Widget {
    let resource = crate::ui_resource::SOURCE_CHOICE_SELECTOR_RESOURCE;
    let builder = gtk_widgets::ui_resource::builder(resource);
    gtk_widgets::objects!(builder, resource, {
        wrapper: gtk::ScrolledWindow,
        server_sources: adw::PreferencesGroup, import_backup: gtk::Button,
        privacy: adw::PreferencesGroup, external_metadata: adw::SwitchRow,
        discord_presence: adw::SwitchRow,
    });
    for &presentation in source_presentations() {
        let resource = crate::ui_resource::SOURCE_CHOICE_RESOURCE;
        let builder = gtk_widgets::ui_resource::builder(resource);
        gtk_widgets::objects!(builder, resource, {
            row: adw::ActionRow, icon: gtk::Image,
        });
        row.set_title(&String::from(gtk_widgets::source_labels::source_kind_title(
            presentation.kind,
        )
        .expect("registered source presentation")));
        icon.set_icon_name(Some(
            gtk_widgets::source_labels::source_kind_icon_name(presentation.kind)
                .expect("registered source presentation"),
        ));
        row.update_property(&[gtk::accessible::Property::Label(&String::from(gtk_widgets::source_labels::source_kind_title(presentation.kind)
                .expect("registered source presentation")))]);
        let weak = Rc::downgrade(shell);
        row.connect_activated(move |_| {
            let Some(shell) = weak.upgrade() else { return };
            let Some(context) = shell.mounted_add_server_context() else {
                return;
            };
            *context.flow.borrow_mut() = (presentation.setup_flow)(&shell, presentation);
            mount_setup_flow(&shell, &context);
        });
        server_sources.add(&row);
    }
    privacy.set_visible(context.onboarding);
    {
        let settings = shell.settings.current.borrow();
        external_metadata.set_active(settings.application.external_metadata_enabled);
        discord_presence.set_active(settings.rich_presence.enabled);
    }
    let weak = Rc::downgrade(shell);
    external_metadata.connect_active_notify(move |row| {
        if let Some(shell) = weak.upgrade() {
            shell.set_external_metadata_enabled(row.is_active());
        }
    });
    let weak = Rc::downgrade(shell);
    discord_presence.connect_active_notify(move |row| {
        if let Some(shell) = weak.upgrade() {
            shell.settings.set_app_setting(
                "Discord presence setting",
                row.is_active(),
                |settings| &mut settings.rich_presence.enabled,
            );
            crate::preferences::show_discord_flatpak_help(row);
        }
    });

    import_backup.set_visible(context.onboarding);

    let weak = Rc::downgrade(shell);
    import_backup.connect_clicked(move |_| {
        if let Some(shell) = weak.upgrade() {
            crate::preferences::backup::import_dialog(&shell);
        }
    });
    wrapper.upcast()
}

fn mount_setup_flow(shell: &Rc<Preferences>, context: &SetupViewContext) {
    context.actions.borrow_mut().take();
    context.retry_focus.borrow_mut().take();
    let flow = context.flow.borrow().clone();
    let view = flow.view(shell, context);
    replace_add_server_content(&context.form, view);
    update_setup_surface(shell, context);
}

fn update_setup_surface(shell: &Rc<Preferences>, context: &SetupViewContext) {
    let operation = shell.source.operation.borrow();
    match setup_surface_state(&operation) {
        SetupSurfaceState::Progress => {
            remember_setup_focus(context);
            context
                .progress_status
                .set_text(&shell.source_connection_progress_text());
            if let Some(actions) = context.actions.borrow().as_ref() {
                actions.connect.set_sensitive(false);
            }
            context.surface.set_visible_child_name(SETUP_PROGRESS_PAGE);
        }
        SetupSurfaceState::Failed(message) => {
            if let Some(actions) = context.actions.borrow().as_ref() {
                actions.status.set_text(message);
                actions.status.add_css_class("error-text");
                actions.status.set_visible(true);
                actions.connect.set_sensitive((actions.ready)());
            }
            context.surface.set_visible_child_name(SETUP_FORM_PAGE);
            restore_setup_focus(context);
        }
        SetupSurfaceState::Form => {
            if let Some(actions) = context.actions.borrow().as_ref() {
                actions.status.remove_css_class("error-text");
                actions.status.set_text("");
                actions.status.set_visible(false);
                actions.connect.set_sensitive((actions.ready)());
            }
            context.surface.set_visible_child_name(SETUP_FORM_PAGE);
            restore_setup_focus(context);
        }
    }
}

fn remember_setup_focus(context: &SetupViewContext) {
    if context.retry_focus.borrow().is_some() {
        return;
    }
    let focus = context
        .content
        .root()
        .and_then(|root| root.focus())
        .filter(|focus| focus.is_ancestor(&context.form));
    *context.retry_focus.borrow_mut() = focus;
}

fn restore_setup_focus(context: &SetupViewContext) {
    if let Some(focus) = context.retry_focus.borrow_mut().take() {
        focus.grab_focus();
    }
}

fn setup_surface_state(operation: &SourceOperation) -> SetupSurfaceState<'_> {
    match operation {
        SourceOperation::Adding { .. } => SetupSurfaceState::Progress,
        SourceOperation::Failed {
            message,
            add_form: true,
            ..
        } => SetupSurfaceState::Failed(message),
        SourceOperation::Idle
        | SourceOperation::Switching { .. }
        | SourceOperation::Refreshing { .. }
        | SourceOperation::Downloading { .. }
        | SourceOperation::Failed {
            add_form: false, ..
        } => SetupSurfaceState::Form,
    }
}

fn credential_draft(preset: Option<CredentialPreset>) -> CredentialHostDraft {
    preset.map_or_else(
        || CredentialHostDraft {
            name: String::new(),
            url: "http://".to_string(),
            username: String::new(),
            password: String::new(),
            cert_verify: true,
        },
        |preset| CredentialHostDraft {
            name: preset.source_name,
            url: preset.server_url,
            username: preset.username,
            password: String::new(),
            cert_verify: !preset.trust_invalid_cert,
        },
    )
}

fn credential_host(
    draft: &Rc<RefCell<CredentialHostDraft>>,
    compact: bool,
    authentication: Option<Rc<Cell<SubsonicAuthentication>>>,
) -> CredentialHost {
    let snapshot = draft.borrow().clone();
    let host = credential_host_view(&snapshot, compact, authentication);
    bind_credential_draft(
        draft,
        &host.name,
        &host.url,
        &host.username,
        &host.password,
        &host.cert_verify,
    );
    host
}

fn credential_host_view(
    snapshot: &CredentialHostDraft,
    compact: bool,
    authentication: Option<Rc<Cell<SubsonicAuthentication>>>,
) -> CredentialHost {
    let resource = crate::ui_resource::CREDENTIAL_HOST_RESOURCE;
    let builder = gtk_widgets::ui_resource::builder(resource);
    gtk_widgets::objects!(builder, resource, {
        section: gtk::Box,
        fields_group: adw::PreferencesGroup,
        rows: adw::PreferencesGroup,
        name: adw::EntryRow,
        url: adw::EntryRow,
        username: adw::EntryRow,
        password: adw::PasswordEntryRow,
        cert_verify: adw::SwitchRow,
        api_key: adw::SwitchRow, legacy_password: adw::SwitchRow,
        save: gtk::Button,
    });
    name.set_text(&snapshot.name);
    style_compact_field_row(&name);
    url.set_text(&snapshot.url);
    style_compact_field_row(&url);
    let fields = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    fields.set_homogeneous(true);
    fields.set_hexpand(true);
    fields.append(&compact_field_row_group(&name));
    fields.append(&compact_field_row_group(&url));
    let fields = install_compact_field_row_responsiveness_at(&fields, 440);
    if !compact {
        fields_group.set_title(&"Server".to_string());
    }
    fields_group.add(&fields);

    username.set_text(&snapshot.username);
    style_compact_field_row(&username);
    password.set_text(&snapshot.password);
    style_compact_field_row(&password);
    cert_verify.set_active(snapshot.cert_verify);
    rows.add(&username);
    rows.add(&password);
    let authentication_toggles = authentication.as_ref().map(|authentication| {
        bind_open_subsonic_authentication(
            &api_key,
            &legacy_password,
            Rc::clone(authentication),
            &username,
            &password,
        );
        rows.add(&api_key);
        rows.add(&legacy_password);
        [api_key, legacy_password]
    });
    rows.add(&cert_verify);

    CredentialHost {
        widget: section,
        fields_group,
        name,
        url,
        username,
        password,
        cert_verify,
        authentication,
        authentication_toggles,
        save,
    }
}

pub(super) fn update_open_subsonic_authentication_fields(
    authentication: SubsonicAuthentication,
    username: &adw::EntryRow,
    secret: &adw::PasswordEntryRow,
) {
    let api_key = authentication == SubsonicAuthentication::ApiKey;
    username.set_visible(!api_key);
    secret.set_title(&if api_key {
        "API key".to_string()
    } else {
        "Password".to_string()
    });
}

fn bind_open_subsonic_authentication(
    api_key: &adw::SwitchRow,
    legacy_password: &adw::SwitchRow,
    authentication: Rc<Cell<SubsonicAuthentication>>,
    username: &adw::EntryRow,
    secret: &adw::PasswordEntryRow,
) {
    update_open_subsonic_authentication_fields(authentication.get(), username, secret);
    api_key.set_active(authentication.get() == SubsonicAuthentication::ApiKey);
    legacy_password.set_active(authentication.get() == SubsonicAuthentication::LegacyPassword);
    legacy_password.set_visible(!api_key.is_active());
    for (row, other, mode) in [
        (api_key, legacy_password, SubsonicAuthentication::ApiKey),
        (
            legacy_password,
            api_key,
            SubsonicAuthentication::LegacyPassword,
        ),
    ] {
        let other = other.downgrade();
        let authentication = Rc::clone(&authentication);
        let username = username.downgrade();
        let secret = secret.downgrade();
        row.connect_active_notify(move |row| {
            if mode == SubsonicAuthentication::ApiKey {
                if let Some(legacy) = other.upgrade() {
                    legacy.set_visible(!row.is_active());
                }
            }
            let next = if row.is_active() {
                if let Some(other) = other.upgrade() {
                    other.set_active(false);
                }
                mode
            } else if other.upgrade().is_some_and(|other| other.is_active()) {
                return;
            } else {
                SubsonicAuthentication::Password
            };
            authentication.set(next);
            if let (Some(username), Some(secret)) = (username.upgrade(), secret.upgrade()) {
                update_open_subsonic_authentication_fields(next, &username, &secret);
            }
        });
    }
}

fn bind_credential_draft(
    draft: &Rc<RefCell<CredentialHostDraft>>,
    name: &adw::EntryRow,
    url: &adw::EntryRow,
    username: &adw::EntryRow,
    password: &adw::PasswordEntryRow,
    cert_verify: &adw::SwitchRow,
) {
    let value = Rc::clone(draft);
    name.connect_text_notify(move |row| value.borrow_mut().name = row.text().to_string());
    let value = Rc::clone(draft);
    url.connect_text_notify(move |row| value.borrow_mut().url = row.text().to_string());
    let value = Rc::clone(draft);
    username.connect_text_notify(move |row| value.borrow_mut().username = row.text().to_string());
    let value = Rc::clone(draft);
    password.connect_text_notify(move |row| value.borrow_mut().password = row.text().to_string());
    let value = Rc::clone(draft);
    cert_verify.connect_active_notify(move |row| value.borrow_mut().cert_verify = row.is_active());
}

fn append_credential_connect(
    shell: &Rc<Preferences>,
    context: &SetupViewContext,
    content: &gtk::Box,
    actions: &gtk::Box,
    status: gtk::Label,
    host: CredentialHost,
    submit: impl Fn(&SourceHandle, CredentialInput) + 'static,
) {
    let login = text_button("rufin-network-server-symbolic", "Connect");
    login.add_css_class("suggested-action");
    login.set_sensitive(host.ready());
    connect_entry_row_activation(&host.name, &login);
    connect_entry_row_activation(&host.url, &login);
    connect_entry_row_activation(&host.username, &login);
    connect_password_entry_row_activation(&host.password, &login);
    let ready: Rc<dyn Fn() -> bool> = {
        let url = host.url.downgrade();
        let username = host.username.downgrade();
        let password = host.password.downgrade();
        let authentication = host.authentication.clone();
        Rc::new(move || {
            let (Some(url), Some(username), Some(password)) =
                (url.upgrade(), username.upgrade(), password.upgrade())
            else {
                return false;
            };
            remote_login_ready(
                &url,
                &username,
                &password,
                authentication.as_ref().is_some_and(|authentication| {
                    authentication.get() == SubsonicAuthentication::ApiKey
                }),
            )
        })
    };
    let update_ready: Rc<dyn Fn()> = {
        let login = login.downgrade();
        let ready = Rc::clone(&ready);
        Rc::new(move || {
            if let Some(login) = login.upgrade() {
                login.set_sensitive(ready());
            }
        })
    };
    {
        let update_ready = Rc::clone(&update_ready);
        host.url.connect_text_notify(move |_| update_ready());
    }
    {
        let update_ready = Rc::clone(&update_ready);
        host.username.connect_text_notify(move |_| update_ready());
    }
    {
        let update_ready = Rc::clone(&update_ready);
        host.password.connect_text_notify(move |_| update_ready());
    }
    if let Some(toggles) = host.authentication_toggles.as_ref() {
        for toggle in toggles {
            let update_ready = Rc::clone(&update_ready);
            toggle.connect_active_notify(move |_| update_ready());
        }
    }
    *context.actions.borrow_mut() = Some(SetupActions {
        status: status.clone(),
        connect: login.clone(),
        ready,
    });

    let source = shell.products.source.clone();
    let status_for_login = status.clone();
    let host_for_click = host.clone();
    login.connect_clicked(move |login| {
        if !host_for_click.ready() {
            let message = if host_for_click
                .authentication
                .as_ref()
                .is_some_and(|authentication| {
                    authentication.get() == SubsonicAuthentication::ApiKey
                }) {
                "Enter a server address and API key".to_string()
            } else {
                "Enter a server address, username, and password".to_string()
            };
            status_for_login.set_text(&message);
            status_for_login.set_visible(true);
            return;
        }
        let message = "Connecting to music server...".to_string();
        begin_connect_attempt(&status_for_login, login, &message);
        submit(&source, host_for_click.input());
    });
    actions.append(&login);
    content.append(actions);
    content.append(&status);
}

fn begin_connect_attempt(status: &gtk::Label, login: &gtk::Button, message: &str) {
    status.remove_css_class("error-text");
    status.set_text(message);
    status.set_visible(true);
    login.set_sensitive(false);
}

fn connect_entry_row_activation(entry: &adw::EntryRow, login: &gtk::Button) {
    let login = login.downgrade();
    entry.connect_entry_activated(move |_| {
        if let Some(login) = login.upgrade() {
            activate_connect_if_ready(&login);
        }
    });
}

fn connect_password_entry_row_activation(entry: &adw::PasswordEntryRow, login: &gtk::Button) {
    let login = login.downgrade();
    entry.connect_entry_activated(move |_| {
        if let Some(login) = login.upgrade() {
            activate_connect_if_ready(&login);
        }
    });
}

fn activate_connect_if_ready(login: &gtk::Button) -> bool {
    if !login.is_sensitive() {
        return false;
    }
    login.emit_clicked();
    true
}

fn trimmed_optional_text(row: &adw::EntryRow) -> Option<String> {
    let text = row.text();
    let trimmed = text.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

fn remote_login_ready(
    url: &adw::EntryRow,
    username: &adw::EntryRow,
    password: &adw::PasswordEntryRow,
    api_key: bool,
) -> bool {
    let address = url.text();
    let address = address.trim().trim_end_matches('/');
    let address_without_scheme = address
        .strip_prefix("http://")
        .or_else(|| address.strip_prefix("https://"))
        .unwrap_or(address);
    !address_without_scheme.trim().is_empty()
        && if api_key {
            !password.text().trim().is_empty()
        } else {
            !username.text().trim().is_empty()
        }
}

fn replace_add_server_content(content: &gtk::Box, child: gtk::Widget) {
    while let Some(current) = content.first_child() {
        content.remove(&current);
    }
    content.append(&child);
}

use gtk_widgets::field_layout::{
    compact_field_row_group, install_compact_field_row_responsiveness_at, style_compact_field_row,
};
