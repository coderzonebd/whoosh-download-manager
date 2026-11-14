//! The extension's IDs in each store, from `packaging/browsers/extension-ids.json`
//! (compiled in, so the host and the installers agree on one list).
//!
//! A store ID is `null` until that store has accepted the extension. Until
//! then the host simply does not allow it, and nothing offers the extension
//! from that store.

use std::sync::OnceLock;

use serde::Deserialize;

use crate::register::{is_chromium_id, is_firefox_id};

const CONFIG: &str = include_str!("../../../packaging/browsers/extension-ids.json");

/// Microsoft Edge Add-ons, for `update_url` in external-extension entries.
pub const EDGE_UPDATE_URL: &str = "https://edge.microsoft.com/extensionwebstorebase/v1/crx";
/// The Chrome Web Store, for `update_url` in external-extension entries.
pub const CHROME_WEB_STORE_UPDATE_URL: &str = "https://clients2.google.com/service/update2/crx";

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtensionIds {
    pub firefox: Firefox,
    pub edge: Edge,
    pub chrome_web_store: ChromeWebStore,
    pub github: Github,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Firefox {
    /// `browser_specific_settings.gecko.id`; fixed by us, not by AMO.
    pub id: String,
    /// The listing's slug on addons.mozilla.org, once listed.
    pub amo_slug: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Edge {
    /// The ID Microsoft Edge Add-ons assigned.
    pub id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChromeWebStore {
    pub id: Option<String>,
    /// Offer the extension to Chrome, Brave and Vivaldi from the installers
    /// (external extensions). Only with an `id`.
    pub external_install: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Github {
    /// The ID of the Chromium build from GitHub (and of development builds):
    /// fixed by the public `key` in the extension's manifest.
    pub chromium_id: String,
}

impl ExtensionIds {
    /// Parses and checks a config file's text.
    pub fn parse(text: &str) -> Result<Self, String> {
        let ids: ExtensionIds = serde_json::from_str(text).map_err(|e| e.to_string())?;
        for id in [&ids.edge.id, &ids.chrome_web_store.id]
            .into_iter()
            .flatten()
            .chain([&ids.github.chromium_id])
        {
            if !is_chromium_id(id) {
                return Err(format!("{id:?} is not a Chromium extension ID"));
            }
        }
        if !is_firefox_id(&ids.firefox.id) {
            return Err(format!(
                "{:?} is not a Firefox extension ID",
                ids.firefox.id
            ));
        }
        if ids.chrome_web_store.external_install && ids.chrome_web_store.id.is_none() {
            return Err("chromeWebStore.externalInstall needs chromeWebStore.id".to_owned());
        }
        Ok(ids)
    }

    /// Store IDs Chromium-based browsers may connect with (not the GitHub one).
    pub fn store_chromium_ids(&self) -> Vec<String> {
        [&self.edge.id, &self.chrome_web_store.id]
            .into_iter()
            .flatten()
            .cloned()
            .collect()
    }

    /// The Chrome Web Store ID when installers should offer it.
    pub fn chrome_web_store_offer(&self) -> Option<&str> {
        self.chrome_web_store
            .id
            .as_deref()
            .filter(|_| self.chrome_web_store.external_install)
    }
}

/// The compiled-in config.
pub fn ids() -> &'static ExtensionIds {
    static IDS: OnceLock<ExtensionIds> = OnceLock::new();
    // Checked by a unit test, so a bad file never reaches a build that ships.
    IDS.get_or_init(|| ExtensionIds::parse(CONFIG).expect("extension-ids.json is valid"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DEV_CHROMIUM_EXTENSION_ID, FIREFOX_EXTENSION_ID};

    #[test]
    fn the_shipped_config_is_valid_and_agrees_with_the_constants() {
        let ids = ExtensionIds::parse(CONFIG).unwrap();
        assert_eq!(ids.github.chromium_id, DEV_CHROMIUM_EXTENSION_ID);
        assert_eq!(ids.firefox.id, FIREFOX_EXTENSION_ID);
    }

    fn sample(edge: &str, cws: &str, external: bool) -> String {
        format!(
            r#"{{"firefox":{{"id":"a@b","amoSlug":null}},"edge":{{"id":{edge}}},
               "chromeWebStore":{{"id":{cws},"externalInstall":{external}}},
               "github":{{"chromiumId":"{DEV_CHROMIUM_EXTENSION_ID}"}}}}"#
        )
    }

    #[test]
    fn store_ids_and_the_switch() {
        let edge = "\"aaaabbbbccccddddeeeeffffgggghhhh\"";
        let cws = "\"ppppoooonnnnmmmmllllkkkkjjjjiiii\"";
        let ids = ExtensionIds::parse(&sample(edge, cws, false)).unwrap();
        assert_eq!(ids.store_chromium_ids().len(), 2);
        assert_eq!(ids.chrome_web_store_offer(), None);
        let ids = ExtensionIds::parse(&sample(edge, cws, true)).unwrap();
        assert_eq!(
            ids.chrome_web_store_offer(),
            Some("ppppoooonnnnmmmmllllkkkkjjjjiiii")
        );
        let none = ExtensionIds::parse(&sample("null", "null", false)).unwrap();
        assert!(none.store_chromium_ids().is_empty());
    }

    #[test]
    fn rejects_bad_ids_and_a_switch_without_an_id() {
        assert!(ExtensionIds::parse(&sample("\"not-an-id\"", "null", false)).is_err());
        assert!(ExtensionIds::parse(&sample("null", "null", true)).is_err());
    }
}
