// Included by interactive.rs to share the terminal panel and input lifecycle.
fn connection_input_window(
    buffer: &str,
    cursor: usize,
    secret: bool,
    width: usize,
) -> (String, usize) {
    let chars = buffer
        .chars()
        .map(|ch| if secret { '*' } else { ch })
        .collect::<Vec<_>>();
    let mut start = cursor.min(chars.len());
    let mut caret = 0;
    while start > 0 {
        let size = crate::prompt::char_width(chars[start - 1]);
        if caret + size >= width {
            break;
        }
        start -= 1;
        caret += size;
    }
    let mut shown = String::new();
    let mut used = 0;
    for ch in &chars[start..] {
        let size = crate::prompt::char_width(*ch);
        if used + size > width {
            break;
        }
        shown.push(*ch);
        used += size;
    }
    (shown, caret)
}

fn connection_input(
    ctx: &ScreenContext,
    title: &str,
    label: &str,
    value: &str,
    secret: bool,
) -> std::io::Result<Option<String>> {
    enable_raw_mode()?;
    let result = prompt_string_input_inner(ctx, title, label, value, secret);
    let _ = disable_raw_mode();
    result
}
fn connection_menu(
    ctx: &ScreenContext,
    title: &str,
    choices: &[String],
) -> std::io::Result<Option<usize>> {
    selection_menu(ctx, title, &[], choices, false, false)
}
fn connection_error(ctx: &ScreenContext, error: impl std::fmt::Display) -> std::io::Result<()> {
    selection_menu(
        ctx,
        crate::i18n::tr("Connection error"),
        &[error.to_string()],
        &[crate::i18n::tr("Back").into()],
        false,
        false,
    )?;
    Ok(())
}
fn edit_provider(
    ctx: &ScreenContext,
    initial: Option<&crate::providers::Provider>,
    config: &Config,
) -> std::io::Result<Option<crate::providers::Provider>> {
    use crate::providers::{ApiType, Provider};
    let title = crate::i18n::tr("Provider connection");
    let Some(name) = connection_input(
        ctx,
        title,
        crate::i18n::tr("Connection name:"),
        initial.map(|p| p.name.as_str()).unwrap_or(""),
        false,
    )?
    else {
        return Ok(None);
    };
    if name.is_empty() {
        return Ok(None);
    }
    let mut apis = ApiType::ALL.to_vec();
    if let Some(initial) = initial {
        apis.sort_by_key(|api| *api != initial.api_type);
    }
    let choices = apis
        .iter()
        .map(|api| api.label().to_string())
        .collect::<Vec<_>>();
    let Some(index) = connection_menu(ctx, crate::i18n::tr("API type"), &choices)? else {
        return Ok(None);
    };
    let api_type = apis[index];
    let default_url = initial
        .map(|p| p.base_url.as_str())
        .unwrap_or(api_type.default_url());
    let Some(base_url) =
        connection_input(ctx, title, crate::i18n::tr("Base URL:"), default_url, false)?
    else {
        return Ok(None);
    };
    let Some(key) = connection_input(
        ctx,
        title,
        crate::i18n::tr(if initial.is_some() {
            "API key (empty: keep existing):"
        } else {
            "API key (optional):"
        }),
        "",
        true,
    )?
    else {
        return Ok(None);
    };
    let Some(env) = connection_input(
        ctx,
        title,
        crate::i18n::tr("API key environment variable (optional):"),
        initial.and_then(|p| p.api_key_env.as_deref()).unwrap_or(""),
        false,
    )?
    else {
        return Ok(None);
    };
    let mut provider = Provider {
        name,
        api_type,
        base_url: crate::config::normalize_base_url(&base_url),
        api_key: if key.is_empty() {
            initial.map(|p| p.api_key.clone()).unwrap_or_default()
        } else {
            key
        },
        api_key_env: (!env.is_empty()).then_some(env),
        model: initial.map(|p| p.model.clone()).unwrap_or_default(),
        headers: initial.map(|p| p.headers.clone()).unwrap_or_default(),
        models: initial.map(|p| p.models.clone()).unwrap_or_default(),
    };
    if initial.is_some_and(|previous| {
        previous.api_type != provider.api_type
            || previous.base_url != provider.base_url
            || previous.api_key != provider.api_key
            || previous.api_key_env != provider.api_key_env
    }) {
        provider.models.clear();
        provider.model.clear();
    }
    if let Err(error) = provider.validate() {
        connection_error(ctx, error)?;
        return Ok(None);
    }
    // Discover against the draft's own endpoint and credentials. Catalog and
    // manual-model edits stay in memory until the whole connection is accepted.
    let mut draft = theme::UserPreferences::default();
    let id = "draft".to_string();
    draft.providers.insert(id.clone(), provider);
    let mut scoped = config.clone();
    draft
        .activate_provider(&id, &mut scoped)
        .map_err(std::io::Error::other)?;
    let Some(model) = select_model_inner(ctx, &scoped, &mut draft, false)? else {
        return Ok(None);
    };
    let mut provider = draft.providers.remove(&id).unwrap();
    provider.model = model;
    Ok(Some(provider))
}

pub fn manage_providers(
    ctx: &ScreenContext,
    prefs: &mut theme::UserPreferences,
    config: &Config,
) -> std::io::Result<Option<String>> {
    loop {
        let mut ids = prefs.providers.keys().cloned().collect::<Vec<_>>();
        ids.sort_by_key(|id| prefs.active_provider.as_ref() != Some(id));
        let mut choices = ids
            .iter()
            .map(|id| {
                let p = &prefs.providers[id];
                format!(
                    "{} {} · {} · {}",
                    if prefs.active_provider.as_ref() == Some(id) {
                        "✓"
                    } else {
                        " "
                    },
                    p.name,
                    p.api_type.label(),
                    p.base_url
                )
            })
            .collect::<Vec<_>>();
        choices.push(crate::i18n::tr("Add provider...").into());
        let Some(index) = connection_menu(ctx, crate::i18n::tr("Saved providers"), &choices)?
        else {
            return Ok(None);
        };
        if index == ids.len() {
            if let Some(provider) = edit_provider(ctx, None, config)? {
                let id = prefs.next_provider_id();
                prefs.providers.insert(id.clone(), provider);
                prefs.save()?;
                return Ok(Some(id));
            }
            continue;
        }
        let id = &ids[index];
        let actions = [
            "Use provider",
            "Edit connection",
            "Models",
            "Extra HTTP headers",
            "Clear API key",
            "Delete provider",
        ]
        .map(|label| crate::i18n::tr(label).into());
        match connection_menu(ctx, &prefs.providers[id].name, &actions)? {
            Some(0) => return Ok(Some(id.clone())),
            Some(1) => {
                if let Some(provider) = edit_provider(ctx, prefs.providers.get(id), config)? {
                    prefs.providers.insert(id.clone(), provider);
                    prefs.save()?;
                    if prefs.active_provider.as_ref() == Some(id) {
                        return Ok(Some(id.clone()));
                    }
                }
            }
            Some(2) => {
                let mut scoped = config.clone();
                let mut copy = prefs.clone();
                if let Err(error) = copy.activate_provider(id, &mut scoped) {
                    connection_error(ctx, error)?;
                    continue;
                }
                if let Some(model) = select_model(ctx, &scoped, prefs)? {
                    prefs.providers.get_mut(id).unwrap().model = model;
                    prefs.save()?;
                    if prefs.active_provider.as_ref() == Some(id) {
                        return Ok(Some(id.clone()));
                    }
                }
            }
            Some(3) => {
                if let Some(input) = connection_input(
                    ctx,
                    crate::i18n::tr("Extra HTTP headers"),
                    crate::i18n::tr("JSON object (empty: keep, {}: clear):"),
                    "",
                    true,
                )? {
                    if !input.is_empty() {
                        match serde_json::from_str(&input) {
                            Ok(headers) => {
                                let mut provider = prefs.providers[id].clone();
                                provider.headers = headers;
                                if let Err(error) = provider.validate() {
                                    connection_error(ctx, error)?;
                                    continue;
                                }
                                prefs.providers.insert(id.clone(), provider);
                                prefs.save()?;
                                if prefs.active_provider.as_ref() == Some(id) {
                                    return Ok(Some(id.clone()));
                                }
                            }
                            Err(error) => connection_error(ctx, error)?,
                        }
                    }
                }
            }
            Some(4) => {
                let p = prefs.providers.get_mut(id).unwrap();
                p.api_key.clear();
                p.api_key_env = None;
                prefs.save()?;
                if prefs.active_provider.as_ref() == Some(id) {
                    return Ok(Some(id.clone()));
                }
            }
            Some(5) => {
                if prefs.active_provider.as_ref() == Some(id) {
                    connection_error(
                        ctx,
                        crate::i18n::tr(
                            "Select another provider before deleting the active connection.",
                        ),
                    )?;
                } else if connection_menu(
                    ctx,
                    crate::i18n::tr("Delete provider?"),
                    &[
                        crate::i18n::tr("Cancel").into(),
                        crate::i18n::tr("Delete provider").into(),
                    ],
                )? == Some(1)
                {
                    prefs.providers.remove(id);
                    prefs.save()?;
                }
            }
            _ => {}
        }
    }
}

fn model_columns(model: &crate::providers::Model, width: usize) -> String {
    let id_width = (width / 2).max(1);
    let name = truncate_visible(model.label(), width.saturating_sub(id_width + 2));
    let id = truncate_visible(&model.id, id_width);
    format!(
        "{name}{}{id}",
        " ".repeat(width.saturating_sub(str_width(&name) + str_width(&id)))
    )
}

/// Search both names and IDs, with a background catalog request scoped to this connection.
pub fn select_model(
    ctx: &ScreenContext,
    config: &Config,
    prefs: &mut theme::UserPreferences,
) -> std::io::Result<Option<String>> {
    select_model_inner(ctx, config, prefs, true)
}

fn select_model_inner(
    ctx: &ScreenContext,
    config: &Config,
    prefs: &mut theme::UserPreferences,
    persist: bool,
) -> std::io::Result<Option<String>> {
    use crate::providers::Model;
    let mut models = config
        .provider_id
        .as_ref()
        .and_then(|id| prefs.providers.get(id))
        .map(|p| p.models.clone())
        .unwrap_or_default();
    if !config.model.is_empty() && !models.iter().any(|model| model.id == config.model) {
        models.insert(
            0,
            Model {
                id: config.model.clone(),
                name: config.model.clone(),
                context_window: None,
            },
        );
    }
    let start_request = || {
        let client = crate::llm::LlmClient::new(config.clone());
        let (tx, rx) = std::sync::mpsc::channel();
        let task = tokio::runtime::Handle::current().spawn(async move {
            let _ = tx.send(client.list_models().await);
        });
        (task, rx)
    };
    let (mut task, mut updates) = start_request();
    struct AbortTask(tokio::task::AbortHandle);
    impl Drop for AbortTask {
        fn drop(&mut self) {
            self.0.abort();
        }
    }
    let mut guard = AbortTask(task.abort_handle());
    enable_raw_mode()?;
    let result = (|| {
        let mut query = String::new();
        let mut selected = 0usize;
        let mut panel = BottomSelectionPanel::default();
        let mut status = crate::i18n::tr("Loading provider models...").to_string();
        loop {
            if let Ok(update) = updates.try_recv() {
                match update {
                    Ok(remote) => {
                        let count = remote.len();
                        for mut model in remote {
                            if let Some(existing) = models.iter_mut().find(|m| m.id == model.id) {
                                if !existing.name.is_empty() && existing.name != existing.id {
                                    model.name = existing.name.clone();
                                }
                                *existing = model;
                            } else {
                                models.push(model);
                            }
                        }
                        models.sort_by(|a, b| {
                            a.label()
                                .to_lowercase()
                                .cmp(&b.label().to_lowercase())
                                .then(a.id.cmp(&b.id))
                        });
                        status = crate::i18n::tf!("Provider models: {count}", count = count);
                        if let Some(p) = config
                            .provider_id
                            .as_ref()
                            .and_then(|id| prefs.providers.get_mut(id))
                        {
                            p.models = models.clone();
                            if persist {
                                prefs.save()?;
                            }
                        }
                    }
                    Err(error)
                        if error
                            .downcast_ref::<crate::providers::ModelCatalogAuthRequired>()
                            .is_some() =>
                    {
                        status = crate::i18n::tr("Catalog requires authorization. Set an API key in the connection, or add a model manually.").into();
                    }
                    Err(_) => {
                        status = crate::i18n::tr(
                            "Catalog unavailable. Use saved models or add a model manually.",
                        )
                        .into()
                    }
                }
            }
            let filtered = models
                .iter()
                .filter(|model| {
                    query.is_empty()
                        || model.id.to_lowercase().contains(&query.to_lowercase())
                        || model.label().to_lowercase().contains(&query.to_lowercase())
                })
                .collect::<Vec<_>>();
            selected = selected.min(filtered.len());
            let (_, rows) = crate::cli_ui::terminal_size();
            let slots = BottomSelectionPanel::visible_items(rows);
            let start = selected.saturating_sub(slots.saturating_sub(1));
            let mut lines = vec![
                (
                    format!("{} {query}", crate::i18n::tr("Search:")),
                    Color::White,
                ),
                (status.clone(), Color::DarkGrey),
            ];
            let width = crate::cli_ui::get_box_width().saturating_sub(8);
            for index in start..(start + slots).min(filtered.len() + 1) {
                let label = if index == filtered.len() {
                    crate::i18n::tr("Add model manually...").into()
                } else {
                    model_columns(filtered[index], width)
                };
                lines.push((
                    format!("{} {label}", if index == selected { ">" } else { " " }),
                    if index == selected {
                        theme::current().primary_crossterm()
                    } else {
                        Color::White
                    },
                ));
            }
            let provider_name = config
                .provider_id
                .as_ref()
                .and_then(|id| prefs.providers.get(id))
                .map(|p| p.name.as_str())
                .unwrap_or("");
            let title = format!("{} · {provider_name}", crate::i18n::tr("Provider models"));
            panel.draw(
                ctx,
                &title,
                &lines,
                crate::i18n::tr("Type to search · ↑↓ · Enter · Ctrl+R Refresh · Esc Back"),
            )?;
            if !event::poll(std::time::Duration::from_millis(100))? {
                continue;
            }
            match crate::cli_ui::read_event_with_background()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => match key.code {
                    KeyCode::Esc => return Ok(None),
                    KeyCode::Char('c') if key.modifiers.contains(event::KeyModifiers::CONTROL) => {
                        return Ok(None)
                    }
                    KeyCode::Char('r') if key.modifiers.contains(event::KeyModifiers::CONTROL) => {
                        task.abort();
                        (task, updates) = start_request();
                        guard.0 = task.abort_handle();
                        status = crate::i18n::tr("Loading provider models...").into();
                    }
                    KeyCode::Up => selected = selected.saturating_sub(1),
                    KeyCode::Down => selected = (selected + 1).min(filtered.len()),
                    KeyCode::PageUp => selected = selected.saturating_sub(slots),
                    KeyCode::PageDown => selected = (selected + slots).min(filtered.len()),
                    KeyCode::Home => selected = 0,
                    KeyCode::End => selected = filtered.len(),
                    KeyCode::Backspace => {
                        query.pop();
                        selected = 0;
                    }
                    KeyCode::Char(ch)
                        if !key.modifiers.intersects(
                            event::KeyModifiers::CONTROL | event::KeyModifiers::ALT,
                        ) =>
                    {
                        query.push(ch);
                        selected = 0;
                    }
                    KeyCode::Enter if selected < filtered.len() => {
                        return Ok(Some(filtered[selected].id.clone()))
                    }
                    KeyCode::Enter => {
                        let Some(id) = prompt_string_input(
                            ctx,
                            crate::i18n::tr("Add model manually..."),
                            crate::i18n::tr("Model ID:"),
                            &query,
                        )?
                        else {
                            continue;
                        };
                        if id.is_empty() {
                            continue;
                        }
                        let Some(name) = prompt_string_input(
                            ctx,
                            crate::i18n::tr("Add model manually..."),
                            crate::i18n::tr("Display name:"),
                            &id,
                        )?
                        else {
                            continue;
                        };
                        if let Some(p) = config
                            .provider_id
                            .as_ref()
                            .and_then(|id| prefs.providers.get_mut(id))
                        {
                            let model = Model {
                                id: id.clone(),
                                name,
                                context_window: None,
                            };
                            if let Some(existing) = p.models.iter_mut().find(|m| m.id == id) {
                                *existing = model;
                            } else {
                                p.models.push(model);
                            }
                            if persist {
                                prefs.save()?;
                            }
                        }
                        return Ok(Some(id));
                    }
                    _ => {}
                },
                _ => {}
            }
        }
    })();
    let _ = disable_raw_mode();
    result
}
