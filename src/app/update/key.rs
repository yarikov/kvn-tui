//! Keyboard input dispatchers for every overlay state.
//!
//! Entry point is `handle_key`, which routes by `Model.overlay`. Each
//! overlay has its own handler that returns the same `Vec<Effect>`
//! contract as the top-level `update` function. All handlers stay pure —
//! they mutate the `Model` and return effects; actual I/O happens in
//! the daemon.

use crossterm::event::KeyEvent;

use crate::app::effect::Effect;
use crate::app::model::{AppStatus, Model, Overlay};

use super::*;

pub(super) fn handle_key(model: &mut Model, key: KeyEvent) -> Vec<Effect> {
    match model.overlay {
        Overlay::None => handle_sources(model, key),
        Overlay::Help => {
            model.overlay = Overlay::None;
            vec![]
        }
        Overlay::ConfirmDelete => handle_confirm_delete(model, key),
        Overlay::RoutingMode => handle_routing_mode(model, key),
        Overlay::GeoRegions => handle_geo_region(model, key),
        Overlay::DnsSettings => handle_dns_settings(model, key),
        Overlay::ThemeSettings => handle_theme_picker(model, key),
        Overlay::ServiceRouting => handle_service_routing(model, key),
    }
}

/// Build the picker list. First entry is the Auto-follow-Omarchy slot
/// (only present when Omarchy is detected); the remaining entries are
/// the bundled palette names in alphabetical order.
pub fn theme_picker_slugs() -> Vec<String> {
    let mut out = Vec::new();
    if crate::omarchy::detect_omarchy_theme().is_some() {
        out.push(crate::tui_client::theme_watch::OMARCHY_SENTINEL.to_string());
    }
    out.extend(
        crate::ui::palette::Palette::bundled_names()
            .into_iter()
            .map(str::to_string),
    );
    out
}

/// Friendly label for an entry in the picker. The Auto entry is
/// annotated with the currently active Omarchy theme so the user can
/// see what would be applied; all others are returned verbatim.
fn theme_picker_label(slug: &str) -> String {
    if slug == crate::tui_client::theme_watch::OMARCHY_SENTINEL {
        match crate::omarchy::detect_omarchy_theme() {
            Some(name) => format!("Auto (Omarchy: {name})"),
            None => "Auto (Omarchy)".to_string(),
        }
    } else {
        slug.to_string()
    }
}

pub fn theme_picker_labels() -> Vec<String> {
    theme_picker_slugs()
        .iter()
        .map(|s| theme_picker_label(s))
        .collect()
}

pub(super) fn handle_sources(model: &mut Model, key: KeyEvent) -> Vec<Effect> {
    let mut effects = Vec::new();
    match key.code {
        // Navigation
        KeyCode::Char('j') | KeyCode::Down => model.select_next(),
        KeyCode::Char('k') | KeyCode::Up => model.select_prev(),
        KeyCode::Char('g') => model.select_first(),
        KeyCode::Char('G') => model.select_last(),

        // Actions. Note: 'p' (paste) and 'e' (edit profiles) are handled in
        // the TUI client directly, not routed through the daemon, so they
        // don't appear here.
        KeyCode::Enter => return handle_enter_on_sources(model),
        KeyCode::Char('d')
            if model.selected_profile().is_some() || model.selected_subscription().is_some() =>
        {
            if is_connection_affected(model) {
                let mut effects = vec![];
                push_status(
                    &mut effects,
                    model,
                    AppStatus::Error("Disconnect before deleting".into()),
                );
                return effects;
            }
            model.overlay = Overlay::ConfirmDelete;
        }
        KeyCode::Char('m') => {
            model.overlay = Overlay::RoutingMode;
            let available = model.config.settings.geo_routing.available_modes();
            model.routing_selected = available
                .iter()
                .position(|m| *m == model.config.settings.geo_routing.mode())
                .unwrap_or(0);
        }
        KeyCode::Char('u') => return handle_update_key(model),
        KeyCode::Char('I') => {
            let schedule = model.config.settings.geo_routing.auto_update.next();
            model.config.settings.geo_routing.auto_update = schedule;
            let mut effects = vec![Effect::SaveConfig, Effect::ResetGeoUpdateSchedules];
            if let Some(region) = model.config.settings.geo_routing.current_region {
                model.geo_retry_state = None;
                effects.push(Effect::ClearGeoRetryState { region });
            }
            push_status(
                &mut effects,
                model,
                AppStatus::Info(format!("Geo auto-update [{}]", schedule.label())),
            );
            return effects;
        }
        KeyCode::Char('i') => {
            if let Some(idx) = model.selected_subscription_index() {
                let (name, label) = if let Some(sub) = model.config.subscriptions.get_mut(idx) {
                    sub.auto_update = sub.auto_update.next();
                    sub.clear_retry_state();
                    sub.next_auto_update =
                        (sub.auto_update != SubscriptionAutoUpdate::Off).then(|| {
                            crate::config::profile::next_update_window_date(chrono::Local::now())
                        });
                    (sub.name.clone(), sub.auto_update.label())
                } else {
                    return effects;
                };
                let mut effects = vec![Effect::SaveConfig];
                push_status(
                    &mut effects,
                    model,
                    crate::app::model::AppStatus::Info(format!(
                        "Subscription '{}' [{}]",
                        name, label
                    )),
                );
                return effects;
            }
        }
        KeyCode::Char('o') => {
            model.overlay = Overlay::GeoRegions;
            model.geo_region_selected = model
                .config
                .settings
                .geo_routing
                .current_region
                .and_then(|r| GeoRegion::ALL.iter().position(|x| *x == r))
                .unwrap_or(0);
        }
        KeyCode::Char('r') if model.connection == ConnectionState::Connected => {
            if let Some(profile) = model.active_profile_id.and_then(|id| {
                model
                    .config
                    .profiles
                    .iter()
                    .find(|profile| profile.id == id)
            }) {
                let profile_id = profile.id;
                let profile_name = profile.name.clone();
                push_status(
                    &mut effects,
                    model,
                    crate::app::model::AppStatus::Info(format!(
                        "Reconnecting to {}…",
                        profile_name
                    )),
                );
                queue_connect(model, profile_id);
            }
        }
        KeyCode::Char('s') if model.connection == ConnectionState::Connected => {
            return vec![Effect::Disconnect];
        }
        KeyCode::Char('a') => {
            let new_val = !model.config.settings.auto_connect;
            model.config.settings.auto_connect = new_val;
            push_status(
                &mut effects,
                model,
                crate::app::model::AppStatus::Info(format!(
                    "Auto-connect {}",
                    if new_val { "enabled" } else { "disabled" }
                )),
            );
            effects.push(Effect::SaveConfig);
            return effects;
        }
        KeyCode::Char('K') => {
            if model.kill_switch_pending.is_some() {
                return effects;
            }
            let new_val = !model.config.settings.kill_switch;
            model.kill_switch_pending = Some(new_val);
            push_status(
                &mut effects,
                model,
                crate::app::model::AppStatus::Info(format!(
                    "Kill switch {}…",
                    if new_val { "enabling" } else { "disabling" }
                )),
            );
            effects.push(Effect::ApplyKillSwitch { enabled: new_val });
            return effects;
        }
        KeyCode::Char('D') => {
            model.overlay = Overlay::DnsSettings;
            model.dns_selected = 0;
            model.dns_strategy_draft = None;
            model.dns_fakeip_draft = None;
        }
        KeyCode::Char('S') => {
            model.overlay = Overlay::ServiceRouting;
            model.service_routing_selected = 0;
            model.service_routing_draft =
                Some(model.config.settings.geo_routing.service_routes.clone());
        }
        KeyCode::Char('t') => {
            if let Some(p) = model.selected_profile() {
                let id = p.id;
                if !model.testing_profiles.contains(&id) && !model.pending_tests.contains(&id) {
                    model.pending_tests.push_back(id);
                }
            }
        }
        KeyCode::Char('T') => {
            for p in &model.config.profiles {
                if !model.testing_profiles.contains(&p.id) && !model.pending_tests.contains(&p.id) {
                    model.pending_tests.push_back(p.id);
                }
            }
        }
        KeyCode::Char('C') => {
            let slugs = theme_picker_slugs();
            model.theme_selected = slugs
                .iter()
                .position(|s| s == &model.config.settings.theme)
                .unwrap_or(0);
            model.theme_draft = None;
            model.overlay = Overlay::ThemeSettings;
        }

        // Help
        KeyCode::Char('?') => model.overlay = Overlay::Help,

        _ => {}
    }
    effects
}

pub(super) fn handle_enter_on_sources(model: &mut Model) -> Vec<Effect> {
    let mut effects = Vec::new();
    if let Some(profile) = model.selected_profile() {
        let profile_id = profile.id;
        let profile_name = profile.name.clone();
        if model.active_profile_id == Some(profile_id) {
            return effects;
        }
        push_status(
            &mut effects,
            model,
            crate::app::model::AppStatus::Info(format!("Connecting to {}…", profile_name)),
        );
        queue_connect(model, profile_id);
    } else if let Some(sub) = model.selected_subscription() {
        let id = sub.id;
        let name = sub.name.clone();
        if !model
            .config
            .profiles
            .iter()
            .any(|p| p.subscription_id == Some(id))
        {
            if !download_allowed(model) {
                let mut result = vec![Effect::SaveConfig];
                push_download_blocked(&mut result, model, DownloadKind::Subscription);
                return result;
            }
            model.subscription_fetching = true;
            model.subscription_updates.insert(id);
            let mut result = vec![Effect::SaveConfig, Effect::UpdateSubscription { id }];
            push_status(
                &mut result,
                model,
                crate::app::model::AppStatus::Info(format!("Updating subscription '{}'…", name)),
            );
            return result;
        }
    } else {
        push_status(
            &mut effects,
            model,
            crate::app::model::AppStatus::Info("No sources. Press p to paste or e to edit.".into()),
        );
    }
    effects
}

pub(super) fn handle_update_key(model: &mut Model) -> Vec<Effect> {
    let mut effects = Vec::new();
    if let Some(idx) = model.selected_subscription_index() {
        if let Some(sub) = model.config.subscriptions.get(idx) {
            let id = sub.id;
            let name = sub.name.clone();
            if !download_allowed(model) {
                push_download_blocked(&mut effects, model, DownloadKind::Subscription);
                return effects;
            }
            model.subscription_fetching = true;
            model.subscription_updates.insert(id);
            let mut result = vec![Effect::SaveConfig, Effect::UpdateSubscription { id }];
            push_status(
                &mut result,
                model,
                crate::app::model::AppStatus::Info(format!("Updating subscription '{}'…", name)),
            );
            return result;
        }
    } else if !model.geo_updating {
        if model.config.settings.geo_routing.current_region == Some(GeoRegion::Global) {
            let services = model.config.settings.geo_routing.enabled_services();
            if services.is_empty() {
                push_status(
                    &mut effects,
                    model,
                    crate::app::model::AppStatus::Info(
                        "No enabled service rule-sets to update".to_string(),
                    ),
                );
                return effects;
            }
            if !download_allowed(model) {
                push_download_blocked(&mut effects, model, DownloadKind::Geo);
                return effects;
            }
            model.geo_updating = true;
            model.geo_automatic_update = false;
            push_status(
                &mut effects,
                model,
                crate::app::model::AppStatus::Info("Checking service rule-sets...".to_string()),
            );
            effects.push(Effect::RetryServiceRuleSets { services });
            return effects;
        }
        if !download_allowed(model) {
            push_download_blocked(&mut effects, model, DownloadKind::Geo);
            return effects;
        }
        model.geo_updating = true;
        model.geo_automatic_update = false;
        model.geo_last_attempt_at = Some(chrono::Local::now());
        push_status(
            &mut effects,
            model,
            crate::app::model::AppStatus::Info("Checking for geo updates...".to_string()),
        );
        effects.push(Effect::DownloadGeo);
        return effects;
    }
    effects
}

pub(super) fn handle_confirm_delete(model: &mut Model, key: KeyEvent) -> Vec<Effect> {
    match key.code {
        KeyCode::Char('y') | KeyCode::Enter => {
            if is_connection_affected(model) {
                model.overlay = Overlay::None;
                let mut effects = vec![];
                push_status(
                    &mut effects,
                    model,
                    AppStatus::Error("Disconnect before deleting".into()),
                );
                return effects;
            }
            model.overlay = Overlay::None;
            let row = model.selected_row();
            match row {
                Some(crate::app::model::SourceRow::StandaloneProfile(_))
                | Some(crate::app::model::SourceRow::SubscriptionProfile { .. }) => {
                    let name = model.selected_profile().map(|p| p.name.clone());
                    model.delete_selected();
                    let mut effects = vec![Effect::SaveConfig];
                    if let Some(name) = name {
                        push_status(
                            &mut effects,
                            model,
                            crate::app::model::AppStatus::Info(format!(
                                "Profile '{}' deleted",
                                name
                            )),
                        );
                    }
                    return effects;
                }
                Some(crate::app::model::SourceRow::SubscriptionHeader(_)) => {
                    let name = model.selected_subscription().map(|s| s.name.clone());
                    model.delete_selected();
                    let mut effects = vec![Effect::SaveConfig];
                    if let Some(name) = name {
                        push_status(
                            &mut effects,
                            model,
                            crate::app::model::AppStatus::Info(format!(
                                "Subscription '{}' deleted",
                                name
                            )),
                        );
                    }
                    return effects;
                }
                _ => {}
            }
        }
        KeyCode::Char('n') | KeyCode::Esc => {
            model.overlay = Overlay::None;
        }
        _ => {}
    }
    vec![]
}

pub(super) fn derive_subscription_name(url: &str) -> String {
    url::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(|h| h.to_string()))
        .unwrap_or_else(|| "Subscription".to_string())
}

pub(super) fn handle_ipc_command(
    model: &mut Model,
    cmd: crate::app::msg::IpcCommand,
) -> Vec<Effect> {
    use crate::app::msg::IpcCommand;
    let mut effects = match cmd {
        IpcCommand::Attach => vec![],
        IpcCommand::Detach => vec![],
        IpcCommand::Key { code, char, ctrl } => {
            let key_event = rebuild_key_event(&code, char, ctrl);
            if let Some(key) = key_event {
                handle_key(model, key)
            } else {
                vec![]
            }
        }
        IpcCommand::Paste { text } => handle_clipboard_text(model, &text),
        IpcCommand::Copied { name, count } => handle_copied_status(model, name, count),
        IpcCommand::ReloadConfig => {
            vec![Effect::ReloadConfig]
        }
        IpcCommand::Quit => vec![Effect::Quit],
        IpcCommand::ClientError { message } => {
            let mut inner = Vec::new();
            push_status(&mut inner, model, AppStatus::Error(message));
            inner
        }
    };
    effects.push(Effect::BroadcastState);
    effects
}

pub(super) fn rebuild_key_event(
    code: &str,
    ch: Option<char>,
    ctrl: bool,
) -> Option<crossterm::event::KeyEvent> {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let key_code = match code {
        "Enter" => KeyCode::Enter,
        "Esc" => KeyCode::Esc,
        "Up" => KeyCode::Up,
        "Down" => KeyCode::Down,
        "Tab" => KeyCode::Tab,
        "BackTab" => KeyCode::BackTab,
        "Char" => KeyCode::Char(ch.unwrap_or(' ')),
        _ => return None,
    };
    let mut modifiers = KeyModifiers::empty();
    if ctrl {
        modifiers |= KeyModifiers::CONTROL;
    }
    Some(KeyEvent::new(key_code, modifiers))
}

pub(super) fn handle_routing_mode(model: &mut Model, key: KeyEvent) -> Vec<Effect> {
    let available = model.config.settings.geo_routing.available_modes();
    match key.code {
        KeyCode::Char('j') | KeyCode::Down => {
            crate::ui::nav::select_next(&mut model.routing_selected, available.len());
        }
        KeyCode::Char('k') | KeyCode::Up => {
            crate::ui::nav::select_prev(&mut model.routing_selected);
        }
        KeyCode::Char('g') => {
            crate::ui::nav::select_first(&mut model.routing_selected);
        }
        KeyCode::Char('G') => {
            crate::ui::nav::select_last(&mut model.routing_selected, available.len());
        }
        KeyCode::Enter => {
            if let Some(&mode) = available.get(model.routing_selected) {
                let changed = model.config.settings.geo_routing.mode() != mode;
                model.config.settings.geo_routing.set_mode(mode);
                model.overlay = Overlay::None;
                let mut effects = vec![Effect::SaveConfig];
                push_status(
                    &mut effects,
                    model,
                    crate::app::model::AppStatus::Info(format!("Routing mode: {}", mode)),
                );

                if changed
                    && model.connection == ConnectionState::Connected
                    && let Some(active_id) = model.active_profile_id
                    && queue_connect(model, active_id)
                {
                    push_status(
                        &mut effects,
                        model,
                        crate::app::model::AppStatus::Info(format!(
                            "Mode changed to {} — reconnecting",
                            mode
                        )),
                    );
                }
                return effects;
            }
        }
        KeyCode::Char('q') | KeyCode::Esc => {
            model.overlay = Overlay::None;
        }
        _ => {}
    }
    vec![]
}

/// Handle keys inside the theme picker overlay. j/k drive a live preview
/// of the highlighted entry (the daemon stores the draft slug; the TUI
/// client resolves it into the actual `Theme` on snapshot apply).
pub(super) fn handle_theme_picker(model: &mut Model, key: KeyEvent) -> Vec<Effect> {
    let slugs = theme_picker_slugs();
    if slugs.is_empty() {
        model.overlay = Overlay::None;
        return vec![];
    }
    match key.code {
        KeyCode::Char('j') | KeyCode::Down => {
            crate::ui::nav::select_next(&mut model.theme_selected, slugs.len());
            model.theme_draft = slugs.get(model.theme_selected).cloned();
        }
        KeyCode::Char('k') | KeyCode::Up => {
            crate::ui::nav::select_prev(&mut model.theme_selected);
            model.theme_draft = slugs.get(model.theme_selected).cloned();
        }
        KeyCode::Char('g') => {
            crate::ui::nav::select_first(&mut model.theme_selected);
            model.theme_draft = slugs.first().cloned();
        }
        KeyCode::Char('G') => {
            crate::ui::nav::select_last(&mut model.theme_selected, slugs.len());
            model.theme_draft = slugs.get(model.theme_selected).cloned();
        }
        KeyCode::Enter => {
            let Some(slug) = slugs.get(model.theme_selected).cloned() else {
                return vec![];
            };
            let changed = model.config.settings.theme != slug;
            model.config.settings.theme = slug.clone();
            model.theme_draft = None;
            model.overlay = Overlay::None;
            if changed {
                let mut effects = vec![Effect::SaveConfig];
                push_status(
                    &mut effects,
                    model,
                    crate::app::model::AppStatus::Info(format!("Theme: {}", slug)),
                );
                return effects;
            }
            return vec![];
        }
        KeyCode::Char('q') | KeyCode::Esc => {
            model.theme_draft = None;
            model.overlay = Overlay::None;
        }
        _ => {}
    }
    vec![]
}

pub(super) fn handle_geo_region(model: &mut Model, key: KeyEvent) -> Vec<Effect> {
    let regions = &GeoRegion::ALL;
    match key.code {
        KeyCode::Char('j') | KeyCode::Down => {
            crate::ui::nav::select_next(&mut model.geo_region_selected, regions.len());
        }
        KeyCode::Char('k') | KeyCode::Up => {
            crate::ui::nav::select_prev(&mut model.geo_region_selected);
        }
        KeyCode::Char('g') => {
            crate::ui::nav::select_first(&mut model.geo_region_selected);
        }
        KeyCode::Char('G') => {
            crate::ui::nav::select_last(&mut model.geo_region_selected, regions.len());
        }
        KeyCode::Enter => {
            if let Some(&region) = regions.get(model.geo_region_selected) {
                let old_region = model.config.settings.geo_routing.current_region;
                let old_mode = model.config.settings.geo_routing.mode();
                let changed = old_region != Some(region);
                model.config.settings.geo_routing.set_region(region);
                model.overlay = Overlay::None;
                let mut effects = vec![Effect::SaveConfig];
                if changed {
                    effects.push(Effect::RefreshGeoLastUpdated);
                }
                push_status(
                    &mut effects,
                    model,
                    crate::app::model::AppStatus::Info(format!("Geo region: {}", region.as_str())),
                );

                // If the region changed and is not Global, check whether geo databases
                // are present and download them automatically if they are missing.
                if changed && region != GeoRegion::Global {
                    if download_allowed(model) {
                        model.geo_updating = true;
                        model.geo_last_attempt_at = Some(chrono::Local::now());
                        push_status(
                            &mut effects,
                            model,
                            crate::app::model::AppStatus::Info(
                                "Checking geo databases...".to_string(),
                            ),
                        );
                        effects.push(Effect::DownloadGeoIfMissing);
                    } else {
                        push_download_blocked(&mut effects, model, DownloadKind::Geo);
                    }
                }

                // Persist the previously active routing mode under the old region
                // and restore the mode stored for the newly selected region.
                if changed {
                    if let Some(old_region) = old_region {
                        model
                            .config
                            .settings
                            .geo_routing
                            .selected_region_modes
                            .insert(old_region, old_mode);
                    }
                    let new_mode = model.config.settings.geo_routing.mode();
                    if new_mode != old_mode {
                        push_status(
                            &mut effects,
                            model,
                            crate::app::model::AppStatus::Info(format!(
                                "Routing mode: {}",
                                new_mode
                            )),
                        );
                    }
                }

                // Trigger auto-connect immediately after picking a region
                // so the user does not have to restart the app.
                if model.connection == ConnectionState::Idle
                    && model.config.settings.auto_connect
                    && let Some(profile_id) = model.config.settings.last_connected_profile
                    && let Some(idx) = model
                        .config
                        .profiles
                        .iter()
                        .position(|p| p.id == profile_id)
                {
                    model.selected = crate::app::model::row_for_profile(&model.config, idx);
                    queue_connect(model, profile_id);
                    if let Some(profile) = model.config.profiles.get(idx) {
                        push_status(
                            &mut effects,
                            model,
                            crate::app::model::AppStatus::Info(format!(
                                "Auto-connecting to {}…",
                                profile.name
                            )),
                        );
                    }
                }

                if changed
                    && model.connection == ConnectionState::Connected
                    && let Some(active_id) = model.active_profile_id
                    && queue_connect(model, active_id)
                {
                    model.logs.push_back("Region changed — reconnecting".into());
                }
                return effects;
            }
        }
        KeyCode::Char('q') | KeyCode::Esc
            if model.config.settings.geo_routing.current_region.is_some() =>
        {
            model.overlay = Overlay::None;
        }
        _ => {}
    }
    vec![]
}

/// Items in the DNS settings overlay, in display order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DnsSettingsItem {
    PresetCloudflareDoh,
    PresetGoogleDot,
    PresetQuad9Doh,
    PresetSystemLocal,
    CycleStrategy,
    ToggleFakeIp,
}

impl DnsSettingsItem {
    pub const ALL: [DnsSettingsItem; 6] = [
        DnsSettingsItem::PresetCloudflareDoh,
        DnsSettingsItem::PresetGoogleDot,
        DnsSettingsItem::PresetQuad9Doh,
        DnsSettingsItem::PresetSystemLocal,
        DnsSettingsItem::CycleStrategy,
        DnsSettingsItem::ToggleFakeIp,
    ];

    pub fn from_index(idx: usize) -> Option<Self> {
        Self::ALL.get(idx).copied()
    }
}

pub(super) fn handle_dns_settings(model: &mut Model, key: KeyEvent) -> Vec<Effect> {
    let len = DnsSettingsItem::ALL.len();
    let on_strategy =
        DnsSettingsItem::from_index(model.dns_selected) == Some(DnsSettingsItem::CycleStrategy);
    let on_fakeip =
        DnsSettingsItem::from_index(model.dns_selected) == Some(DnsSettingsItem::ToggleFakeIp);
    match key.code {
        KeyCode::Char('j') | KeyCode::Down => {
            crate::ui::nav::select_next(&mut model.dns_selected, len);
        }
        KeyCode::Char('k') | KeyCode::Up => {
            crate::ui::nav::select_prev(&mut model.dns_selected);
        }
        KeyCode::Char('g') => crate::ui::nav::select_first(&mut model.dns_selected),
        KeyCode::Char('G') => crate::ui::nav::select_last(&mut model.dns_selected, len),
        KeyCode::Char('l') | KeyCode::Right if on_strategy => {
            let base = model
                .dns_strategy_draft
                .clone()
                .unwrap_or_else(|| model.config.settings.dns.strategy.clone());
            model.dns_strategy_draft = Some(base.next());
        }
        KeyCode::Char('h') | KeyCode::Left if on_strategy => {
            let base = model
                .dns_strategy_draft
                .clone()
                .unwrap_or_else(|| model.config.settings.dns.strategy.clone());
            model.dns_strategy_draft = Some(base.prev());
        }
        KeyCode::Char('l') | KeyCode::Right if on_fakeip => {
            let current = model
                .dns_fakeip_draft
                .unwrap_or(model.config.settings.dns.fakeip_enabled);
            model.dns_fakeip_draft = Some(!current);
        }
        KeyCode::Char('h') | KeyCode::Left if on_fakeip => {
            let current = model
                .dns_fakeip_draft
                .unwrap_or(model.config.settings.dns.fakeip_enabled);
            model.dns_fakeip_draft = Some(!current);
        }
        KeyCode::Enter => {
            let Some(item) = DnsSettingsItem::from_index(model.dns_selected) else {
                return vec![];
            };
            return apply_dns_item(model, item);
        }
        KeyCode::Char('q') | KeyCode::Esc => {
            model.overlay = Overlay::None;
            model.dns_strategy_draft = None;
            model.dns_fakeip_draft = None;
        }
        _ => {}
    }
    vec![]
}

fn apply_dns_item(model: &mut Model, item: DnsSettingsItem) -> Vec<Effect> {
    use crate::config::profile::{DnsServer, DnsStrategy};

    let mut effects = Vec::new();
    let mut servers_changed = false;
    let mut strategy_changed = false;
    let mut fakeip_changed = false;

    match item {
        DnsSettingsItem::PresetCloudflareDoh => {
            replace_preset(
                &mut model.config.settings.dns.servers,
                &mut model.config.settings.dns.final_server,
                vec![
                    DnsServer::Local {
                        tag: "local".to_string(),
                    },
                    DnsServer::Https {
                        tag: "remote".to_string(),
                        server: "1.1.1.1".to_string(),
                        server_port: None,
                        path: "/dns-query".to_string(),
                    },
                ],
                "remote",
            );
            servers_changed = true;
            push_status(
                &mut effects,
                model,
                AppStatus::Info("DNS preset: Cloudflare DoH (1.1.1.1)".into()),
            );
        }
        DnsSettingsItem::PresetGoogleDot => {
            replace_preset(
                &mut model.config.settings.dns.servers,
                &mut model.config.settings.dns.final_server,
                vec![
                    DnsServer::Local {
                        tag: "local".to_string(),
                    },
                    DnsServer::Tls {
                        tag: "remote".to_string(),
                        server: "8.8.8.8".to_string(),
                        server_port: Some(853),
                    },
                ],
                "remote",
            );
            servers_changed = true;
            push_status(
                &mut effects,
                model,
                AppStatus::Info("DNS preset: Google DoT (8.8.8.8)".into()),
            );
        }
        DnsSettingsItem::PresetQuad9Doh => {
            replace_preset(
                &mut model.config.settings.dns.servers,
                &mut model.config.settings.dns.final_server,
                vec![
                    DnsServer::Local {
                        tag: "local".to_string(),
                    },
                    DnsServer::Https {
                        tag: "remote".to_string(),
                        server: "9.9.9.9".to_string(),
                        server_port: None,
                        path: "/dns-query".to_string(),
                    },
                ],
                "remote",
            );
            servers_changed = true;
            push_status(
                &mut effects,
                model,
                AppStatus::Info("DNS preset: Quad9 DoH (9.9.9.9)".into()),
            );
        }
        DnsSettingsItem::PresetSystemLocal => {
            replace_preset(
                &mut model.config.settings.dns.servers,
                &mut model.config.settings.dns.final_server,
                vec![DnsServer::Local {
                    tag: "local".to_string(),
                }],
                "local",
            );
            servers_changed = true;
            push_status(
                &mut effects,
                model,
                AppStatus::Info("DNS preset: system resolver".into()),
            );
        }
        DnsSettingsItem::CycleStrategy => {
            // Commit the h/l-driven draft if it differs from the current
            // setting; without a draft, Enter is a no-op (h/l is the way to
            // change strategy now).
            let Some(draft) = model.dns_strategy_draft.take() else {
                return effects;
            };
            if draft == model.config.settings.dns.strategy {
                return effects;
            }
            model.config.settings.dns.strategy = draft.clone();
            model.config.settings.dns_strategy = draft.clone();
            strategy_changed = true;
            push_status(
                &mut effects,
                model,
                AppStatus::Info(format!("DNS strategy: {}", draft.as_str())),
            );
        }
        DnsSettingsItem::ToggleFakeIp => {
            let Some(enabled) = model.dns_fakeip_draft.take() else {
                return effects;
            };
            if enabled == model.config.settings.dns.fakeip_enabled {
                return effects;
            }
            model.config.settings.dns.fakeip_enabled = enabled;
            if enabled
                && !model
                    .config
                    .settings
                    .dns
                    .servers
                    .iter()
                    .any(|s| matches!(s, DnsServer::FakeIp { .. }))
            {
                model.config.settings.dns.servers.push(DnsServer::FakeIp {
                    tag: "fakeip".to_string(),
                    inet4_range: "198.18.0.0/15".to_string(),
                    inet6_range: "fc00::/18".to_string(),
                });
            }
            // Force a strategy that fake-IP can serve sensibly.
            if enabled && matches!(model.config.settings.dns.strategy, DnsStrategy::OnlyIpv6) {
                model.config.settings.dns.strategy = DnsStrategy::PreferIpv4;
                model.config.settings.dns_strategy = DnsStrategy::PreferIpv4;
            }
            fakeip_changed = true;
            push_status(
                &mut effects,
                model,
                AppStatus::Info(format!(
                    "Fake-IP {}",
                    if enabled { "enabled" } else { "disabled" }
                )),
            );
        }
    }

    if servers_changed || strategy_changed || fakeip_changed {
        effects.push(Effect::SaveConfig);
        effects.push(Effect::BroadcastState);
        if model.connection == ConnectionState::Connected
            && let Some(active_id) = model.active_profile_id
            && queue_connect(model, active_id)
        {
            push_status(
                &mut effects,
                model,
                AppStatus::Info("DNS changed — reconnecting…".into()),
            );
        }
    }

    effects
}

/// Handle keys inside the service routing overlay. `h`/`l` cycle the
/// highlighted service's draft route; Enter commits the whole draft (and
/// reconnects if the active tunnel is affected); Esc discards it.
pub(super) fn handle_service_routing(model: &mut Model, key: KeyEvent) -> Vec<Effect> {
    use crate::config::profile::{RoutedService, ServiceRoute};

    let len = RoutedService::ALL.len();
    match key.code {
        KeyCode::Char('j') | KeyCode::Down => {
            crate::ui::nav::select_next(&mut model.service_routing_selected, len);
        }
        KeyCode::Char('k') | KeyCode::Up => {
            crate::ui::nav::select_prev(&mut model.service_routing_selected);
        }
        KeyCode::Char('g') => crate::ui::nav::select_first(&mut model.service_routing_selected),
        KeyCode::Char('G') => crate::ui::nav::select_last(&mut model.service_routing_selected, len),
        KeyCode::Char('l') | KeyCode::Right | KeyCode::Char('h') | KeyCode::Left => {
            let Some(service) = RoutedService::ALL
                .get(model.service_routing_selected)
                .copied()
            else {
                return vec![];
            };
            // The draft is created when the overlay opens; a missing one
            // means inconsistent overlay state — do nothing.
            let Some(draft) = model.service_routing_draft.as_mut() else {
                return vec![];
            };
            let current = draft.get(&service).copied().unwrap_or_default();
            let forward = matches!(key.code, KeyCode::Char('l') | KeyCode::Right);
            let new = if forward {
                current.next()
            } else {
                current.prev()
            };
            // Absent = Disabled everywhere, so a route cycled back to
            // Disabled is removed rather than stored: a full cycle leaves
            // the draft identical to the committed map and Enter stays a
            // no-op instead of saving + reconnecting for nothing.
            if new == ServiceRoute::Disabled {
                draft.remove(&service);
            } else {
                draft.insert(service, new);
            }
        }
        KeyCode::Enter => {
            let Some(draft) = model.service_routing_draft.take() else {
                model.overlay = Overlay::None;
                return vec![];
            };
            model.overlay = Overlay::None;
            let changed = draft != model.config.settings.geo_routing.service_routes;
            if !changed {
                return vec![];
            }
            model.config.settings.geo_routing.service_routes = draft;
            let mut effects = vec![Effect::SaveConfig, Effect::BroadcastState];
            match model.connection {
                ConnectionState::Connected => {
                    // Don't reconnect yet: a first-enabled service's
                    // rule-sets may not be on disk, and the new sing-box
                    // would come up without its rules (inert until a second
                    // manual reconnect). Fetch them through the STILL-ACTIVE
                    // tunnel first; `Msg::ServiceRuleSetsReady` then
                    // reconnects the active profile (missing files degrade
                    // gracefully — the reconnect happens regardless).
                    model.pending_service_reconnect = true;
                    push_status(
                        &mut effects,
                        model,
                        crate::app::model::AppStatus::Info(
                            "Service routing changed — updating rule-sets".into(),
                        ),
                    );
                    effects.push(Effect::DownloadServiceRuleSetsIfMissing);
                }
                ConnectionState::Connecting | ConnectionState::ConnectPending => {
                    // A connect is already in flight with a settings snapshot
                    // taken before this commit — be honest that the change is
                    // not active yet.
                    push_status(
                        &mut effects,
                        model,
                        crate::app::model::AppStatus::Info(
                            "Service routing saved — takes effect on next reconnect".into(),
                        ),
                    );
                }
                _ => {
                    if model
                        .config
                        .settings
                        .geo_routing
                        .enabled_services()
                        .is_empty()
                    {
                        push_status(
                            &mut effects,
                            model,
                            crate::app::model::AppStatus::Info("Service routing updated".into()),
                        );
                    } else if download_allowed(model) {
                        push_status(
                            &mut effects,
                            model,
                            crate::app::model::AppStatus::Info(
                                "Service routing changed — updating rule-sets".into(),
                            ),
                        );
                        effects.push(Effect::DownloadServiceRuleSetsIfMissing);
                    } else {
                        push_download_blocked(&mut effects, model, DownloadKind::Geo);
                    }
                }
            }
            return effects;
        }
        KeyCode::Char('q') | KeyCode::Esc => {
            model.overlay = Overlay::None;
            model.service_routing_draft = None;
        }
        _ => {}
    }
    vec![]
}

fn replace_preset(
    servers: &mut Vec<crate::config::profile::DnsServer>,
    final_server: &mut String,
    preset: Vec<crate::config::profile::DnsServer>,
    new_final: &str,
) {
    *servers = preset;
    *final_server = new_final.to_string();
}

fn is_connection_affected(model: &Model) -> bool {
    let affected =
        |id| model.active_profile_id == Some(id) || model.connecting_profile_id == Some(id);
    if model.active_profile_id.is_none() && model.connecting_profile_id.is_none() {
        return false;
    }
    use crate::app::model::SourceRow;
    match model.selected_row() {
        Some(SourceRow::StandaloneProfile(_)) | Some(SourceRow::SubscriptionProfile { .. }) => {
            model
                .selected_profile()
                .map(|p| affected(p.id))
                .unwrap_or(false)
        }
        Some(SourceRow::SubscriptionHeader(_)) => model
            .selected_subscription()
            .map(|sub| {
                model
                    .config
                    .profiles
                    .iter()
                    .any(|p| p.subscription_id == Some(sub.id) && affected(p.id))
            })
            .unwrap_or(false),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::profile::{
        DnsServer, DnsStrategy, Profile, Subscription, SubscriptionAutoUpdate,
    };
    use crate::test_helpers::{key, model_with_profiles};
    use crossterm::event::{KeyCode, KeyEvent};
    use uuid::Uuid;

    fn enter() -> KeyEvent {
        KeyEvent::from(KeyCode::Enter)
    }

    fn esc() -> KeyEvent {
        KeyEvent::from(KeyCode::Esc)
    }

    fn with_dns_overlay() -> Model {
        let mut model = model_with_profiles(vec![]);
        model.overlay = Overlay::DnsSettings;
        model
    }

    fn final_server_is(model: &Model, expected_kind: fn(&DnsServer) -> bool) -> bool {
        model
            .config
            .settings
            .dns
            .final_server_entry()
            .map(expected_kind)
            .unwrap_or(false)
    }

    // ---- handle_dns_settings ----

    #[test]
    fn dns_settings_navigation_wraps() {
        let mut model = with_dns_overlay();
        assert_eq!(model.dns_selected, 0);
        handle_dns_settings(&mut model, key('j'));
        assert_eq!(model.dns_selected, 1);
        handle_dns_settings(&mut model, key('k'));
        assert_eq!(model.dns_selected, 0);
        handle_dns_settings(&mut model, key('G'));
        assert_eq!(model.dns_selected, DnsSettingsItem::ALL.len() - 1);
        handle_dns_settings(&mut model, key('g'));
        assert_eq!(model.dns_selected, 0);
    }

    #[test]
    fn dns_settings_navigation_arrows() {
        let mut model = with_dns_overlay();
        handle_dns_settings(&mut model, KeyEvent::from(KeyCode::Down));
        assert_eq!(model.dns_selected, 1);
        handle_dns_settings(&mut model, KeyEvent::from(KeyCode::Up));
        assert_eq!(model.dns_selected, 0);
    }

    #[test]
    fn dns_settings_h_l_only_on_strategy() {
        let mut model = with_dns_overlay();
        // Cursor on Cloudflare preset → h/l do nothing.
        handle_dns_settings(&mut model, key('l'));
        assert!(model.dns_strategy_draft.is_none());
        handle_dns_settings(&mut model, key('h'));
        assert!(model.dns_strategy_draft.is_none());
    }

    #[test]
    fn dns_settings_l_cycles_strategy_draft_forward() {
        let mut model = with_dns_overlay();
        model.dns_selected = DnsSettingsItem::ALL
            .iter()
            .position(|i| *i == DnsSettingsItem::CycleStrategy)
            .unwrap();
        // Default is PreferIpv4; l → PreferIpv6.
        handle_dns_settings(&mut model, key('l'));
        assert_eq!(model.dns_strategy_draft, Some(DnsStrategy::PreferIpv6));
        // l again → OnlyIpv4.
        handle_dns_settings(&mut model, KeyEvent::from(KeyCode::Right));
        assert_eq!(model.dns_strategy_draft, Some(DnsStrategy::OnlyIpv4));
    }

    #[test]
    fn dns_settings_h_cycles_strategy_draft_backward() {
        let mut model = with_dns_overlay();
        model.dns_selected = DnsSettingsItem::ALL
            .iter()
            .position(|i| *i == DnsSettingsItem::CycleStrategy)
            .unwrap();
        handle_dns_settings(&mut model, key('h'));
        assert_eq!(model.dns_strategy_draft, Some(DnsStrategy::OnlyIpv6));
        handle_dns_settings(&mut model, KeyEvent::from(KeyCode::Left));
        assert_eq!(model.dns_strategy_draft, Some(DnsStrategy::OnlyIpv4));
    }

    #[test]
    fn dns_settings_enter_strategy_no_draft_is_noop() {
        let mut model = with_dns_overlay();
        model.dns_selected = DnsSettingsItem::ALL
            .iter()
            .position(|i| *i == DnsSettingsItem::CycleStrategy)
            .unwrap();
        let before = model.config.settings.dns.strategy.clone();
        let effects = handle_dns_settings(&mut model, enter());
        assert!(effects.is_empty());
        assert_eq!(model.config.settings.dns.strategy, before);
    }

    #[test]
    fn dns_settings_enter_strategy_commits_draft() {
        let mut model = with_dns_overlay();
        model.dns_selected = DnsSettingsItem::ALL
            .iter()
            .position(|i| *i == DnsSettingsItem::CycleStrategy)
            .unwrap();
        model.dns_strategy_draft = Some(DnsStrategy::OnlyIpv4);
        let effects = handle_dns_settings(&mut model, enter());
        assert_eq!(model.config.settings.dns.strategy, DnsStrategy::OnlyIpv4);
        assert_eq!(model.config.settings.dns_strategy, DnsStrategy::OnlyIpv4);
        assert!(effects.contains(&Effect::SaveConfig));
        assert!(effects.contains(&Effect::BroadcastState));
        assert!(model.dns_strategy_draft.is_none());
    }

    #[test]
    fn dns_settings_enter_strategy_same_value_is_noop() {
        let mut model = with_dns_overlay();
        model.dns_selected = DnsSettingsItem::ALL
            .iter()
            .position(|i| *i == DnsSettingsItem::CycleStrategy)
            .unwrap();
        // Draft equals current — Enter discards draft, emits nothing.
        let same = model.config.settings.dns.strategy.clone();
        model.dns_strategy_draft = Some(same.clone());
        let effects = handle_dns_settings(&mut model, enter());
        assert!(effects.is_empty());
        assert!(model.dns_strategy_draft.is_none());
        assert_eq!(model.config.settings.dns.strategy, same);
    }

    #[test]
    fn dns_settings_enter_cloudflare_preset() {
        let mut model = with_dns_overlay();
        model.dns_selected = 0;
        let effects = handle_dns_settings(&mut model, enter());
        assert!(effects.contains(&Effect::SaveConfig));
        assert!(effects.contains(&Effect::BroadcastState));
        assert!(final_server_is(&model, |s| matches!(
            s,
            DnsServer::Https { server, .. } if server == "1.1.1.1"
        )));
    }

    #[test]
    fn dns_settings_enter_google_preset() {
        let mut model = with_dns_overlay();
        model.dns_selected = 1;
        handle_dns_settings(&mut model, enter());
        assert!(final_server_is(&model, |s| matches!(
            s,
            DnsServer::Tls { server, .. } if server == "8.8.8.8"
        )));
    }

    #[test]
    fn dns_settings_enter_quad9_preset() {
        let mut model = with_dns_overlay();
        model.dns_selected = 2;
        handle_dns_settings(&mut model, enter());
        assert!(final_server_is(&model, |s| matches!(
            s,
            DnsServer::Https { server, .. } if server == "9.9.9.9"
        )));
    }

    #[test]
    fn dns_settings_enter_system_preset() {
        let mut model = with_dns_overlay();
        model.dns_selected = 3;
        handle_dns_settings(&mut model, enter());
        assert!(final_server_is(&model, |s| matches!(
            s,
            DnsServer::Local { .. }
        )));
        assert_eq!(model.config.settings.dns.servers.len(), 1);
    }

    #[test]
    fn dns_settings_toggle_fakeip_enables_and_adds_server() {
        let mut model = with_dns_overlay();
        model.dns_selected = DnsSettingsItem::ALL
            .iter()
            .position(|i| *i == DnsSettingsItem::ToggleFakeIp)
            .unwrap();
        assert!(!model.config.settings.dns.fakeip_enabled);
        handle_dns_settings(&mut model, key('l'));
        assert_eq!(model.dns_fakeip_draft, Some(true));
        handle_dns_settings(&mut model, enter());
        assert!(model.config.settings.dns.fakeip_enabled);
        assert!(model.config.settings.dns.fakeip_server().is_some());
        assert!(model.dns_fakeip_draft.is_none());
    }

    #[test]
    fn dns_settings_toggle_fakeip_disables_back() {
        let mut model = with_dns_overlay();
        model.dns_selected = DnsSettingsItem::ALL
            .iter()
            .position(|i| *i == DnsSettingsItem::ToggleFakeIp)
            .unwrap();
        // Select and commit on.
        handle_dns_settings(&mut model, key('l'));
        handle_dns_settings(&mut model, enter());
        assert!(model.config.settings.dns.fakeip_enabled);
        // Select and commit off.
        handle_dns_settings(&mut model, key('h'));
        handle_dns_settings(&mut model, enter());
        assert!(!model.config.settings.dns.fakeip_enabled);
    }

    #[test]
    fn dns_settings_fakeip_cycles_in_both_directions() {
        let mut model = with_dns_overlay();
        model.dns_selected = DnsSettingsItem::ALL
            .iter()
            .position(|i| *i == DnsSettingsItem::ToggleFakeIp)
            .unwrap();

        handle_dns_settings(&mut model, key('l'));
        assert_eq!(model.dns_fakeip_draft, Some(true));
        handle_dns_settings(&mut model, KeyEvent::from(KeyCode::Right));
        assert_eq!(model.dns_fakeip_draft, Some(false));

        handle_dns_settings(&mut model, key('h'));
        assert_eq!(model.dns_fakeip_draft, Some(true));
        handle_dns_settings(&mut model, KeyEvent::from(KeyCode::Left));
        assert_eq!(model.dns_fakeip_draft, Some(false));
    }

    #[test]
    fn dns_settings_toggle_fakeip_forces_ipv4_strategy() {
        let mut model = with_dns_overlay();
        model.config.settings.dns.strategy = DnsStrategy::OnlyIpv6;
        model.config.settings.dns_strategy = DnsStrategy::OnlyIpv6;
        model.dns_selected = DnsSettingsItem::ALL
            .iter()
            .position(|i| *i == DnsSettingsItem::ToggleFakeIp)
            .unwrap();
        handle_dns_settings(&mut model, key('l'));
        handle_dns_settings(&mut model, enter());
        assert_eq!(model.config.settings.dns.strategy, DnsStrategy::PreferIpv4);
        assert_eq!(model.config.settings.dns_strategy, DnsStrategy::PreferIpv4);
    }

    #[test]
    fn dns_settings_esc_clears_draft_and_closes_overlay() {
        let mut model = with_dns_overlay();
        model.dns_selected = DnsSettingsItem::ALL
            .iter()
            .position(|i| *i == DnsSettingsItem::CycleStrategy)
            .unwrap();
        model.dns_strategy_draft = Some(DnsStrategy::OnlyIpv6);
        model.dns_fakeip_draft = Some(true);
        let effects = handle_dns_settings(&mut model, esc());
        assert!(effects.is_empty());
        assert_eq!(model.overlay, Overlay::None);
        assert!(model.dns_strategy_draft.is_none());
        assert!(model.dns_fakeip_draft.is_none());
    }

    #[test]
    fn dns_settings_q_closes_overlay() {
        let mut model = with_dns_overlay();
        handle_dns_settings(&mut model, key('q'));
        assert_eq!(model.overlay, Overlay::None);
    }

    #[test]
    fn dns_settings_changing_preset_while_connected_triggers_reconnect() {
        let a = Profile::new_vless("A".into(), "1.1.1.1".into(), 443, "u1".into());
        let b = Profile::new_vless("B".into(), "2.2.2.2".into(), 443, "u2".into());
        let active_id = a.id;
        let mut model = model_with_profiles(vec![a, b]);
        model.select_next();
        model.overlay = Overlay::DnsSettings;
        model.connection = ConnectionState::Connected;
        model.active_profile_id = Some(active_id);
        model.dns_selected = 1; // Google DoT
        let effects = handle_dns_settings(&mut model, enter());
        assert_eq!(model.connection, ConnectionState::Connecting);
        assert_eq!(model.connecting_profile_id, Some(active_id));
        assert!(!effects.iter().any(|e| matches!(e, Effect::Connect { .. })));
    }

    #[test]
    fn dns_settings_unknown_key_is_noop() {
        let mut model = with_dns_overlay();
        let before = model.dns_selected;
        let effects = handle_dns_settings(&mut model, key('z'));
        assert!(effects.is_empty());
        assert_eq!(model.dns_selected, before);
    }

    // ---- handle_sources gaps ----

    #[test]
    fn sources_d_blocked_when_active_profile_selected() {
        let mut model = model_with_profiles(vec![Profile::new_vless(
            "A".into(),
            "1.1.1.1".into(),
            443,
            "u".into(),
        )]);
        model.connection = ConnectionState::Connected;
        model.active_profile_id = Some(model.config.profiles[0].id);
        model.selected = 0;
        let effects = handle_sources(&mut model, key('d'));
        assert_eq!(model.overlay, Overlay::None);
        assert!(model.status.is_error());
        assert!(model.status.text().contains("Disconnect before"));
        // Effect carries the AppendAppLog only (no SaveConfig, no opening overlay).
        assert!(!effects.iter().any(|e| matches!(e, Effect::SaveConfig)));
    }

    #[test]
    fn sources_d_blocked_when_active_profile_under_subscription_header() {
        // Create a subscription with one profile that's currently active.
        let sub_id = Uuid::new_v4();
        let mut profile = Profile::new_vless("A".into(), "1.1.1.1".into(), 443, "u".into());
        profile.subscription_id = Some(sub_id);
        let mut model = model_with_profiles(vec![profile]);
        model.config.subscriptions.push(Subscription {
            id: sub_id,
            name: "Sub".into(),
            url: "http://s".into(),
            auto_update: SubscriptionAutoUpdate::Off,
            last_updated: None,
            next_auto_update: None,
            retry_state: None,
        });
        model.connection = ConnectionState::Connected;
        model.active_profile_id = Some(model.config.profiles[0].id);
        // Select the subscription header (first row now that the standalone group is empty).
        model.selected = crate::app::model::row_for_subscription_header(&model.config, 0);
        let _ = handle_sources(&mut model, key('d'));
        assert_eq!(model.overlay, Overlay::None);
        assert!(model.status.is_error());
    }

    #[test]
    fn sources_d_blocked_when_connecting_profile_selected() {
        let mut model = model_with_profiles(vec![Profile::new_vless(
            "A".into(),
            "1.1.1.1".into(),
            443,
            "u".into(),
        )]);
        model.connection = ConnectionState::ConnectPending;
        model.connecting_profile_id = Some(model.config.profiles[0].id);

        let effects = handle_sources(&mut model, key('d'));

        assert_eq!(model.overlay, Overlay::None);
        assert!(model.status.text().contains("Disconnect before"));
        assert!(!effects.contains(&Effect::SaveConfig));
    }

    #[test]
    fn confirm_delete_rechecks_connection_target() {
        let mut model = model_with_profiles(vec![Profile::new_vless(
            "A".into(),
            "1.1.1.1".into(),
            443,
            "u".into(),
        )]);
        model.overlay = Overlay::ConfirmDelete;
        model.connection = ConnectionState::ConnectPending;
        model.connecting_profile_id = Some(model.config.profiles[0].id);

        let effects = handle_confirm_delete(&mut model, enter());

        assert_eq!(model.config.profiles.len(), 1);
        assert_eq!(model.overlay, Overlay::None);
        assert!(model.status.text().contains("Disconnect before"));
        assert!(!effects.contains(&Effect::SaveConfig));
    }

    #[test]
    fn sources_o_opens_geo_regions_preselecting_current() {
        let mut model = model_with_profiles(vec![]);
        model.config.settings.geo_routing.set_region(GeoRegion::Cn);
        let _ = handle_sources(&mut model, key('o'));
        assert_eq!(model.overlay, Overlay::GeoRegions);
        assert_eq!(model.geo_region_selected, 1);
    }

    #[test]
    fn sources_o_preselects_ir_and_global_and_falls_back_to_zero() {
        let mut model = model_with_profiles(vec![]);
        model.config.settings.geo_routing.set_region(GeoRegion::Ir);
        let _ = handle_sources(&mut model, key('o'));
        assert_eq!(model.geo_region_selected, 2);

        let mut model = model_with_profiles(vec![]);
        model
            .config
            .settings
            .geo_routing
            .set_region(GeoRegion::Global);
        let _ = handle_sources(&mut model, key('o'));
        assert_eq!(model.geo_region_selected, 3);

        // No region set → defaults to 0.
        let mut model = model_with_profiles(vec![]);
        let _ = handle_sources(&mut model, key('o'));
        assert_eq!(model.geo_region_selected, 0);
    }

    #[test]
    fn sources_capital_d_opens_dns_settings_and_resets_draft() {
        let mut model = model_with_profiles(vec![]);
        model.dns_selected = 4;
        model.dns_strategy_draft = Some(DnsStrategy::OnlyIpv6);
        model.dns_fakeip_draft = Some(true);
        let effects = handle_sources(&mut model, key('D'));
        assert!(effects.is_empty());
        assert_eq!(model.overlay, Overlay::DnsSettings);
        assert_eq!(model.dns_selected, 0);
        assert!(model.dns_strategy_draft.is_none());
        assert!(model.dns_fakeip_draft.is_none());
    }

    #[test]
    fn sources_unknown_key_is_noop() {
        let mut model = model_with_profiles(vec![Profile::new_vless(
            "A".into(),
            "1.1.1.1".into(),
            443,
            "u".into(),
        )]);
        let before_selected = model.selected;
        let before_overlay = model.overlay;
        let effects = handle_sources(&mut model, key('z'));
        assert!(effects.is_empty());
        assert_eq!(model.selected, before_selected);
        assert_eq!(model.overlay, before_overlay);
    }

    #[test]
    fn sources_r_ignored_when_not_connected() {
        let mut model = model_with_profiles(vec![Profile::new_vless(
            "A".into(),
            "1.1.1.1".into(),
            443,
            "u".into(),
        )]);
        // Idle by default.
        let effects = handle_sources(&mut model, key('r'));
        assert!(effects.is_empty());
        assert_eq!(model.connection, ConnectionState::Idle);
    }

    #[test]
    fn sources_i_on_non_subscription_is_noop() {
        let mut model = model_with_profiles(vec![Profile::new_vless(
            "A".into(),
            "1.1.1.1".into(),
            443,
            "u".into(),
        )]);
        // Cursor on standalone profile, not on a subscription header.
        let effects = handle_sources(&mut model, key('i'));
        assert!(effects.is_empty());
    }

    // ---- handle_enter_on_sources gaps ----

    #[test]
    fn enter_on_empty_sources_lists_a_message() {
        let mut model = model_with_profiles(vec![]);
        let effects = handle_enter_on_sources(&mut model);
        // No profile, no subscription → status message, no effects beyond log.
        assert!(model.status.text().contains("No sources"));
        assert!(!effects.iter().any(|e| matches!(e, Effect::Connect { .. })));
    }

    // ---- handle_confirm_delete gaps ----

    #[test]
    fn confirm_delete_enter_on_profile_deletes() {
        let mut model = model_with_profiles(vec![Profile::new_vless(
            "A".into(),
            "1.1.1.1".into(),
            443,
            "u".into(),
        )]);
        model.overlay = Overlay::ConfirmDelete;
        let effects = handle_confirm_delete(&mut model, enter());
        assert!(model.config.profiles.is_empty());
        assert_eq!(model.overlay, Overlay::None);
        assert!(effects.contains(&Effect::SaveConfig));
    }

    #[test]
    fn confirm_delete_y_on_subscription_header_deletes_sub() {
        let sub_id = Uuid::new_v4();
        let mut profile = Profile::new_vless("A".into(), "1.1.1.1".into(), 443, "u".into());
        profile.subscription_id = Some(sub_id);
        let mut model = model_with_profiles(vec![profile]);
        model.config.subscriptions.push(Subscription {
            id: sub_id,
            name: "Example".into(),
            url: "http://e".into(),
            auto_update: SubscriptionAutoUpdate::Off,
            last_updated: None,
            next_auto_update: None,
            retry_state: None,
        });
        model.overlay = Overlay::ConfirmDelete;
        model.selected = crate::app::model::row_for_subscription_header(&model.config, 0);
        let effects = handle_confirm_delete(&mut model, key('y'));
        assert!(model.config.subscriptions.is_empty());
        assert!(model.config.profiles.is_empty());
        assert_eq!(model.overlay, Overlay::None);
        assert!(effects.iter().any(
            |e| matches!(e, Effect::AppendAppLog { message, .. } if message.contains("Example"))
        ));
    }

    #[test]
    fn confirm_delete_esc_cancels() {
        let mut model = model_with_profiles(vec![Profile::new_vless(
            "A".into(),
            "1.1.1.1".into(),
            443,
            "u".into(),
        )]);
        model.overlay = Overlay::ConfirmDelete;
        let effects = handle_confirm_delete(&mut model, esc());
        assert_eq!(model.config.profiles.len(), 1);
        assert_eq!(model.overlay, Overlay::None);
        assert!(effects.is_empty());
    }

    #[test]
    fn confirm_delete_unknown_key_noop() {
        let mut model = model_with_profiles(vec![Profile::new_vless(
            "A".into(),
            "1.1.1.1".into(),
            443,
            "u".into(),
        )]);
        model.overlay = Overlay::ConfirmDelete;
        let effects = handle_confirm_delete(&mut model, key('z'));
        assert_eq!(model.config.profiles.len(), 1);
        // Unknown key leaves overlay open.
        assert_eq!(model.overlay, Overlay::ConfirmDelete);
        assert!(effects.is_empty());
    }

    // ---- derive_subscription_name ----

    #[test]
    fn derive_subscription_name_uses_host_when_present() {
        assert_eq!(
            derive_subscription_name("https://example.com/sub"),
            "example.com"
        );
        assert_eq!(
            derive_subscription_name("https://sub.example.com:8443/path?q=1"),
            "sub.example.com"
        );
    }

    #[test]
    fn derive_subscription_name_ip_url() {
        assert_eq!(derive_subscription_name("http://1.2.3.4/sub"), "1.2.3.4");
    }

    #[test]
    fn derive_subscription_name_invalid_url_fallback() {
        assert_eq!(derive_subscription_name("not a url"), "Subscription");
    }

    #[test]
    fn derive_subscription_name_url_without_host_fallback() {
        // `file:` URLs parse but have no host.
        assert_eq!(derive_subscription_name("file:///tmp/sub"), "Subscription");
    }

    // ---- rebuild_key_event ----

    #[test]
    fn rebuild_key_event_named_codes() {
        use crossterm::event::{KeyCode, KeyModifiers};
        assert_eq!(
            rebuild_key_event("Enter", None, false).unwrap().code,
            KeyCode::Enter
        );
        assert_eq!(
            rebuild_key_event("Esc", None, false).unwrap().code,
            KeyCode::Esc
        );
        assert_eq!(
            rebuild_key_event("Up", None, false).unwrap().code,
            KeyCode::Up
        );
        assert_eq!(
            rebuild_key_event("Down", None, false).unwrap().code,
            KeyCode::Down
        );
        let ctrl_c = rebuild_key_event("Char", Some('c'), true).unwrap();
        assert_eq!(ctrl_c.code, KeyCode::Char('c'));
        assert_eq!(ctrl_c.modifiers, KeyModifiers::CONTROL);
    }

    #[test]
    fn rebuild_key_event_char_without_value_uses_space() {
        let event = rebuild_key_event("Char", None, false).unwrap();
        assert_eq!(event.code, crossterm::event::KeyCode::Char(' '));
    }

    #[test]
    fn rebuild_key_event_unknown_code_returns_none() {
        assert!(rebuild_key_event("F1", None, false).is_none());
        assert!(rebuild_key_event("", None, false).is_none());
    }

    // ---- handle_ipc_command gaps ----

    #[test]
    fn ipc_command_detach_emits_broadcast_only() {
        let mut model = model_with_profiles(vec![]);
        let effects = handle_ipc_command(&mut model, crate::app::msg::IpcCommand::Detach);
        assert_eq!(effects, vec![Effect::BroadcastState]);
    }

    #[test]
    fn ipc_command_quit_emits_quit_then_broadcast() {
        let mut model = model_with_profiles(vec![]);
        let effects = handle_ipc_command(&mut model, crate::app::msg::IpcCommand::Quit);
        assert_eq!(effects, vec![Effect::Quit, Effect::BroadcastState]);
    }

    #[test]
    fn ipc_command_copied_sets_status_and_broadcasts() {
        let mut model = model_with_profiles(vec![]);
        let effects = handle_ipc_command(
            &mut model,
            crate::app::msg::IpcCommand::Copied {
                name: "Alpha".into(),
                count: 3,
            },
        );
        assert!(effects.iter().any(|e| matches!(e, Effect::BroadcastState)));
        assert!(model.status.text().contains("Alpha"));
    }

    #[test]
    fn ipc_command_key_with_unknown_code_only_broadcasts() {
        let mut model = model_with_profiles(vec![]);
        let effects = handle_ipc_command(
            &mut model,
            crate::app::msg::IpcCommand::Key {
                code: "F1".into(),
                char: None,
                ctrl: false,
            },
        );
        assert_eq!(effects, vec![Effect::BroadcastState]);
    }

    // ---- handle_service_routing ----

    fn with_service_routing_overlay() -> Model {
        let mut model = model_with_profiles(vec![]);
        handle_sources(&mut model, key('S'));
        assert_eq!(model.overlay, Overlay::ServiceRouting);
        model
    }

    #[test]
    fn service_routing_opens_with_draft_copy_of_committed_routes() {
        use crate::config::profile::{RoutedService, ServiceRoute};
        let mut model = model_with_profiles(vec![]);
        model
            .config
            .settings
            .geo_routing
            .service_routes
            .insert(RoutedService::Steam, ServiceRoute::Direct);
        handle_sources(&mut model, key('S'));
        assert_eq!(model.overlay, Overlay::ServiceRouting);
        assert_eq!(model.service_routing_selected, 0);
        assert_eq!(
            model.service_routing_draft,
            Some(model.config.settings.geo_routing.service_routes.clone())
        );
    }

    #[test]
    fn service_routing_navigation_and_bounds() {
        use crate::config::profile::RoutedService;
        let mut model = with_service_routing_overlay();
        assert_eq!(model.service_routing_selected, 0);
        handle_service_routing(&mut model, key('j'));
        assert_eq!(model.service_routing_selected, 1);
        handle_service_routing(&mut model, key('k'));
        assert_eq!(model.service_routing_selected, 0);
        handle_service_routing(&mut model, key('G'));
        assert_eq!(model.service_routing_selected, RoutedService::ALL.len() - 1);
        handle_service_routing(&mut model, key('g'));
        assert_eq!(model.service_routing_selected, 0);
    }

    #[test]
    fn service_routing_l_and_h_cycle_draft_route() {
        use crate::config::profile::{RoutedService, ServiceRoute};
        let mut model = with_service_routing_overlay();
        let route_of = |m: &Model, s| {
            m.service_routing_draft
                .as_ref()
                .unwrap()
                .get(&s)
                .copied()
                .unwrap_or_default()
        };
        assert_eq!(
            route_of(&model, RoutedService::Steam),
            ServiceRoute::Disabled
        );
        handle_service_routing(&mut model, key('l'));
        assert_eq!(route_of(&model, RoutedService::Steam), ServiceRoute::Proxy);
        handle_service_routing(&mut model, key('l'));
        assert_eq!(route_of(&model, RoutedService::Steam), ServiceRoute::Direct);
        handle_service_routing(&mut model, key('h'));
        assert_eq!(route_of(&model, RoutedService::Steam), ServiceRoute::Proxy);
        // Only the highlighted service's draft changes; committed is untouched.
        assert_eq!(
            route_of(&model, RoutedService::Telegram),
            ServiceRoute::Disabled
        );
        assert!(model.config.settings.geo_routing.service_routes.is_empty());
    }

    #[test]
    fn service_routing_enter_commits_draft_and_saves() {
        use crate::config::profile::{RoutedService, ServiceRoute};
        let mut model = with_service_routing_overlay();
        handle_service_routing(&mut model, key('l')); // Steam → Proxy
        let effects = handle_service_routing(&mut model, enter());
        assert_eq!(model.overlay, Overlay::None);
        assert!(model.service_routing_draft.is_none());
        assert_eq!(
            model
                .config
                .settings
                .geo_routing
                .service_route(RoutedService::Steam),
            ServiceRoute::Proxy
        );
        assert!(effects.contains(&Effect::SaveConfig));
        assert!(effects.contains(&Effect::DownloadServiceRuleSetsIfMissing));
        // Not connected — no reconnect.
        assert_eq!(model.connection, ConnectionState::Idle);
    }

    #[test]
    fn service_routing_enter_without_changes_is_noop() {
        let mut model = with_service_routing_overlay();
        let effects = handle_service_routing(&mut model, enter());
        assert_eq!(model.overlay, Overlay::None);
        assert!(model.service_routing_draft.is_none());
        assert!(!effects.contains(&Effect::SaveConfig));
    }

    #[test]
    fn service_routing_esc_discards_draft() {
        let mut model = with_service_routing_overlay();
        handle_service_routing(&mut model, key('l'));
        handle_service_routing(&mut model, esc());
        assert_eq!(model.overlay, Overlay::None);
        assert!(model.service_routing_draft.is_none());
        assert!(model.config.settings.geo_routing.service_routes.is_empty());
    }

    #[test]
    fn service_routing_commit_while_connected_downloads_before_reconnect() {
        // First-enable ordering: the commit must NOT reconnect immediately —
        // a first-enabled service's rule-sets may not be on disk yet, and
        // the new sing-box would come up without its rules. Instead it
        // downloads using the still-active tunnel and defers the reconnect
        // to Msg::ServiceRuleSetsReady (covered in update.rs tests).
        let a = Profile::new_vless("A".into(), "e".into(), 1, "u".into());
        let active_id = a.id;
        let mut model = model_with_profiles(vec![a]);
        model.connection = ConnectionState::Connected;
        model.active_profile_id = Some(active_id);
        handle_sources(&mut model, key('S'));
        handle_service_routing(&mut model, key('l'));
        let effects = handle_service_routing(&mut model, enter());
        assert!(effects.contains(&Effect::SaveConfig));
        assert!(effects.contains(&Effect::DownloadServiceRuleSetsIfMissing));
        assert!(
            !effects.iter().any(|e| matches!(e, Effect::Connect { .. })),
            "reconnect must wait for the rule-set download to finish"
        );
        // The old tunnel keeps running while the download is in flight.
        assert_eq!(model.connection, ConnectionState::Connected);
        assert!(model.pending_service_reconnect);
    }

    #[test]
    fn service_routing_full_cycle_back_to_disabled_commits_as_noop() {
        // Disabled → Proxy → Direct → Disabled must land on a draft
        // identical to the (empty) committed map: absent = Disabled, so the
        // entry is removed rather than stored, and Enter neither saves nor
        // reconnects.
        let profile = Profile::new_vless("A".into(), "e".into(), 1, "u".into());
        let mut model = model_with_profiles(vec![profile.clone()]);
        model.connection = ConnectionState::Connected;
        model.active_profile_id = Some(profile.id);
        handle_sources(&mut model, key('S'));
        handle_service_routing(&mut model, key('l')); // Proxy
        handle_service_routing(&mut model, key('l')); // Direct
        handle_service_routing(&mut model, key('l')); // Disabled
        assert!(
            model.service_routing_draft.as_ref().unwrap().is_empty(),
            "cycling back to Disabled must remove the entry, not store it"
        );
        let effects = handle_service_routing(&mut model, enter());
        assert_eq!(model.overlay, Overlay::None);
        assert!(!effects.contains(&Effect::SaveConfig));
        assert!(!effects.contains(&Effect::DownloadServiceRuleSetsIfMissing));
        assert!(!model.pending_service_reconnect);
        assert_eq!(model.connection, ConnectionState::Connected);
    }

    #[test]
    fn service_routing_commit_during_pending_connect_saves_without_reconnect() {
        let profile = Profile::new_vless("A".into(), "e".into(), 1, "u".into());
        let mut model = model_with_profiles(vec![profile]);
        model.connection = ConnectionState::ConnectPending;
        handle_sources(&mut model, key('S'));
        handle_service_routing(&mut model, key('l'));
        let effects = handle_service_routing(&mut model, enter());
        assert!(effects.contains(&Effect::SaveConfig));
        assert!(!effects.iter().any(|e| matches!(e, Effect::Connect { .. })));
        assert_eq!(model.connection, ConnectionState::ConnectPending);
        assert_eq!(
            model.status.text(),
            "Service routing saved — takes effect on next reconnect"
        );
    }
}
