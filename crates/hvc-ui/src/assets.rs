//! Embedded static assets for the desktop shell.
//!
//! GPUI resolves `svg().path("icons/house.svg")` through the `AssetSource` registered on the
//! `Application` (`hvc-app/src/main.rs`). The bytes live next to the widgets that name them so a
//! test in this crate can prove every `Icon` variant maps to a real file; a typo in a path is
//! otherwise an invisible icon at runtime, not an error.
//!
//! Icons are Lucide (ISC licence, see `assets/icons/LICENSE-lucide.txt`): 24x24, stroke-only,
//! `currentColor`. The renderer keeps only the alpha mask and tints it with the element's
//! `text_color`, so the stroke colour inside the file never matters.

use std::borrow::Cow;

use gpui::{AssetSource, SharedString};

pub struct HvcAssets;

macro_rules! icon_asset {
    ($name:literal) => {
        (
            concat!("icons/", $name, ".svg"),
            include_bytes!(concat!("../assets/icons/", $name, ".svg")) as &[u8],
        )
    };
}

/// Every embedded asset, keyed by the path the UI requests it with.
pub(crate) static ASSETS: &[(&str, &[u8])] = &[
    icon_asset!("activity"),
    icon_asset!("ban"),
    icon_asset!("bell"),
    icon_asset!("bot"),
    icon_asset!("brain"),
    icon_asset!("chart-column"),
    icon_asset!("check"),
    icon_asset!("circle-help"),
    icon_asset!("chevron-down"),
    icon_asset!("chevron-left"),
    icon_asset!("chevron-right"),
    icon_asset!("chevron-up"),
    icon_asset!("clock"),
    icon_asset!("copy"),
    icon_asset!("cpu"),
    icon_asset!("database"),
    icon_asset!("download"),
    icon_asset!("eye"),
    icon_asset!("file-text"),
    icon_asset!("flask-conical"),
    icon_asset!("folder"),
    icon_asset!("folder-open"),
    icon_asset!("hammer"),
    icon_asset!("heart-pulse"),
    icon_asset!("house"),
    icon_asset!("info"),
    icon_asset!("keyboard"),
    icon_asset!("layers"),
    icon_asset!("list-todo"),
    icon_asset!("maximize"),
    icon_asset!("message-square"),
    icon_asset!("network"),
    icon_asset!("minus"),
    icon_asset!("panel-right-close"),
    icon_asset!("panel-right-open"),
    icon_asset!("pause"),
    icon_asset!("play"),
    icon_asset!("plug"),
    icon_asset!("plus"),
    icon_asset!("rotate-cw"),
    icon_asset!("route"),
    icon_asset!("save"),
    icon_asset!("scroll-text"),
    icon_asset!("search"),
    icon_asset!("send"),
    icon_asset!("settings"),
    icon_asset!("shield-check"),
    icon_asset!("signpost"),
    icon_asset!("sparkles"),
    icon_asset!("target"),
    icon_asset!("sliders-horizontal"),
    icon_asset!("square"),
    icon_asset!("timer"),
    icon_asset!("trash-2"),
    icon_asset!("triangle-alert"),
    icon_asset!("vault"),
    icon_asset!("waves"),
    icon_asset!("workflow"),
    icon_asset!("x"),
    icon_asset!("zap"),
];

impl AssetSource for HvcAssets {
    fn load(&self, path: &str) -> anyhow::Result<Option<Cow<'static, [u8]>>> {
        Ok(ASSETS
            .iter()
            .find(|(asset_path, _)| *asset_path == path)
            .map(|(_, bytes)| Cow::Borrowed(*bytes)))
    }

    fn list(&self, path: &str) -> anyhow::Result<Vec<SharedString>> {
        let prefix = path.trim_end_matches('/');
        Ok(ASSETS
            .iter()
            .filter(|(asset_path, _)| {
                prefix.is_empty()
                    || asset_path
                        .strip_prefix(prefix)
                        .is_some_and(|rest| rest.starts_with('/'))
            })
            .map(|(asset_path, _)| SharedString::from(*asset_path))
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::icons::Icon;

    #[test]
    fn every_icon_variant_resolves_to_an_embedded_svg() {
        for icon in Icon::ALL {
            let bytes = HvcAssets
                .load(icon.path())
                .unwrap()
                .unwrap_or_else(|| panic!("{:?} has no embedded asset at {}", icon, icon.path()));
            let text = std::str::from_utf8(&bytes).unwrap();
            assert!(text.contains("<svg"), "{} is not an svg", icon.path());
            assert!(
                text.contains("viewBox=\"0 0 24 24\""),
                "{} is not a 24x24 Lucide icon",
                icon.path()
            );
        }
    }

    #[test]
    fn asset_table_has_no_duplicates_and_lists_by_prefix() {
        let mut paths: Vec<&str> = ASSETS.iter().map(|(path, _)| *path).collect();
        let total = paths.len();
        paths.sort_unstable();
        paths.dedup();
        assert_eq!(paths.len(), total, "duplicate asset path in ASSETS");

        let listed = HvcAssets.list("icons/").unwrap();
        assert_eq!(listed.len(), total);
        assert_eq!(HvcAssets.list("icons").unwrap().len(), total);
        assert!(HvcAssets.list("fonts").unwrap().is_empty());
        assert!(HvcAssets.load("icons/missing.svg").unwrap().is_none());
    }

    #[test]
    fn every_embedded_asset_is_named_by_an_icon() {
        for (path, _) in ASSETS {
            assert!(
                Icon::ALL.iter().any(|icon| icon.path() == *path),
                "{path} is embedded but no Icon variant uses it"
            );
        }
    }
}
