//! Moving through the Companion's conversations (PI-Desktop analysis, PD11): a queue for
//! messages written while a conversation replies, branching from a message, searching the
//! project's conversations, and pinning or archiving them.

use gpui::{Context, Entity};
use ikore_core::chat_service::ConversationMark;

use crate::app_model::AppModel;
use crate::chat::{ConversationSearch, MAX_QUEUED_MESSAGES};
use crate::session::HeaderNoticeKind;
use crate::text_input::NativeTextInput;

impl AppModel {
    /// Reads the project's pinned and archived conversations.
    pub(crate) fn refresh_chat_marks(&mut self, cx: &mut Context<Self>) {
        let Some(backend) = self.chat_backend.clone() else {
            return;
        };
        let Some(project_id) = self
            .chat
            .read(cx)
            .project_id
            .as_ref()
            .map(ToString::to_string)
        else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let result = backend.conversation_marks(project_id.clone()).await;
            let _ = this.update(cx, |model, cx| {
                model.chat.update(cx, |chat, _| {
                    if chat.project_id.as_ref().map(AsRef::as_ref) == Some(project_id.as_str())
                        && let Ok(marks) = result
                    {
                        chat.marks = marks;
                    }
                });
                cx.notify();
            });
        })
        .detach();
    }

    /// Pins, unpins, archives or restores a conversation. The list follows the store's answer.
    pub(crate) fn mark_chat_conversation(
        &mut self,
        conversation_id: String,
        mark: ConversationMark,
        on: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(backend) = self.chat_backend.clone() else {
            return;
        };
        let project_id = self.chat.read(cx).project_id.clone();
        cx.spawn(async move |this, cx| {
            let result = backend.mark_conversation(conversation_id, mark, on).await;
            let _ = this.update(cx, |model, cx| {
                match result {
                    Ok(marks) => model.chat.update(cx, |chat, _| {
                        if chat.project_id == project_id {
                            chat.marks = marks;
                        }
                    }),
                    Err(error) => {
                        model.push_header_notice(HeaderNoticeKind::Warning, error.presentable(), cx)
                    }
                }
                // A sidebar that is open shows pins and archives too.
                model.refresh_projects_sidebar(cx);
                cx.notify();
            });
        })
        .detach();
    }

    pub(crate) fn toggle_archived_chats(&mut self, cx: &mut Context<Self>) {
        self.chat
            .update(cx, |chat, _| chat.show_archived = !chat.show_archived);
        cx.notify();
    }

    /// Searches the project's conversations for the search field's text. An empty field
    /// closes the search and the list comes back.
    pub(crate) fn search_chat_conversations(&mut self, cx: &mut Context<Self>) {
        let query = self.chat_search_input.read(cx).value().trim().to_owned();
        let project_id = self
            .chat
            .read(cx)
            .project_id
            .as_ref()
            .map(ToString::to_string);
        let (Some(project_id), Some(backend), false) =
            (project_id, self.chat_backend.clone(), query.is_empty())
        else {
            self.clear_chat_search(cx);
            return;
        };
        self.chat.update(cx, |chat, _| {
            chat.conversation_search = Some(ConversationSearch {
                query: query.clone(),
                matches: Vec::new(),
                searching: true,
            });
        });
        self.chat_search_task = Some(cx.spawn(async move |this, cx| {
            let result = backend
                .search_conversations(project_id.clone(), query.clone())
                .await;
            let _ = this.update(cx, |model, cx| {
                model.chat.update(cx, |chat, _| {
                    let Some(search) = chat.conversation_search.as_mut() else {
                        return;
                    };
                    if search.query != query
                        || chat.project_id.as_ref().map(AsRef::as_ref) != Some(project_id.as_str())
                    {
                        return;
                    }
                    search.searching = false;
                    search.matches = result.unwrap_or_default();
                });
                cx.notify();
            });
        }));
        cx.notify();
    }

    pub(crate) fn clear_chat_search(&mut self, cx: &mut Context<Self>) {
        self.chat_search_task = None;
        self.chat_search_input
            .update(cx, |input, cx| input.clear(cx));
        self.chat
            .update(cx, |chat, _| chat.conversation_search = None);
        cx.notify();
    }

    /// Starts a new conversation with the history up to `message_id` and opens it.
    pub(crate) fn fork_chat_conversation(
        &mut self,
        conversation_id: String,
        message_id: String,
        cx: &mut Context<Self>,
    ) {
        let Some(backend) = self.chat_backend.clone() else {
            return;
        };
        let project_id = self.chat.read(cx).project_id.clone();
        cx.spawn(async move |this, cx| {
            let result = backend.fork_conversation(conversation_id, message_id).await;
            let _ = this.update(cx, |model, cx| {
                match result {
                    Ok(snapshot) => {
                        let id = snapshot.conversation.id.clone();
                        let applied = model.chat.update(cx, |chat, _| {
                            if chat.project_id != project_id {
                                return false;
                            }
                            chat.apply_created(snapshot);
                            true
                        });
                        if applied {
                            model.ensure_chat_composer(&id, cx);
                            model.select_view(crate::app_model::AppView::Chat, cx);
                            model.schedule_session_persistence(cx);
                            model.refresh_projects_sidebar(cx);
                        }
                    }
                    Err(error) => {
                        model.push_header_notice(HeaderNoticeKind::Warning, error.presentable(), cx)
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Keeps a message for when the conversation is free, and empties the field it came from.
    pub(crate) fn queue_chat_message(
        &mut self,
        conversation_id: String,
        content: String,
        input: Option<Entity<NativeTextInput>>,
        cx: &mut Context<Self>,
    ) {
        let content = content.trim().to_owned();
        if content.is_empty() {
            return;
        }
        let queued = self
            .chat
            .update(cx, |chat, _| chat.enqueue(&conversation_id, content));
        if !queued {
            self.push_header_notice(
                HeaderNoticeKind::Warning,
                format!(
                    "This conversation already has {MAX_QUEUED_MESSAGES} messages waiting. Remove one or wait for the reply."
                ),
                cx,
            );
            return;
        }
        if let Some(input) = input {
            input.update(cx, |input, cx| input.clear(cx));
            self.companion_draft_changed(conversation_id, String::new(), cx);
        }
        cx.notify();
    }

    pub(crate) fn move_queued_chat_message(
        &mut self,
        conversation_id: String,
        index: usize,
        cx: &mut Context<Self>,
    ) {
        self.chat
            .update(cx, |chat, _| chat.move_queued_up(&conversation_id, index));
        cx.notify();
    }

    pub(crate) fn remove_queued_chat_message(
        &mut self,
        conversation_id: String,
        index: usize,
        cx: &mut Context<Self>,
    ) {
        self.chat
            .update(cx, |chat, _| chat.remove_queued(&conversation_id, index));
        cx.notify();
    }

    /// Sends a queued message now, when the conversation is free but its queue waits (after an
    /// error, for example).
    pub(crate) fn send_queued_chat_message_now(
        &mut self,
        conversation_id: String,
        index: usize,
        cx: &mut Context<Self>,
    ) {
        let Some(content) = self
            .chat
            .read(cx)
            .queued(&conversation_id)
            .get(index)
            .cloned()
        else {
            return;
        };
        if self.send_chat_message_to(conversation_id.clone(), content, cx) {
            self.chat
                .update(cx, |chat, _| chat.remove_queued(&conversation_id, index));
        }
        cx.notify();
    }

    /// Sends the next queued message of every conversation that is free again.
    pub(crate) fn flush_chat_queue(&mut self, cx: &mut Context<Self>) {
        let ready = self.chat.read(cx).queues_ready();
        for conversation_id in ready {
            let Some(content) = self.chat.read(cx).queued(&conversation_id).first().cloned() else {
                continue;
            };
            if self.send_chat_message_to(conversation_id.clone(), content, cx) {
                self.chat
                    .update(cx, |chat, _| chat.remove_queued(&conversation_id, 0));
                cx.notify();
            }
        }
    }
}
