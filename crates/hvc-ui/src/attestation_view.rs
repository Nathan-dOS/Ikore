//! The Attestation card on the pulsation detail, the "attested" mark in the
//! list and the Security page in Settings (ADR-036 M3). The host seals, signs
//! and verifies; this module draws what it reports and forwards requests.
//! A changed file is "changed since the attestation", never tampering or
//! authorship.

use std::path::PathBuf;

use gpui::{AnyElement, ClipboardItem, Context, PathPromptOptions, div, prelude::*, px};
use hvc_core::attestation::{
    AttestationFileCheck, AttestationKeyState, AttestationState, AttestationStatus,
    AttestationVerification, FileKey, KEY_ID_PREFIX, RederivationCheck, SignatureCheck,
    WorkspaceCheck, commit_trailer,
};

use crate::app_model::AppModel;
use crate::icons::Icon;
use crate::pulsations::{AttestationAction, AttestationNotice};
use crate::settings::SettingsPage;
use crate::settings_ui::{PageContent, hint, note, readout, row, section, stacked_row, switch};
use crate::theme;
use crate::widgets::{ButtonVariant, btn, btn_confirm, icon, info_tip};

/// What an attestation holds and claims, shown wherever one is saved.
pub(crate) const CONTENTS: &str = "An attestation records the relative paths and SHA-256 digests of the files this pulsation applied, and the identifiers, digests, states and counters of its approvals, mandate revisions, stage runs and host checks. It never holds file content, prompts, reason text, absolute paths or credentials; a note named after the feature keeps that name in its relative path. It shows that the record is unchanged since sealing and which local key sealed it; it is not a SLSA level, a trusted time or a personal identity.";

/// The workspace paths listed under a check; the rest are counted.
const LISTED_PATHS: usize = 8;

/// The first eight hex digits of a key id, as in the saved key's file name.
pub(crate) fn short_key(key_id: &str) -> String {
    key_id
        .strip_prefix(KEY_ID_PREFIX)
        .unwrap_or(key_id)
        .chars()
        .take(8)
        .collect()
}

fn signature_text(check: SignatureCheck) -> (&'static str, gpui::Rgba) {
    match check {
        SignatureCheck::Valid => ("Signature valid", theme::success()),
        SignatureCheck::Invalid => ("Signature invalid", theme::danger()),
        SignatureCheck::Unsigned => ("Not signed", theme::warning()),
        SignatureCheck::UnknownKey => ("Signed by an unknown key", theme::warning()),
        SignatureCheck::TooLarge => ("Too large to sign", theme::warning()),
    }
}

fn rederivation_text(check: &AttestationVerification) -> (String, gpui::Rgba) {
    match check.rederivation {
        RederivationCheck::Matches => (
            "Re-derived from the current records: matches".into(),
            theme::success(),
        ),
        RederivationCheck::Differs => (
            match &check.difference {
                Some(block) => format!("Re-derived from the current records: differs in `{block}`"),
                None => "Re-derived from the current records: differs".into(),
            },
            theme::danger(),
        ),
        RederivationCheck::Unavailable => (
            "Re-derivation unavailable: the records it needs are gone".into(),
            theme::text_muted(),
        ),
    }
}

const fn workspace_word(state: WorkspaceCheck) -> &'static str {
    match state {
        WorkspaceCheck::Unchanged => "unchanged",
        WorkspaceCheck::Modified => "changed since the attestation",
        WorkspaceCheck::Missing => "missing",
        WorkspaceCheck::Unreadable => "unreadable",
    }
}

fn workspace_summary(check: &AttestationVerification) -> String {
    let count = |state| {
        check
            .workspace
            .iter()
            .filter(|subject| subject.state == state)
            .count()
    };
    let mut parts = vec![format!("{} unchanged", count(WorkspaceCheck::Unchanged))];
    for state in [
        WorkspaceCheck::Modified,
        WorkspaceCheck::Missing,
        WorkspaceCheck::Unreadable,
    ] {
        let n = count(state);
        if n > 0 {
            parts.push(format!("{n} {}", workspace_word(state)));
        }
    }
    format!("Workspace now · {}", parts.join(" · "))
}

/// The list's mark for a pulsation with a signed attestation.
pub(crate) fn attested_badge() -> AnyElement {
    div()
        .debug_selector(|| "pulsation-attested".into())
        .flex_shrink_0()
        .flex()
        .items_center()
        .gap_1()
        .child(icon(Icon::ShieldCheck, px(11.0), theme::success()))
        .child(
            div()
                .text_xs()
                .text_color(theme::success())
                .child("attested"),
        )
        .into_any_element()
}

/// Headline, its colour and the explanation under it, by state.
fn headline(status: &AttestationStatus) -> (String, gpui::Rgba, Option<String>) {
    let files = format!("{} file(s)", status.subject_count);
    match status.state {
        AttestationState::NotSealed => (
            "Not sealed".into(),
            theme::text_muted(),
            Some("This pulsation ended before attestations existed.".into()),
        ),
        AttestationState::Pending => (
            "Sealing…".into(),
            theme::text_muted(),
            Some("The host seals the record once this pulsation's file writes settle.".into()),
        ),
        AttestationState::Skipped => (
            "Not sealed".into(),
            theme::text_muted(),
            Some("Sealing was off in Settings → Security when this pulsation ended.".into()),
        ),
        AttestationState::Failed => (
            "Sealing failed".into(),
            theme::danger(),
            status.diagnostic.clone(),
        ),
        AttestationState::Signed => (
            format!(
                "Signed by key {} · {files}",
                status.key_id.as_deref().map(short_key).unwrap_or_default()
            ),
            theme::success(),
            None,
        ),
        AttestationState::Unsigned => (
            format!("Unsigned · {files}"),
            theme::warning(),
            Some("No attestation key was available when this pulsation was sealed. The record is kept; sign it once the key is ready.".into()),
        ),
        AttestationState::TooLarge => (
            format!("Too large to sign · {files}"),
            theme::warning(),
            Some("The statement is over 1 MiB, so only its digest is kept. It can be checked here but not saved.".into()),
        ),
    }
}

fn notice_text(notice: &AttestationNotice) -> (String, gpui::Rgba) {
    match notice {
        AttestationNotice::Verified => ("Checked again.".into(), theme::text_muted()),
        AttestationNotice::Signed => ("Signed.".into(), theme::text_muted()),
        AttestationNotice::Requeued => ("Queued to seal again.".into(), theme::text_muted()),
        AttestationNotice::Saved(paths) => (
            format!(
                "Saved {}.",
                paths
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect::<Vec<_>>()
                    .join(" and ")
            ),
            theme::text_muted(),
        ),
        AttestationNotice::TrailerCopied => (
            "Commit trailer copied to the clipboard.".into(),
            theme::text_muted(),
        ),
        AttestationNotice::Failed(error) => (error.clone(), theme::danger()),
    }
}

fn verification_lines(check: &AttestationVerification) -> AnyElement {
    let (signature, signature_color) = signature_text(check.signature);
    let (rederivation, rederivation_color) = rederivation_text(check);
    let flagged = check
        .workspace
        .iter()
        .filter(|subject| subject.state != WorkspaceCheck::Unchanged)
        .collect::<Vec<_>>();
    div()
        .debug_selector(|| "attestation-verification".into())
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .text_xs()
                .text_color(theme::text_muted())
                .child(format!(
                    "Last checked {}",
                    crate::time::format_timestamp(&check.checked_at)
                )),
        )
        .child(div().text_sm().text_color(signature_color).child(signature))
        .child(
            div()
                .text_sm()
                .text_color(rederivation_color)
                .child(rederivation),
        )
        .child(
            div()
                .text_sm()
                .text_color(if flagged.is_empty() {
                    theme::text_muted()
                } else {
                    theme::warning()
                })
                .child(workspace_summary(check)),
        )
        .children(
            flagged
                .iter()
                .take(LISTED_PATHS)
                .enumerate()
                .map(|(index, subject)| {
                    div()
                        .debug_selector(move || format!("attestation-workspace-{index}"))
                        .flex()
                        .flex_wrap()
                        .gap_2()
                        .child(
                            div()
                                .font_family(theme::FONT_MONO)
                                .text_xs()
                                .child(subject.path.clone()),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme::warning())
                                .child(workspace_word(subject.state)),
                        )
                }),
        )
        .when(flagged.len() > LISTED_PATHS, |list| {
            list.child(
                div()
                    .text_xs()
                    .text_color(theme::text_muted())
                    .child(format!("{} more", flagged.len() - LISTED_PATHS)),
            )
        })
        .into_any_element()
}

/// The Attestation card, once the pulsation has ended.
pub(crate) fn attestation_panel(model: &AppModel, cx: &mut Context<AppModel>) -> AnyElement {
    let state = model.pulsations.read(cx);
    let Some(status) = state
        .detail()
        .and_then(|detail| detail.attestation.clone())
        .filter(|status| status.state != AttestationState::NotSealed)
    else {
        return div().into_any_element();
    };
    let busy = state.attestation.busy;
    let notice = state.attestation.notice.clone();
    let (title, color, explanation) = headline(&status);
    let checkable = matches!(
        status.state,
        AttestationState::Signed | AttestationState::Unsigned | AttestationState::TooLarge
    );
    let signed = status.state == AttestationState::Signed;
    div()
        .id("pulsation-attestation")
        .debug_selector(|| "pulsation-attestation".into())
        .mb_3()
        .p_3()
        .rounded_md()
        .border_1()
        .border_color(theme::border())
        .min_w_0()
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(icon(Icon::ShieldCheck, px(16.0), theme::accent()))
                .child(
                    div()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child("Attestation"),
                )
                .child(info_tip("pulsation-attestation", CONTENTS)),
        )
        .child(
            div()
                .debug_selector(|| "attestation-headline".into())
                .min_w_0()
                .text_sm()
                .text_color(color)
                .child(title),
        )
        .when_some(explanation, |card, text| {
            card.child(
                div()
                    .min_w_0()
                    .text_xs()
                    .text_color(theme::text_muted())
                    .child(text),
            )
        })
        .when_some(status.statement_sha256.clone(), |card, digest| {
            card.child(
                div()
                    .debug_selector(|| "attestation-digest".into())
                    .min_w_0()
                    .font_family(theme::FONT_MONO)
                    .text_xs()
                    .text_color(theme::text_muted())
                    .child(format!("sha256:{digest}")),
            )
        })
        .when_some(status.verification.as_ref(), |card, check| {
            card.child(verification_lines(check))
        })
        .when_some(notice, |card, notice| {
            let (text, color) = notice_text(&notice);
            card.child(
                div()
                    .debug_selector(|| "attestation-notice".into())
                    .min_w_0()
                    .text_xs()
                    .text_color(color)
                    .child(text),
            )
        })
        .child(
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap_2()
                .when(checkable, |row| {
                    row.child(btn(
                        "attestation-verify",
                        if busy { "Checking…" } else { "Verify again" },
                        if signed {
                            ButtonVariant::Primary
                        } else {
                            ButtonVariant::Secondary
                        },
                        !busy,
                        cx,
                        |model, cx| {
                            model.run_attestation_action(AttestationAction::VerifyAgain, cx)
                        },
                    ))
                })
                .when(signed, |row| {
                    row.child(btn(
                        "attestation-save",
                        "Save attestation…",
                        ButtonVariant::Secondary,
                        !busy,
                        cx,
                        |model, cx| model.save_attestation(cx),
                    ))
                    .child(btn(
                        "attestation-trailer",
                        "Copy commit trailer",
                        ButtonVariant::Secondary,
                        !busy,
                        cx,
                        |model, cx| model.copy_attestation_trailer(cx),
                    ))
                })
                .when(status.state == AttestationState::Unsigned, |row| {
                    row.child(btn(
                        "attestation-sign",
                        "Sign now",
                        ButtonVariant::Primary,
                        !busy,
                        cx,
                        |model, cx| model.run_attestation_action(AttestationAction::SignNow, cx),
                    ))
                })
                .when(status.state == AttestationState::Failed, |row| {
                    row.child(btn(
                        "attestation-retry",
                        "Seal again",
                        ButtonVariant::Primary,
                        !busy,
                        cx,
                        |model, cx| model.run_attestation_action(AttestationAction::SealAgain, cx),
                    ))
                }),
        )
        .into_any_element()
}

fn key_state_text(state: AttestationKeyState) -> (&'static str, gpui::Rgba) {
    match state {
        AttestationKeyState::Ready => ("Ready", theme::success()),
        AttestationKeyState::NoKey => (
            "No key yet: the first pulsation that ends creates one",
            theme::text_muted(),
        ),
        AttestationKeyState::KeyringUnavailable => (
            "The operating system's credential store is unavailable: new attestations stay unsigned",
            theme::warning(),
        ),
        AttestationKeyState::KeyMissing => (
            "The active key is missing from the credential store: new attestations stay unsigned until you create a new key",
            theme::danger(),
        ),
        AttestationKeyState::Mismatch => (
            "The credential store holds a different key: new attestations stay unsigned until you create a new key",
            theme::danger(),
        ),
    }
}

fn file_check_lines(result: &Result<AttestationFileCheck, String>) -> AnyElement {
    let check = match result {
        Ok(check) => check,
        Err(error) => {
            return div()
                .text_sm()
                .text_color(theme::danger())
                .child(error.clone())
                .into_any_element();
        }
    };
    let (signature, color) = signature_text(check.signature);
    let key = match check.key {
        FileKey::KnownOnThisMachine => "key known on this machine",
        FileKey::UnknownKey => "unknown key: checked with the public key file chosen with it",
        FileKey::Unavailable => "no key to check it with: choose the .pub.pem saved with it",
    };
    div()
        .debug_selector(|| "attestation-file-check".into())
        .flex()
        .flex_col()
        .gap_1()
        .child(div().text_sm().text_color(color).child(signature))
        .child(hint(key))
        .when_some(check.pulsation_id.clone(), |list, id| {
            list.child(readout(format!(
                "Pulsation {id} · {} file(s)",
                check.subject_count
            )))
        })
        .when_some(check.key_id.as_deref(), |list, key_id| {
            list.child(readout(format!("Key {}", short_key(key_id))))
        })
        .when_some(check.statement_sha256.clone(), |list, digest| {
            list.child(readout(format!("sha256:{digest}")))
        })
        .into_any_element()
}

/// "Verify a file…" needs no key of its own, so it is there whatever the
/// key's state.
fn verify_file_section(
    busy: bool,
    file_check: Option<Result<AttestationFileCheck, String>>,
    hl: Option<&str>,
    cx: &mut Context<AppModel>,
) -> AnyElement {
    let body = div()
        .flex()
        .flex_col()
        .gap_2()
        .child(note("Choose a saved .intoto.jsonl bundle, and the .pub.pem saved with it when it comes from another machine."))
        .child(
            div().px_3().child(btn(
                "attestation-verify-file",
                "Verify a file…",
                ButtonVariant::Secondary,
                !busy,
                cx,
                |model, cx| model.verify_attestation_file(cx),
            )),
        )
        .when_some(file_check, |body, result| {
            body.child(div().px_3().child(file_check_lines(&result)))
        });
    section(
        "Verify a file",
        None,
        vec![stacked_row("attestation-verify-file", hl, body)],
    )
}

pub(crate) fn security_page(
    model: &AppModel,
    hl: Option<&str>,
    cx: &mut Context<AppModel>,
) -> PageContent {
    let settings = model.settings.read(cx);
    let busy = settings.busy;
    let confirming = settings.attestation_rotate_confirm;
    let file_check = settings.attestation_file_check.clone();
    let Some(status) = settings
        .snapshot
        .as_ref()
        .and_then(|snapshot| snapshot.attestation.clone())
    else {
        return PageContent::new(SettingsPage::Security.label())
            .description(SettingsPage::Security.description())
            .section(
                "Attestation key",
                section(
                    "Attestation key",
                    None,
                    vec![note("The key status could not be read.")],
                ),
            )
            .section(
                "Verify a file",
                verify_file_section(busy, file_check, hl, cx),
            );
    };
    let (state_text, state_color) = key_state_text(status.state);
    let has_key = !status.keys.is_empty();
    let key_body = div()
        .flex()
        .flex_col()
        .gap_2()
        .child(note("Each pulsation that ends gets an attestation signed with this machine's Ed25519 key. The private key stays in the operating system's credential store; only public keys are kept in the app's database, so a retired key still verifies what it signed."))
        .child(
            div()
                .debug_selector(|| "attestation-key-state".into())
                .px_3()
                .text_sm()
                .text_color(state_color)
                .child(state_text),
        )
        .children(status.keys.iter().enumerate().map(|(index, key)| {
            div()
                .debug_selector(move || format!("attestation-key-{index}"))
                .px_3()
                .child(readout(match &key.retired_at {
                    Some(retired) => format!(
                        "{} · retired {} · created {}",
                        short_key(&key.key_id),
                        crate::time::format_timestamp(retired),
                        crate::time::format_timestamp(&key.created_at)
                    ),
                    None => format!(
                        "{} · active · created {}",
                        short_key(&key.key_id),
                        crate::time::format_timestamp(&key.created_at)
                    ),
                }))
        }))
        .child(
            div()
                .px_3()
                .flex()
                .flex_wrap()
                .items_center()
                .gap_2()
                .child(btn_confirm(
                    "attestation-create-key",
                    if has_key { "Create a new key" } else { "Create key" },
                    "Confirm: create and use a new key",
                    confirming,
                    !busy,
                    if confirming {
                        ButtonVariant::Danger
                    } else {
                        ButtonVariant::Secondary
                    },
                    cx,
                    |model, cx| {
                        model.settings.update(cx, |settings, _| {
                            settings.attestation_rotate_confirm = true;
                        });
                        cx.notify();
                    },
                    |model, cx| model.create_attestation_key(cx),
                ))
                .when(confirming, |row| {
                    row.child(btn(
                        "attestation-create-key-cancel",
                        "Cancel",
                        ButtonVariant::Secondary,
                        !busy,
                        cx,
                        |model, cx| {
                            model.settings.update(cx, |settings, _| {
                                settings.attestation_rotate_confirm = false;
                            });
                            cx.notify();
                        },
                    ))
                }),
        )
        .when(confirming && has_key, |body| {
            body.child(hint("The current key is retired and keeps verifying what it signed; pulsations that end from now on are signed with the new one."))
        });
    PageContent::new(SettingsPage::Security.label())
        .description(SettingsPage::Security.description())
        .section(
            "Attestation key",
            section(
                "Attestation key",
                None,
                vec![
                    stacked_row("attestation-key", hl, key_body),
                    row(
                        "attestation-sealing",
                        hl,
                        switch(
                            "attestation-sealing",
                            status.sealing_enabled,
                            !busy,
                            cx,
                            move |model, cx| {
                                model.set_attestation_sealing(!status.sealing_enabled, cx)
                            },
                        ),
                    ),
                ],
            ),
        )
        .section(
            "Verify a file",
            verify_file_section(busy, file_check, hl, cx),
        )
}

impl AppModel {
    /// One Attestation card action on the active pulsation; the detail is
    /// read again afterwards, so the card shows the host's new state.
    pub(crate) fn run_attestation_action(
        &mut self,
        action: AttestationAction,
        cx: &mut Context<Self>,
    ) {
        let Some(backend) = self.pulsations_backend.clone() else {
            return;
        };
        let pulsation_id = self.pulsations.update(cx, |state, _| {
            if state.attestation.busy {
                return None;
            }
            let id = state.active()?.id.clone();
            state.attestation.busy = true;
            state.attestation.notice = None;
            Some(id)
        });
        let Some(pulsation_id) = pulsation_id else {
            return;
        };
        cx.notify();
        let kind = action.clone();
        self.attestation_task = Some(cx.spawn(async move |this, cx| {
            let result = backend.attestation(pulsation_id.clone(), action).await;
            let _ = this.update(cx, |model, cx| {
                let notice = match (result, kind) {
                    (Ok(paths), AttestationAction::Save(_)) => AttestationNotice::Saved(paths),
                    (Ok(_), AttestationAction::VerifyAgain) => AttestationNotice::Verified,
                    (Ok(_), AttestationAction::SignNow) => AttestationNotice::Signed,
                    (Ok(_), AttestationAction::SealAgain) => AttestationNotice::Requeued,
                    (Err(error), _) => AttestationNotice::Failed(error),
                };
                model.pulsations.update(cx, |state, _| {
                    // Leaving the pulsation already reset its card.
                    if state.active_id().as_deref() == Some(pulsation_id.as_str()) {
                        state.attestation.busy = false;
                        state.attestation.notice = Some(notice);
                    }
                });
                model.refresh_pulsations(cx);
                cx.notify();
            });
        }));
    }

    /// Asks where to keep the bundle; the public key is written next to it.
    pub(crate) fn save_attestation(&mut self, cx: &mut Context<Self>) {
        let state = self.pulsations.read(cx);
        if state.attestation.busy {
            return;
        }
        let Some(pulsation_id) = state.active().map(|pulsation| pulsation.id.clone()) else {
            return;
        };
        let suggested = format!("{pulsation_id}.intoto.jsonl");
        let chosen =
            cx.prompt_for_new_path(&crate::app_model::report_save_directory(), Some(&suggested));
        self.attestation_task = Some(cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(target))) = chosen.await else {
                return;
            };
            let _ = this.update(cx, |model, cx| {
                // The developer may have moved to another pulsation meanwhile.
                if model.pulsations.read(cx).active_id().as_deref() == Some(pulsation_id.as_str()) {
                    model.run_attestation_action(AttestationAction::Save(target), cx);
                }
            });
        }));
    }

    pub(crate) fn copy_attestation_trailer(&mut self, cx: &mut Context<Self>) {
        let state = self.pulsations.read(cx);
        let Some((pulsation_id, digest)) = state.detail().and_then(|detail| {
            let status = detail.attestation.as_ref()?;
            Some((
                detail.pulsation_id.clone(),
                status.statement_sha256.clone()?,
            ))
        }) else {
            return;
        };
        cx.write_to_clipboard(ClipboardItem::new_string(commit_trailer(
            &pulsation_id,
            &digest,
        )));
        self.pulsations.update(cx, |state, _| {
            state.attestation.notice = Some(AttestationNotice::TrailerCopied);
        });
        cx.notify();
    }

    pub(crate) fn create_attestation_key(&mut self, cx: &mut Context<Self>) {
        self.settings.update(cx, |settings, _| {
            settings.attestation_rotate_confirm = false;
        });
        let Some(backend) = self.settings_backend.clone() else {
            return;
        };
        self.run_settings_operation(
            Box::pin(async move {
                backend.create_attestation_key().await?;
                Ok("A new attestation key is active; earlier attestations keep verifying with the retired one.".into())
            }),
            cx,
        );
    }

    pub(crate) fn set_attestation_sealing(&mut self, enabled: bool, cx: &mut Context<Self>) {
        let Some(backend) = self.settings_backend.clone() else {
            return;
        };
        self.run_settings_operation(
            Box::pin(async move {
                backend.set_attestation_sealing(enabled).await?;
                Ok(if enabled {
                    "Pulsations that end from now on are sealed."
                } else {
                    "Pulsations that end from now on are not sealed."
                }
                .into())
            }),
            cx,
        );
    }

    /// Reads the chosen bundle (and public key, when one is chosen with it)
    /// and asks the host to check it.
    pub(crate) fn verify_attestation_file(&mut self, cx: &mut Context<Self>) {
        let Some(backend) = self.settings_backend.clone() else {
            return;
        };
        let chosen = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some("Verify".into()),
        });
        self.attestation_file_task = Some(cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = chosen.await else {
                return;
            };
            let (keys, bundles): (Vec<PathBuf>, Vec<PathBuf>) =
                paths.into_iter().partition(|path| {
                    path.extension()
                        .is_some_and(|extension| extension.eq_ignore_ascii_case("pem"))
                });
            let result = match bundles.first() {
                None => Err("Choose an .intoto.jsonl bundle to verify.".to_owned()),
                Some(bundle) => match std::fs::read_to_string(bundle) {
                    Err(error) => Err(format!("The bundle could not be read: {error}")),
                    Ok(text) => {
                        let pem = keys
                            .first()
                            .and_then(|key| std::fs::read_to_string(key).ok());
                        backend
                            .verify_attestation_file(text, pem)
                            .await
                            .map_err(|error| error.presentable())
                    }
                },
            };
            let _ = this.update(cx, |model, cx| {
                model.settings.update(cx, |settings, _| {
                    settings.attestation_file_check = Some(result);
                });
                cx.notify();
            });
        }));
    }
}
