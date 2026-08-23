import { openWorkspaceFromDialog } from "../../app/applicationController";
import { usePulsationStore } from "../../state/pulsationStore";
import { useRuntimeStore } from "../../state/runtimeStore";
import { useVaultStore } from "../../state/vaultStore";
import { getActiveViewDefinition, useViewStore } from "../../state/viewStore";

const bannerFlowStyle = { position: "static", gridColumn: "1 / -1" } as const;

export function AppHeader() {
  const activeView = useViewStore((state) => state.activeView);
  const tauriAvailable = useViewStore((state) => state.tauriAvailable);
  const loadingWorkspace = useViewStore((state) => state.loadingWorkspace);
  const globalError = useViewStore((state) => state.globalError);
  const persistenceWarning = useViewStore((state) => state.persistenceWarning);
  const notice = useViewStore((state) => state.notice);
  const setGlobalError = useViewStore((state) => state.setGlobalError);
  const setPersistenceWarning = useViewStore((state) => state.setPersistenceWarning);
  const setNotice = useViewStore((state) => state.setNotice);
  const project = usePulsationStore((state) => state.activeProject);
  const runtimeError = useRuntimeStore((state) => state.error);
  const vaultError = useVaultStore((state) => state.error);
  const workspaceRoot = useVaultStore((state) => state.workspaceRoot);
  const definition = getActiveViewDefinition(activeView);
  const error = globalError ?? persistenceWarning ?? runtimeError ?? vaultError;

  return (
    <header className="app-header">
      <div className="header-copy">
        <span className="eyebrow">{project?.name ?? "Desktop orchestrator"}</span>
        <h1>{definition.label}</h1>
        <p>{definition.description}</p>
      </div>
      <div className="header-actions">
        <div className="workspace-identity" title={workspaceRoot ?? "No workspace open"}>
          <span>Workspace</span>
          <strong>{workspaceRoot ? workspaceRoot.split(/[\\/]/).filter(Boolean).slice(-1)[0] : "Not open"}</strong>
        </div>
        <button
          type="button"
          className="button-secondary"
          disabled={!tauriAvailable || loadingWorkspace}
          onClick={() => void openWorkspaceFromDialog()}
        >
          {loadingWorkspace ? "Opening…" : "Open folder"}
        </button>
      </div>
      {!tauriAvailable ? (
        <div className="app-banner is-warning" style={bannerFlowStyle}>IPC is unavailable in browser mode. Run the Tauri desktop app.</div>
      ) : null}
      {error ? (
        <button
          type="button"
          className="app-banner is-error"
          style={bannerFlowStyle}
          onClick={() => {
            setGlobalError(null);
            setPersistenceWarning(null);
          }}
        >
          {persistenceWarning ? "Persistence: " : ""}{error}
        </button>
      ) : notice ? (
        <button type="button" className="app-banner is-notice" style={bannerFlowStyle} onClick={() => setNotice(null)}>{notice}</button>
      ) : null}
    </header>
  );
}
