import { AppHeader } from "./components/layout/AppHeader";
import { AppSidebar } from "./components/layout/AppSidebar";
import { StatusBar } from "./components/layout/StatusBar";
import { useViewStore } from "./state/viewStore";
import { LogsView } from "./views/LogsView";
import { ChatView } from "./views/ChatView";
import { PulsationsView } from "./views/PulsationsView";
import { SettingsView } from "./views/SettingsView";
import { VaultView } from "./views/VaultView";
import { WorkspaceView } from "./views/WorkspaceView";

const VIEWS = {
  workspace: WorkspaceView,
  pulsations: PulsationsView,
  vault: VaultView,
  chat: ChatView,
  settings: SettingsView,
  logs: LogsView
} as const;

export function App() {
  const activeView = useViewStore((state) => state.activeView);
  const ActiveView = VIEWS[activeView];
  return (
    <div className="app-shell">
      <AppSidebar />
      <div className="app-frame">
        <AppHeader />
        <main className="view-host"><ActiveView /></main>
      </div>
      <StatusBar />
    </div>
  );
}
