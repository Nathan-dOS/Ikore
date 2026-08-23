import { usePulsationStore } from "../../state/pulsationStore";
import { APP_VIEWS, useViewStore } from "../../state/viewStore";

export function AppSidebar() {
  const activeView = useViewStore((state) => state.activeView);
  const collapsed = useViewStore((state) => state.sidebarCollapsed);
  const setActiveView = useViewStore((state) => state.setActiveView);
  const toggleSidebar = useViewStore((state) => state.toggleSidebar);
  const hitlCount = usePulsationStore((state) => state.hitlWaitingCount);
  const backgroundCount = usePulsationStore((state) => state.backgroundAgentsCount);

  return (
    <aside className={`app-sidebar ${collapsed ? "is-collapsed" : ""}`} aria-label="Application navigation">
      <div className="brand-lockup">
        <span className="brand-mark" aria-hidden="true">HV</span>
        {!collapsed ? <span className="brand-name">Hard Vibe Code.</span> : null}
      </div>
      <nav className="app-nav">
        {APP_VIEWS.map((view) => {
          const badge = view.id === "workspace" ? hitlCount : view.id === "pulsations" ? backgroundCount : 0;
          const checkpoint = view.id === "workspace" && hitlCount > 0;
          return (
            <button
              key={view.id}
              type="button"
              className={`app-nav-item ${checkpoint ? "has-checkpoint" : ""}`}
              aria-current={activeView === view.id ? "page" : undefined}
              aria-label={view.label}
              title={collapsed ? view.label : undefined}
              onClick={() => setActiveView(view.id)}
            >
              <span className="nav-glyph" aria-hidden="true">{view.glyph}</span>
              {!collapsed ? <span className="nav-label">{view.shortLabel}</span> : null}
              {badge > 0 ? <span className="nav-badge" aria-label={`${badge} pending`}>{badge}</span> : null}
            </button>
          );
        })}
      </nav>
      <button
        type="button"
        className="sidebar-toggle"
        onClick={toggleSidebar}
        aria-label={collapsed ? "Expand sidebar" : "Collapse sidebar"}
      >
        <span aria-hidden="true">{collapsed ? "»" : "«"}</span>
        {!collapsed ? "Collapse" : null}
      </button>
    </aside>
  );
}
