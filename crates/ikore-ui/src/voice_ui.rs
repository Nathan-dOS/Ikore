//! Voice dictation in the UI (ADR-029 V1–V8): the Settings page that
//! downloads the speech model and configures push-to-talk, and the reactions
//! to what the core reports while the developer dictates. Dictation itself
//! runs in the core, so it works while HVC is in the background.
use crate::activity::ActivityKind;
use crate::flow::FlowTarget;
use crate::session::HeaderNoticeKind;
use crate::settings::SettingsPage;
use crate::settings_ui::{PageContent, note, readout, row, section, select, stacked_row, switch};
use crate::settings_view::action;
use crate::{AppModel, AppView, theme};
use gpui::{Context, div, prelude::*};
use ikore_core::voice::{
    Dictation, Hotkey, Modifiers, SetupStage, VoiceBackend, VoiceSettings, VoiceStatus,
};

impl AppModel {
    /// Reads the voice status off the UI thread (it touches `SQLite` and the disk).
    pub(crate) fn refresh_voice_status(&mut self, cx: &mut Context<Self>) {
        let Some(core) = self.core.clone() else {
            return;
        };
        let runtime = core.runtime_handle().clone();
        cx.spawn(async move |this, cx| {
            let status = runtime.spawn_blocking(move || core.voice().status()).await;
            let _ = this.update(cx, |model, cx| {
                if let Ok(status) = status {
                    model.apply_voice_status(status, cx);
                }
            });
        })
        .detach();
    }

    /// Keeps the status and reacts once to each dictation outcome.
    pub(crate) fn apply_voice_status(&mut self, status: VoiceStatus, cx: &mut Context<Self>) {
        let previous = self
            .voice_status
            .replace(status.clone())
            .map(|previous| previous.dictation)
            .unwrap_or_default();
        if status.dictation != previous {
            match &status.dictation {
                Dictation::Saved { project_id, .. } => {
                    self.record_activity(
                        ActivityKind::Notes,
                        "Voice note saved",
                        Some(FlowTarget::View(AppView::Notes)),
                        false,
                    );
                    let open = self
                        .workspace
                        .read(cx)
                        .project_id
                        .as_ref()
                        .is_some_and(|id| id.as_ref() == project_id);
                    if open {
                        self.refresh_notes(cx);
                    }
                }
                Dictation::Failed(reason) => self.push_header_notice(
                    HeaderNoticeKind::Warning,
                    format!("Voice note not saved: {reason}"),
                    cx,
                ),
                Dictation::Idle | Dictation::Recording | Dictation::Transcribing => {}
            }
        }
        cx.notify();
    }

    /// Voice notes go to the project open in HVC.
    pub(crate) fn sync_voice_project(&self, cx: &Context<Self>) {
        if let Some(core) = &self.core {
            core.voice().set_active_project(
                self.workspace
                    .read(cx)
                    .project_id
                    .as_ref()
                    .map(ToString::to_string),
            );
        }
    }

    pub(crate) fn start_voice_setup(&mut self, cx: &mut Context<Self>) {
        if let Some(core) = &self.core {
            core.voice().start_setup();
        }
        self.refresh_voice_status(cx);
    }

    pub(crate) fn cancel_voice_setup(&mut self, cx: &mut Context<Self>) {
        if let Some(core) = &self.core {
            core.voice().cancel_setup();
        }
        self.refresh_voice_status(cx);
    }

    fn voice_settings(&self) -> VoiceSettings {
        self.voice_status
            .as_ref()
            .map(|status| status.settings)
            .unwrap_or_default()
    }

    pub(crate) fn save_voice_settings(&mut self, settings: VoiceSettings, cx: &mut Context<Self>) {
        let Some(core) = self.core.clone() else {
            return;
        };
        let runtime = core.runtime_handle().clone();
        cx.spawn(async move |this, cx| {
            let saved = runtime
                .spawn_blocking(move || core.voice().save_settings(settings))
                .await;
            let _ = this.update(cx, |model, cx| match saved {
                Ok(Ok(status)) => model.apply_voice_status(status, cx),
                Ok(Err(error)) => model.push_header_notice(
                    HeaderNoticeKind::Error,
                    format!("Voice settings were not saved: {error}"),
                    cx,
                ),
                Err(error) => model.push_header_notice(
                    HeaderNoticeKind::Error,
                    format!("Voice settings were not saved: {error}"),
                    cx,
                ),
            });
        })
        .detach();
    }

    pub(crate) fn toggle_voice_push_to_talk(&mut self, cx: &mut Context<Self>) {
        let mut settings = self.voice_settings();
        settings.push_to_talk = !settings.push_to_talk;
        self.save_voice_settings(settings, cx);
    }

    /// Options for the model selector and shortcut menus.
    pub(crate) fn voice_choices(&self, id: &str) -> Option<Vec<crate::select_menu::Choice>> {
        let hotkey = self.voice_settings().hotkey;
        match id {
            "voice-backend" => Some(
                VoiceBackend::ALL
                    .into_iter()
                    .map(|backend| {
                        crate::select_menu::Choice::new(
                            backend.id(),
                            backend.label(),
                            backend == self.voice_settings().backend,
                        )
                    })
                    .collect(),
            ),
            "voice-modifiers" => Some(
                Modifiers::ALL
                    .into_iter()
                    .map(|modifiers| {
                        crate::select_menu::Choice::new(
                            modifiers.id(),
                            modifiers.label(),
                            modifiers == hotkey.modifiers,
                        )
                    })
                    .collect(),
            ),
            "voice-key" => Some(
                Hotkey::FUNCTION_KEYS
                    .map(|key| {
                        crate::select_menu::Choice::new(
                            format!("f{key}"),
                            format!("F{key}"),
                            key == hotkey.function_key(),
                        )
                    })
                    .collect(),
            ),
            _ => None,
        }
    }

    pub(crate) fn apply_voice_choice(&mut self, id: &str, value: &str, cx: &mut Context<Self>) {
        let mut settings = self.voice_settings();
        if id == "voice-backend" {
            if let Some(backend) = VoiceBackend::parse(value) {
                settings.backend = backend;
                self.save_voice_settings(settings, cx);
            }
            return;
        }
        let current = settings.hotkey;
        let hotkey = match id {
            "voice-modifiers" => Modifiers::parse(value)
                .and_then(|modifiers| Hotkey::new(modifiers, current.function_key()).ok()),
            "voice-key" => value
                .strip_prefix('f')
                .and_then(|key| key.parse().ok())
                .and_then(|key| Hotkey::new(current.modifiers, key).ok()),
            _ => None,
        };
        if let Some(hotkey) = hotkey {
            settings.hotkey = hotkey;
            self.save_voice_settings(settings, cx);
        }
    }
}

fn megabytes(bytes: u64) -> u64 {
    bytes / 1_000_000
}

/// One line on what push-to-talk is doing.
fn push_to_talk_line(status: &VoiceStatus) -> (gpui::Rgba, String) {
    let hotkey = status.settings.hotkey.label();
    if let Some(error) = &status.hotkey_error {
        return (
            theme::danger(),
            format!("The shortcut is not active: {error}"),
        );
    }
    match &status.dictation {
        Dictation::Recording => (
            theme::accent(),
            "Recording… release the shortcut to save.".into(),
        ),
        Dictation::Transcribing => (theme::accent(), "Transcribing…".into()),
        Dictation::Failed(reason) if status.listening => (
            theme::warning(),
            format!("Last dictation not saved: {reason}"),
        ),
        _ if status.listening => (
            theme::success(),
            format!("Hold {hotkey} anywhere in Windows, speak, then release."),
        ),
        _ if !status.runtime.ready() => (
            theme::text_muted(),
            "Download the speech model first.".into(),
        ),
        _ => (theme::text_muted(), "Push-to-talk is off.".into()),
    }
}

pub(crate) fn voice_page(
    model: &AppModel,
    hl: Option<&str>,
    cx: &mut Context<AppModel>,
) -> PageContent {
    let page = PageContent::new(SettingsPage::Voice.label())
        .description(SettingsPage::Voice.description());
    // Rows render before the first status read, with their controls disabled.
    let checking = model.voice_status.is_none();
    let status = model
        .voice_status
        .clone()
        .unwrap_or_else(VoiceStatus::checking);
    if !status.runtime.platform_supported {
        return page.section(
            "Speech model",
            section(
                "Speech model",
                None,
                vec![note("Voice dictation is available on Windows only.")],
            ),
        );
    }
    let ready = status.runtime.ready();
    let running = status.setup.is_some();
    let model_line = format!(
        "{} · {} MB · {}",
        status.model_name,
        megabytes(status.model_bytes),
        if status.runtime.model_ready {
            "downloaded"
        } else {
            "not downloaded"
        }
    );
    let runtime_line = format!(
        "{} {} · {}",
        match status.settings.backend {
            VoiceBackend::Nemotron => "NeMo-Speech.cpp",
            VoiceBackend::WhisperLargeV3Turbo => "whisper.cpp",
        },
        status.runtime_version,
        if status.runtime.runtime_ready {
            "installed"
        } else {
            "not installed"
        }
    );
    let progress = match (&status.setup, &status.setup_error) {
        (Some(progress), _) if progress.total == 0 => Some((
            theme::accent(),
            "Preparing the local speech runtime…".into(),
        )),
        (Some(progress), _) => Some((
            theme::accent(),
            format!(
                "Downloading the {}: {}% ({} MB of {} MB)",
                match progress.stage {
                    SetupStage::Runtime => "runtime",
                    SetupStage::Model => "model",
                },
                (progress.received * 100)
                    .checked_div(progress.total)
                    .unwrap_or(0)
                    .min(100),
                megabytes(progress.received),
                megabytes(progress.total)
            ),
        )),
        (None, Some(error)) => Some((theme::danger(), format!("Download failed: {error}"))),
        (None, None) => None,
    };
    let control = if running {
        action(
            "settings-voice-cancel",
            "Cancel download",
            true,
            cx,
            |model, cx| model.cancel_voice_setup(cx),
        )
        .into_any_element()
    } else if ready {
        readout("Ready").into_any_element()
    } else {
        action(
            "settings-voice-download",
            "Download speech model",
            !checking,
            cx,
            |model, cx| model.start_voice_setup(cx),
        )
        .into_any_element()
    };
    let (line_color, line) = if checking {
        (theme::text_muted(), "Checking the voice setup…".to_string())
    } else {
        push_to_talk_line(&status)
    };
    let hotkey = status.settings.hotkey;
    page.section(
        "Speech model",
        section(
            "Speech model",
            None,
            vec![
                stacked_row(
                    "voice-model",
                    hl,
                    div()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .child(select(
                            "voice-backend",
                            status.settings.backend.label(),
                            !checking
                                && !running
                                && !matches!(
                                    status.dictation,
                                    Dictation::Recording | Dictation::Transcribing
                                ),
                            cx,
                        ))
                        .child(readout(model_line)),
                ),
                stacked_row(
                    "voice-setup",
                    hl,
                    div()
                        .debug_selector(|| "settings-voice-setup".into())
                        .flex()
                        .flex_col()
                        .gap_2()
                        .child(readout(runtime_line))
                        .child(div().child(control))
                        .when_some(progress, |body, (color, text)| {
                            body.child(div().text_sm().text_color(color).child(text))
                        }),
                ),
            ],
        ),
    )
    .section(
        "Push-to-talk",
        section(
            "Push-to-talk",
            None,
            vec![
                row(
                    "voice-push-to-talk",
                    hl,
                    switch(
                        "settings-voice-push-to-talk",
                        status.settings.push_to_talk,
                        ready,
                        cx,
                        |model, cx| model.toggle_voice_push_to_talk(cx),
                    ),
                ),
                row(
                    "voice-shortcut",
                    hl,
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(select(
                            "voice-modifiers",
                            hotkey.modifiers.label(),
                            !checking,
                            cx,
                        ))
                        .child(select(
                            "voice-key",
                            format!("F{}", hotkey.function_key()),
                            !checking,
                            cx,
                        )),
                ),
                div()
                    .debug_selector(|| "settings-voice-status".into())
                    .px_3()
                    .py_2()
                    .text_sm()
                    .text_color(line_color)
                    .child(line)
                    .into_any_element(),
            ],
        ),
    )
}
