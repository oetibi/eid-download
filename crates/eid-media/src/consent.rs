//! Consent before any video tool is downloaded or run (ADR-0007, D2).
//!
//! The text comes from the translation files (`locales/<lang>/consent.txt`) with the tool list
//! of the pinned manifest filled in, so it always names the exact versions and sources that
//! will be installed. The record stores the SHA-256 of the text the user saw; a different
//! text (new tool versions, reworded text) needs a new consent. Only this module can grant,
//! and only from a [`ConsentText`] it rendered itself: the IPC and Native Messaging layers
//! have no way to reach it (D7; `ipc_and_native_messaging_cannot_grant_consent`).

use crate::manifest::Manifest;
use crate::settings::{ConsentRecord, SettingsStore};
use crate::tools::ToolStore;
use crate::MediaError;
use sha2::{Digest, Sha256};

/// Version of the consent wording; bump it when the meaning of the text changes.
pub const CONSENT_VERSION: u32 = 1;

const TEXT_AR: &str = include_str!("../locales/ar/consent.txt");
const TEXT_EN: &str = include_str!("../locales/en/consent.txt");

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lang {
    Ar,
    En,
}

/// A rendered consent text. Constructed only by [`consent_text`].
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct ConsentText {
    pub lang: Lang,
    pub text: String,
    pub sha256: String,
}

fn hex(d: &[u8]) -> String {
    d.iter().map(|b| format!("{b:02x}")).collect()
}

/// The raw translation file for `lang` (before the tool list is filled in).
pub fn template(lang: Lang) -> &'static str {
    match lang {
        Lang::Ar => TEXT_AR,
        Lang::En => TEXT_EN,
    }
}

/// Render the consent text for `lang` with the manifest's tools.
pub fn consent_text(lang: Lang, m: &Manifest) -> ConsentText {
    let tools: Vec<String> = m
        .tools
        .iter()
        .map(|t| format!("- {} {} — {} ({})", t.name, t.version, t.source, t.license))
        .collect();
    let text = template(lang).replace("{tools}", &tools.join("\n"));
    let sha256 = hex(&Sha256::digest(text.as_bytes()));
    ConsentText {
        lang,
        text,
        sha256,
    }
}

/// Whether the stored consent covers the current text (in either language).
pub fn is_granted(store: &SettingsStore, m: &Manifest) -> Result<bool, MediaError> {
    let s = store.load()?;
    Ok(match s.media_consent {
        Some(r) if r.version == CONSENT_VERSION => [Lang::Ar, Lang::En]
            .iter()
            .any(|l| consent_text(*l, m).sha256 == r.text_sha256),
        _ => false,
    })
}

/// Fails with [`MediaError::NoConsent`] unless consent covers the current text.
pub fn require(store: &SettingsStore, m: &Manifest) -> Result<(), MediaError> {
    if is_granted(store, m)? {
        Ok(())
    } else {
        Err(MediaError::NoConsent)
    }
}

/// Record that the user agreed to `text` at `ts` (Unix seconds).
pub fn grant(store: &SettingsStore, text: &ConsentText, ts: u64) -> Result<(), MediaError> {
    let mut s = store.load()?;
    s.media_consent = Some(ConsentRecord {
        version: CONSENT_VERSION,
        text_sha256: text.sha256.clone(),
        ts,
    });
    store.save(&s)
}

/// Withdraw consent and delete the tools. Consent is cleared first, so video is off even if
/// deleting the tools fails (for example because a job still holds them).
pub fn revoke(store: &SettingsStore, tools: &ToolStore) -> Result<(), MediaError> {
    let mut s = store.load().unwrap_or_default();
    s.media_consent = None;
    store.save(&s)?;
    tools.remove_all()
}
