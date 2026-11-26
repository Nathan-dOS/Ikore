//! Embedded static assets for the desktop shell.
//!
//! GPUI resolves `svg().path("icons/house.svg")` through the `AssetSource` registered on the
//! `Application` (`ikore-app/src/main.rs`). The bytes live next to the widgets that name them so a
//! test in this crate can prove every `Icon` variant maps to a real file; a typo in a path is
//! otherwise an invisible icon at runtime, not an error.
//!
//! Icons use Lucide (ISC licence, see `assets/icons/LICENSE-lucide.txt`) and original
//! `canvas-*` glyphs with the same 24x24, stroke-only vocabulary:
//! `currentColor`. The renderer keeps only the alpha mask and tints it with the element's
//! `text_color`, so the stroke colour inside the file never matters.

use std::borrow::Cow;

use gpui::{AssetSource, SharedString};

pub struct IkoreAssets;

pub type HvcAssets = IkoreAssets;

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
    icon_asset!("archive"),
    icon_asset!("arrow-right"),
    icon_asset!("ban"),
    icon_asset!("bell"),
    icon_asset!("bot"),
    icon_asset!("canvas-cursor"),
    icon_asset!("canvas-hand"),
    icon_asset!("canvas-diamond"),
    icon_asset!("canvas-ellipse"),
    icon_asset!("canvas-line"),
    icon_asset!("canvas-type"),
    icon_asset!("canvas-eraser"),
    icon_asset!("canvas-lock"),
    icon_asset!("canvas-lock-open"),
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
    icon_asset!("flag"),
    icon_asset!("folder-open"),
    icon_asset!("git-branch"),
    icon_asset!("git-compare"),
    icon_asset!("hammer"),
    icon_asset!("heart-pulse"),
    icon_asset!("house"),
    icon_asset!("info"),
    icon_asset!("keyboard"),
    icon_asset!("layers"),
    icon_asset!("list-todo"),
    icon_asset!("maximize"),
    icon_asset!("message-square"),
    icon_asset!("minus"),
    icon_asset!("mic"),
    icon_asset!("network"),
    icon_asset!("notebook-pen"),
    icon_asset!("panel-right-close"),
    icon_asset!("panel-right-open"),
    icon_asset!("pause"),
    icon_asset!("pencil"),
    icon_asset!("pin"),
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

macro_rules! anim_frame {
    ($clip:literal, $frame:literal) => {
        (
            concat!("anim/", $clip, "/f", $frame, ".svg"),
            include_bytes!(concat!(
                "../assets/icons/anim/",
                $clip,
                "/f",
                $frame,
                ".svg"
            )) as &[u8],
        )
    };
}

macro_rules! anim_clip_16 {
    ($clip:literal) => {
        (
            $clip,
            &[
                anim_frame!($clip, "00"),
                anim_frame!($clip, "01"),
                anim_frame!($clip, "02"),
                anim_frame!($clip, "03"),
                anim_frame!($clip, "04"),
                anim_frame!($clip, "05"),
                anim_frame!($clip, "06"),
                anim_frame!($clip, "07"),
                anim_frame!($clip, "08"),
                anim_frame!($clip, "09"),
                anim_frame!($clip, "10"),
                anim_frame!($clip, "11"),
                anim_frame!($clip, "12"),
                anim_frame!($clip, "13"),
                anim_frame!($clip, "14"),
                anim_frame!($clip, "15"),
            ],
        )
    };
}

macro_rules! anim_clip_14 {
    ($clip:literal) => {
        (
            $clip,
            &[
                anim_frame!($clip, "00"),
                anim_frame!($clip, "01"),
                anim_frame!($clip, "02"),
                anim_frame!($clip, "03"),
                anim_frame!($clip, "04"),
                anim_frame!($clip, "05"),
                anim_frame!($clip, "06"),
                anim_frame!($clip, "07"),
                anim_frame!($clip, "08"),
                anim_frame!($clip, "09"),
                anim_frame!($clip, "10"),
                anim_frame!($clip, "11"),
                anim_frame!($clip, "12"),
                anim_frame!($clip, "13"),
            ],
        )
    };
}

pub(crate) type AnimAssetEntry = (&'static str, &'static [u8]);
pub(crate) type AnimClipGroup = (&'static str, &'static [AnimAssetEntry]);

/// Animated SVG frame sequences baked from Lordicon Lottie JSONs (`anim/<clip>/f00.svg`).
pub(crate) static ANIM_GROUPS: &[AnimClipGroup] = &[
    anim_clip_16!("ikore-brand"),
    anim_clip_16!("status-saved"),
    anim_clip_16!("status-loading"),
    anim_clip_16!("status-idle"),
    anim_clip_14!("status-attention"),
    anim_clip_16!("status-error"),
    anim_clip_16!("info-tip"),
    anim_clip_16!("nav-home"),
    anim_clip_16!("nav-new-work"),
    anim_clip_16!("nav-projects"),
    anim_clip_16!("nav-pulsations"),
    anim_clip_16!("nav-logs"),
    anim_clip_16!("nav-chat"),
    anim_clip_16!("nav-notes"),
    anim_clip_16!("nav-canvas"),
    anim_clip_16!("nav-workspace"),
    anim_clip_16!("header-health"),
    anim_clip_16!("agents-ghost"),
    anim_clip_16!("agents-terminal"),
    anim_clip_16!("agents-api"),
    anim_clip_16!("agents-wand"),
    anim_clip_16!("agents-oracle"),
];

fn load_anim_asset(path: &str) -> Option<&'static [u8]> {
    let rest = path.strip_prefix("anim/")?;
    let (clip, file) = rest.split_once('/')?;
    let num = file.strip_prefix('f')?.strip_suffix(".svg")?;
    let index: usize = num.parse().ok()?;
    let (_, frames) = ANIM_GROUPS.iter().find(|(name, _)| *name == clip)?;
    frames.get(index).map(|(_, bytes)| *bytes)
}

impl AssetSource for IkoreAssets {
    fn load(&self, path: &str) -> anyhow::Result<Option<Cow<'static, [u8]>>> {
        if let Some((_, bytes)) = ASSETS.iter().find(|(asset_path, _)| *asset_path == path) {
            return Ok(Some(Cow::Borrowed(*bytes)));
        }
        Ok(load_anim_asset(path).map(Cow::Borrowed))
    }

    fn list(&self, path: &str) -> anyhow::Result<Vec<SharedString>> {
        let prefix = path.trim_end_matches('/');
        Ok(ASSETS
            .iter()
            .chain(ANIM_GROUPS.iter().flat_map(|(_, frames)| frames.iter()))
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
            let bytes = IkoreAssets
                .load(icon.path())
                .unwrap()
                .unwrap_or_else(|| panic!("{:?} has no embedded asset at {}", icon, icon.path()));
            let text = std::str::from_utf8(&bytes).unwrap();
            assert!(text.contains("<svg"), "{} is not an svg", icon.path());
            assert!(
                text.contains("viewBox=\"0 0 24 24\""),
                "{} is not a 24x24 vector icon",
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

        let listed = IkoreAssets.list("icons/").unwrap();
        assert_eq!(listed.len(), total);
        assert_eq!(IkoreAssets.list("icons").unwrap().len(), total);
        assert!(IkoreAssets.list("fonts").unwrap().is_empty());
        assert!(IkoreAssets.load("icons/missing.svg").unwrap().is_none());
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

    #[test]
    fn anim_asset_table_has_no_duplicates() {
        let mut paths: Vec<&str> = ANIM_GROUPS
            .iter()
            .flat_map(|(_, frames)| frames.iter().map(|(path, _)| *path))
            .collect();
        let total = paths.len();
        assert_eq!(total, 350);
        paths.sort_unstable();
        paths.dedup();
        assert_eq!(paths.len(), total, "duplicate path in ANIM_GROUPS");
        assert_eq!(IkoreAssets.list("anim/").unwrap().len(), total);
    }

    #[gpui::test]
    fn every_anim_clip_frame_resolves_and_rasterizes_with_gpui_svg_renderer(
        cx: &mut gpui::TestAppContext,
    ) {
        use crate::animated_icon::AnimClip;

        let renderer = cx.update(|app| app.svg_renderer());
        for clip in AnimClip::ALL {
            for frame in 0..clip.frame_count() {
                let path = clip.frame_path(frame);
                let bytes = IkoreAssets
                    .load(path.as_ref())
                    .unwrap()
                    .unwrap_or_else(|| panic!("missing animated frame {path}"));
                let image = gpui::Image::from_bytes(gpui::ImageFormat::Svg, bytes.into_owned());
                let rendered = image
                    .to_image_data(renderer.clone())
                    .unwrap_or_else(|err| panic!("failed to rasterize {path}: {err}"));
                let pixels = rendered.as_bytes(0).expect("svg frame 0 bytes");
                assert!(!pixels.is_empty(), "{path} produced empty buffer");
                if frame == clip.rest_frame() {
                    let has_visible_alpha = pixels.as_chunks::<4>().0.iter().any(|px| px[3] > 0);
                    assert!(
                        has_visible_alpha,
                        "resting frame {path} for {:?} rendered completely transparent",
                        clip
                    );
                }
            }
        }
    }
}
