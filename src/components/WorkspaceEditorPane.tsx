import {
  forwardRef,
  useEffect,
  useImperativeHandle,
  useMemo,
  useRef,
  type ForwardedRef
} from "react";
import Editor, { loader } from "@monaco-editor/react";
import * as monaco from "monaco-editor/esm/vs/editor/editor.api";
import type { editor as MonacoEditorNamespace } from "monaco-editor/esm/vs/editor/editor.api";
import "monaco-editor/esm/vs/basic-languages/markdown/markdown.contribution";
import editorWorker from "monaco-editor/esm/vs/editor/editor.worker?worker";

import { renderMarkdown } from "../markdown/renderMarkdown";
import type { EditorTab } from "../state/editorStore";

loader.config({ monaco });

const globalMonaco = self as unknown as {
  MonacoEnvironment?: {
    getWorker: (_moduleId: string, _label: string) => Worker;
  };
};

if (!globalMonaco.MonacoEnvironment) {
  globalMonaco.MonacoEnvironment = {
    getWorker() {
      return new editorWorker();
    }
  };
}

function languageFromPath(relativePath: string): string {
  const normalized = relativePath.toLowerCase();
  if (normalized.endsWith(".md")) {
    return "markdown";
  }

  return "plaintext";
}

export interface WorkspaceEditorPaneHandle {
  runFindInFile: () => void;
  focusAt: (line: number, column: number) => boolean;
}

interface WorkspaceEditorPaneProps {
  tabs: EditorTab[];
  activeTab: EditorTab | null;
  activeTabPath: string | null;
  setActiveTab: (relativePath: string) => void;
  onCloseTab: (relativePath: string) => void;
  onContentChange: (nextValue: string | undefined) => void;
  onResolveConflictByReload: () => void;
  onResolveConflictByOverwrite: () => void;
  onEditorMounted: () => void;
  onCursorChange: (relativePath: string, line: number | null, column: number | null) => void;
  onScrollChange: (relativePath: string, scrollTop: number | null) => void;
}

function WorkspaceEditorPane(
  {
    tabs,
    activeTab,
    activeTabPath,
    setActiveTab,
    onCloseTab,
    onContentChange,
    onResolveConflictByReload,
    onResolveConflictByOverwrite,
    onEditorMounted,
    onCursorChange,
    onScrollChange
  }: WorkspaceEditorPaneProps,
  ref: ForwardedRef<WorkspaceEditorPaneHandle>
) {
  const editorRef = useRef<MonacoEditorNamespace.IStandaloneCodeEditor | null>(null);
  const editorDisposablesRef = useRef<monaco.IDisposable[]>([]);
  const activeTabPathRef = useRef<string | null>(activeTabPath);
  const previewHtml = useMemo(() => renderMarkdown(activeTab?.content ?? ""), [activeTab?.content]);

  useImperativeHandle(
    ref,
    () => ({
      runFindInFile: () => {
        void editorRef.current?.getAction("actions.find")?.run();
      },
      focusAt: (line: number, column: number) => {
        if (!editorRef.current) {
          return false;
        }

        editorRef.current.revealPositionInCenter({
          lineNumber: line,
          column
        });
        editorRef.current.setPosition({
          lineNumber: line,
          column
        });
        editorRef.current.focus();
        return true;
      }
    }),
    []
  );

  useEffect(() => {
    activeTabPathRef.current = activeTabPath;
  }, [activeTabPath]);

  useEffect(() => {
    const editor = editorRef.current;
    if (!editor || !activeTab) {
      return;
    }

    if (activeTab.cursorLine && activeTab.cursorColumn) {
      editor.setPosition({
        lineNumber: Math.max(1, activeTab.cursorLine),
        column: Math.max(1, activeTab.cursorColumn)
      });
    }

    if (activeTab.scrollTop !== null && activeTab.scrollTop !== undefined) {
      editor.setScrollTop(Math.max(0, activeTab.scrollTop));
    }
  }, [activeTab?.cursorColumn, activeTab?.cursorLine, activeTab?.relativePath, activeTab?.scrollTop]);

  useEffect(() => {
    return () => {
      editorDisposablesRef.current.forEach((disposable) => disposable.dispose());
      editorDisposablesRef.current = [];
    };
  }, []);

  return (
    <div className="editor-mode">
      <div className="tabs">
        {tabs.map((tab) => (
          <button
            type="button"
            key={tab.relativePath}
            className={`tab ${tab.relativePath === activeTabPath ? "tab-active" : ""}`}
            onClick={() => setActiveTab(tab.relativePath)}
          >
            <span>{tab.name}</span>
            {tab.saving ? <small>saving</small> : null}
            {tab.dirty && !tab.saving ? <small>dirty</small> : null}
            {tab.conflict ? <small>conflict</small> : null}
            <span className="tab-close-wrap">
              <span
                className="tab-close"
                onClick={(event) => {
                  event.stopPropagation();
                  onCloseTab(tab.relativePath);
                }}
              >
                x
              </span>
            </span>
          </button>
        ))}
      </div>

      {activeTab?.conflict ? (
        <div className="conflict-box">
          <strong>Revision conflict detected.</strong>
          <p>{activeTab.conflict.message}</p>
          <div className="conflict-actions">
            <button type="button" onClick={onResolveConflictByReload}>
              Reload from disk
            </button>
            <button type="button" onClick={onResolveConflictByOverwrite}>
              Overwrite disk
            </button>
          </div>
        </div>
      ) : null}

      {activeTab?.saveError ? <p className="error">{activeTab.saveError}</p> : null}

      <div className="workspace-split">
        <div className="editor-surface">
          {activeTab ? (
            <Editor
              language={languageFromPath(activeTab.relativePath)}
              path={activeTab.relativePath}
              value={activeTab.content}
              theme="vs-dark"
              options={{
                minimap: { enabled: false },
                wordWrap: "on",
                fontSize: 14,
                automaticLayout: true
              }}
              onMount={(editorInstance) => {
                editorDisposablesRef.current.forEach((disposable) => disposable.dispose());
                editorDisposablesRef.current = [];

                editorRef.current = editorInstance;
                const onCursorDisposable = editorInstance.onDidChangeCursorPosition((event) => {
                  if (!activeTabPathRef.current) {
                    return;
                  }
                  onCursorChange(
                    activeTabPathRef.current,
                    event.position.lineNumber,
                    event.position.column
                  );
                });
                const onScrollDisposable = editorInstance.onDidScrollChange((event) => {
                  if (!activeTabPathRef.current || !event.scrollTopChanged) {
                    return;
                  }
                  onScrollChange(activeTabPathRef.current, event.scrollTop);
                });

                editorDisposablesRef.current = [onCursorDisposable, onScrollDisposable];
                onEditorMounted();
              }}
              onChange={onContentChange}
            />
          ) : (
            <p className="empty-text">Open a file from the tree to start editing.</p>
          )}
        </div>
        <div className="preview-surface">
          <h3>Markdown Preview</h3>
          <div className="markdown-preview" dangerouslySetInnerHTML={{ __html: previewHtml }} />
        </div>
      </div>
    </div>
  );
}

export default forwardRef(WorkspaceEditorPane);
