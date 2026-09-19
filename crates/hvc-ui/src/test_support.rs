use gpui::{AppContext as _, Entity, TestAppContext};
use hvc_test_support::CoreTestHarness;

use crate::AppModel;

pub(crate) struct GpuiTestHarness {
    core: CoreTestHarness,
}

impl GpuiTestHarness {
    pub(crate) fn new() -> Self {
        Self {
            core: CoreTestHarness::new(),
        }
    }

    pub(crate) fn mount(&self, cx: &mut TestAppContext) -> Entity<AppModel> {
        let core = self.core.core();
        cx.new(|cx| AppModel::new(core, cx))
    }

    pub(crate) fn data_dir(&self) -> &std::path::Path {
        self.core.data_dir()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[gpui::test]
    fn mounts_app_model_with_an_isolated_core(cx: &mut TestAppContext) {
        let harness = GpuiTestHarness::new();
        let model = harness.mount(cx);
        cx.run_until_parked();

        assert!(harness.data_dir().join("hard-vibe-code.db").is_file());
        cx.read(|cx| {
            assert_eq!(model.read(cx).active_view(), crate::AppView::Home);
        });
    }
}
