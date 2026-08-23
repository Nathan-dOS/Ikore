import { FormEvent, useMemo, useState } from "react";

import { ChatMessageContent } from "../components/chat/ChatMessageContent";
import { EmptyState } from "../components/EmptyState";
import { useChatStore } from "../state/chatStore";
import { usePulsationStore } from "../state/pulsationStore";
import { useRuntimeStore } from "../state/runtimeStore";
import { formatDateTime } from "../utils/formatters";

export function ChatView() {
  const project = usePulsationStore((state) => state.activeProject);
  const connections = useRuntimeStore((state) => state.connections);
  const conversations = useChatStore((state) => state.conversations);
  const activeConversationId = useChatStore((state) => state.activeConversationId);
  const messagesByConversation = useChatStore((state) => state.messagesByConversation);
  const toolCallsByConversation = useChatStore((state) => state.toolCallsByConversation);
  const loading = useChatStore((state) => state.loading);
  const busy = useChatStore((state) => state.busy);
  const error = useChatStore((state) => state.error);
  const selectConversation = useChatStore((state) => state.selectConversation);
  const createConversation = useChatStore((state) => state.createConversation);
  const deleteConversation = useChatStore((state) => state.deleteConversation);
  const sendMessage = useChatStore((state) => state.sendMessage);
  const cancelActiveTurn = useChatStore((state) => state.cancelActiveTurn);
  const [connectionId, setConnectionId] = useState("");
  const [draft, setDraft] = useState("");
  const [acknowledged, setAcknowledged] = useState<Record<string, boolean>>({});

  const eligibleConnections = useMemo(
    () => connections.filter((connection) => connection.chatEligible),
    [connections]
  );
  const effectiveConnectionId = eligibleConnections.some(
    (connection) => connection.id === connectionId
  )
    ? connectionId
    : eligibleConnections[0]?.id ?? "";
  const activeConversation =
    conversations.find((conversation) => conversation.id === activeConversationId) ?? null;
  const messages = activeConversationId ? messagesByConversation[activeConversationId] ?? [] : [];
  const toolCalls = activeConversationId ? toolCallsByConversation[activeConversationId] ?? [] : [];
  const activeConnection = connections.find(
    (connection) => connection.id === activeConversation?.connectionId
  );
  const activeTurn = [...messages]
    .reverse()
    .find(
      (message) =>
        message.role === "assistant" &&
        (message.status === "pending" || message.status === "streaming")
    );
  const requiresDisclosure = Boolean(activeConversation && messages.length === 0);
  const disclosureAccepted = activeConversation
    ? acknowledged[activeConversation.id] === true || !requiresDisclosure
    : false;

  if (!project) {
    return (
      <EmptyState
        title="Abra um projeto para conversar"
        description="O Companion pertence ao workspace ativo e nunca recebe acesso fora desse repositório."
      />
    );
  }

  const create = async () => {
    if (!effectiveConnectionId) return;
    await createConversation(effectiveConnectionId).catch(() => undefined);
    setDraft("");
  };

  const remove = async (conversationId: string) => {
    if (!window.confirm("Excluir esta conversa e todo o histórico local?")) return;
    await deleteConversation(conversationId).catch(() => undefined);
  };

  const submit = async (event: FormEvent) => {
    event.preventDefault();
    if (!draft.trim() || !disclosureAccepted || activeTurn) return;
    const content = draft;
    setDraft("");
    try {
      await sendMessage(content);
    } catch {
      setDraft(content);
    }
  };

  return (
    <div className="chat-view">
      <aside className="panel chat-conversation-panel">
        <div className="section-heading">
          <div><span className="eyebrow">Projeto ativo</span><h2>Conversas</h2></div>
          <span className="count-chip">{conversations.length}</span>
        </div>
        <label className="field-stack">
          <span>Conexão compatível</span>
          <select
            aria-label="Conexão do novo chat"
            value={effectiveConnectionId}
            onChange={(event) => setConnectionId(event.target.value)}
            disabled={eligibleConnections.length === 0 || busy}
          >
            {eligibleConnections.length === 0 ? <option value="">Nenhuma disponível</option> : null}
            {eligibleConnections.map((connection) => (
              <option key={connection.id} value={connection.id}>
                {connection.displayName} · {connection.defaultModel}
              </option>
            ))}
          </select>
        </label>
        <button type="button" className="primary-button" onClick={() => void create()} disabled={!effectiveConnectionId || busy}>
          Nova conversa
        </button>
        {eligibleConnections.length === 0 ? (
          <p className="inline-warning">Configure uma conexão habilitada, credenciada e com tool calling em Settings.</p>
        ) : null}
        <nav className="conversation-list" aria-label="Conversas do projeto">
          {conversations.map((conversation) => (
            <div key={conversation.id} className={conversation.id === activeConversationId ? "is-active" : ""}>
              <button type="button" onClick={() => void selectConversation(conversation.id)}>
                <strong>{conversation.title}</strong>
                <span>{conversation.providerId} · {conversation.modelId}</span>
              </button>
              <button
                type="button"
                className="danger-ghost"
                aria-label={`Excluir ${conversation.title}`}
                onClick={() => void remove(conversation.id)}
                disabled={messagesByConversation[conversation.id]?.some(
                  (message) => message.status === "pending" || message.status === "streaming"
                )}
              >
                ×
              </button>
            </div>
          ))}
        </nav>
      </aside>

      <section className="panel chat-thread-panel">
        {!activeConversation ? (
          <EmptyState
            title={loading ? "Carregando conversas…" : "Comece uma conversa"}
            description="Escolha uma conexão compatível. Provider e modelo ficarão fixos nesta conversa."
          />
        ) : (
          <>
            <header className="chat-thread-header">
              <div><span className="eyebrow">Chat / Companion</span><h2>{activeConversation.title}</h2></div>
              <div className="chat-runtime-lock">
                <strong>{activeConversation.providerId}</strong>
                <span>{activeConnection?.displayName ?? "Conexão removida"}</span>
                <code>{activeConversation.modelId}</code>
              </div>
            </header>
            {!activeConversation.connectionAvailable ? (
              <p className="inline-warning">Esta conversa permanece legível, mas a conexão fixada não aceita novos envios.</p>
            ) : null}
            <div className="chat-message-list" aria-live="polite">
              {messages.length === 0 ? (
                <div className="chat-welcome">
                  <strong>Converse sobre a arquitetura real deste repositório.</strong>
                  <span>O Companion pode listar, buscar e ler trechos seguros. Ele não pode aplicar diffs nem alterar arquivos.</span>
                </div>
              ) : null}
              {messages.map((message) => (
                <article key={message.id} className={`chat-message role-${message.role}`}>
                  <header>
                    <strong>{message.role === "user" ? "Você" : "Companion"}</strong>
                    <span>{message.status}</span>
                    <time>{formatDateTime(message.createdAt)}</time>
                  </header>
                  {message.content ? <ChatMessageContent content={message.content} /> : (
                    <p className="muted">{message.status === "pending" || message.status === "streaming" ? "Pensando…" : "Sem conteúdo."}</p>
                  )}
                  {message.errorMessage ? <p className="inline-error">{message.errorMessage}</p> : null}
                </article>
              ))}
            </div>
            <form className="chat-composer" onSubmit={(event) => void submit(event)}>
              {requiresDisclosure ? (
                <label className="repository-disclosure">
                  <input
                    type="checkbox"
                    checked={acknowledged[activeConversation.id] ?? false}
                    onChange={(event) =>
                      setAcknowledged((current) => ({ ...current, [activeConversation.id]: event.target.checked }))
                    }
                  />
                  <span>Entendo que trechos consultados do repositório serão enviados à API desta conexão.</span>
                </label>
              ) : null}
              <textarea
                aria-label="Mensagem para o Companion"
                value={draft}
                onChange={(event) => setDraft(event.target.value)}
                placeholder="Pergunte sobre arquitetura, fluxo de dados, riscos ou peça uma proposta de diff…"
                disabled={Boolean(activeTurn) || !activeConversation.connectionAvailable}
                rows={3}
              />
              <div>
                <span>Somente leitura · sem shell, web, Git ou escrita</span>
                {activeTurn ? (
                  <button type="button" className="danger-button" onClick={() => void cancelActiveTurn()}>
                    Cancelar resposta
                  </button>
                ) : (
                  <button
                    type="submit"
                    className="primary-button"
                    disabled={!draft.trim() || !disclosureAccepted || busy || !activeConversation.connectionAvailable}
                  >
                    Enviar
                  </button>
                )}
              </div>
            </form>
          </>
        )}
        {error ? <p className="inline-error chat-error">{error}</p> : null}
      </section>

      <aside className="panel chat-tool-panel">
        <div className="section-heading"><div><span className="eyebrow">Read-only</span><h2>Atividade</h2></div><span className="count-chip">{toolCalls.length}</span></div>
        {toolCalls.length === 0 ? <p className="muted">As buscas e leituras aparecerão aqui durante a resposta.</p> : null}
        <div className="tool-activity-list">
          {toolCalls.map((tool) => (
            <article key={tool.id} className={`tool-activity status-${tool.status}`}>
              <header><strong>{toolLabel(tool.toolName)}</strong><span>{tool.status}</span></header>
              <code>{summarizeArguments(tool.sanitizedArguments)}</code>
              {tool.resultSummary ? <p>{tool.resultSummary}</p> : null}
              <small>{tool.resultBytes} bytes · rodada {tool.roundIndex + 1}</small>
            </article>
          ))}
        </div>
      </aside>
    </div>
  );
}

function toolLabel(name: string): string {
  if (name === "list_repository_tree") return "Listando árvore";
  if (name === "search_repository") return "Buscando no repositório";
  if (name === "read_repository_file") return "Lendo arquivo";
  return name;
}

function summarizeArguments(argumentsValue: Record<string, unknown>): string {
  const path = typeof argumentsValue.path === "string" ? argumentsValue.path : ".";
  const query = typeof argumentsValue.query === "string" ? ` · “${argumentsValue.query}”` : "";
  return `${path}${query}`;
}
