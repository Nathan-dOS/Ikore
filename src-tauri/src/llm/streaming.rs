use serde_json::Value;

use super::domain::ProviderProtocol;
use super::error::{LlmError, LlmErrorKind};

pub const DELTA_BATCH_BYTES: usize = 1024;
pub const MAX_STREAM_BUFFER_BYTES: usize = 256 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParsedStreamEvent {
    Delta(String),
    ToolCallStarted {
        index: usize,
        id: String,
        name: String,
    },
    ToolArgumentsDelta {
        index: usize,
        delta: String,
    },
    ToolCallCompleted {
        index: usize,
    },
    Completed,
    Usage {
        input_tokens: Option<i64>,
        output_tokens: Option<i64>,
    },
}

#[derive(Debug)]
pub struct SseParser {
    protocol: ProviderProtocol,
    buffer: Vec<u8>,
}

impl SseParser {
    pub fn new(protocol: ProviderProtocol) -> Self {
        Self {
            protocol,
            buffer: Vec::new(),
        }
    }

    pub fn push(&mut self, chunk: &[u8]) -> Result<Vec<ParsedStreamEvent>, LlmError> {
        self.buffer.extend_from_slice(chunk);
        if self.buffer.len() > MAX_STREAM_BUFFER_BYTES {
            return Err(LlmError::new(
                LlmErrorKind::StreamFailed,
                "Buffer máximo do stream excedido.",
            ));
        }

        let mut events = Vec::new();
        while let Some((boundary, delimiter_len)) = find_frame_boundary(&self.buffer) {
            let frame_bytes = self.buffer.drain(..boundary).collect::<Vec<_>>();
            self.buffer.drain(..delimiter_len);
            let frame = String::from_utf8(frame_bytes).map_err(|_| {
                LlmError::new(
                    LlmErrorKind::StreamFailed,
                    "Stream do provider contém UTF-8 inválido.",
                )
            })?;
            if let Some(event) = self.parse_frame(&frame)? {
                events.extend(event);
            }
        }
        Ok(events)
    }

    pub fn finish(&mut self) -> Result<Vec<ParsedStreamEvent>, LlmError> {
        if self.buffer.iter().all(u8::is_ascii_whitespace) {
            self.buffer.clear();
            return Ok(Vec::new());
        }
        let frame = String::from_utf8(std::mem::take(&mut self.buffer)).map_err(|_| {
            LlmError::new(
                LlmErrorKind::StreamFailed,
                "Stream do provider contém UTF-8 inválido.",
            )
        })?;
        Ok(self.parse_frame(&frame)?.unwrap_or_default())
    }

    fn parse_frame(&mut self, frame: &str) -> Result<Option<Vec<ParsedStreamEvent>>, LlmError> {
        let mut data_lines = Vec::new();
        let mut frame_event = None;
        for line in frame.lines() {
            if let Some(value) = line.strip_prefix("event:") {
                frame_event = Some(value.trim().to_string());
            } else if let Some(value) = line.strip_prefix("data:") {
                data_lines.push(value.trim_start());
            }
        }
        if data_lines.is_empty() {
            return Ok(None);
        }
        let data = data_lines.join("\n");
        if data.trim() == "[DONE]" {
            return Ok(Some(vec![ParsedStreamEvent::Completed]));
        }
        let payload: Value = serde_json::from_str(&data).map_err(|_| {
            LlmError::new(
                LlmErrorKind::StreamFailed,
                "Evento SSE contém JSON inválido.",
            )
        })?;
        Ok(Some(parse_payload(
            self.protocol,
            frame_event.as_deref(),
            &payload,
        )))
    }
}

fn find_frame_boundary(buffer: &[u8]) -> Option<(usize, usize)> {
    let lf = buffer.windows(2).position(|window| window == b"\n\n");
    let crlf = buffer.windows(4).position(|window| window == b"\r\n\r\n");
    match (lf, crlf) {
        (Some(left), Some(right)) if left <= right => Some((left, 2)),
        (Some(_), Some(right)) => Some((right, 4)),
        (Some(left), None) => Some((left, 2)),
        (None, Some(right)) => Some((right, 4)),
        (None, None) => None,
    }
}

fn parse_payload(
    protocol: ProviderProtocol,
    event_name: Option<&str>,
    payload: &Value,
) -> Vec<ParsedStreamEvent> {
    match protocol {
        ProviderProtocol::OpenaiResponses => parse_responses(event_name, payload),
        ProviderProtocol::OpenaiChat => parse_chat(payload),
        ProviderProtocol::AnthropicMessages => parse_anthropic(event_name, payload),
    }
}

fn parse_responses(event_name: Option<&str>, payload: &Value) -> Vec<ParsedStreamEvent> {
    let event_type = event_name
        .or_else(|| payload.get("type").and_then(Value::as_str))
        .unwrap_or("");
    match event_type {
        "response.output_text.delta" => payload
            .get("delta")
            .and_then(Value::as_str)
            .filter(|delta| !delta.is_empty())
            .map(|delta| vec![ParsedStreamEvent::Delta(delta.to_string())])
            .unwrap_or_default(),
        "response.completed" => {
            let usage = payload.get("response").and_then(|value| value.get("usage"));
            vec![
                ParsedStreamEvent::Usage {
                    input_tokens: usage
                        .and_then(|value| value.get("input_tokens"))
                        .and_then(Value::as_i64),
                    output_tokens: usage
                        .and_then(|value| value.get("output_tokens"))
                        .and_then(Value::as_i64),
                },
                ParsedStreamEvent::Completed,
            ]
        }
        "response.output_item.added"
            if payload.pointer("/item/type").and_then(Value::as_str) == Some("function_call") =>
        {
            vec![ParsedStreamEvent::ToolCallStarted {
                index: payload
                    .get("output_index")
                    .and_then(Value::as_u64)
                    .unwrap_or(0) as usize,
                id: payload
                    .pointer("/item/call_id")
                    .or_else(|| payload.pointer("/item/id"))
                    .and_then(Value::as_str)
                    .unwrap_or("tool-call")
                    .to_string(),
                name: payload
                    .pointer("/item/name")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            }]
        }
        "response.function_call_arguments.delta" => vec![ParsedStreamEvent::ToolArgumentsDelta {
            index: payload
                .get("output_index")
                .and_then(Value::as_u64)
                .unwrap_or(0) as usize,
            delta: payload
                .get("delta")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
        }],
        "response.function_call_arguments.done" => vec![ParsedStreamEvent::ToolCallCompleted {
            index: payload
                .get("output_index")
                .and_then(Value::as_u64)
                .unwrap_or(0) as usize,
        }],
        _ => Vec::new(),
    }
}

fn parse_chat(payload: &Value) -> Vec<ParsedStreamEvent> {
    let mut events = Vec::new();
    if let Some(delta) = payload
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
        .and_then(|choice| choice.get("delta"))
        .and_then(|delta| delta.get("content"))
        .and_then(Value::as_str)
        .filter(|delta| !delta.is_empty())
    {
        events.push(ParsedStreamEvent::Delta(delta.to_string()));
    }
    if let Some(tool_calls) = payload
        .pointer("/choices/0/delta/tool_calls")
        .and_then(Value::as_array)
    {
        for call in tool_calls {
            let index = call.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
            let id = call.get("id").and_then(Value::as_str).unwrap_or_default();
            let name = call
                .pointer("/function/name")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if !id.is_empty() || !name.is_empty() {
                events.push(ParsedStreamEvent::ToolCallStarted {
                    index,
                    id: id.to_string(),
                    name: name.to_string(),
                });
            }
            if let Some(arguments) = call
                .pointer("/function/arguments")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
            {
                events.push(ParsedStreamEvent::ToolArgumentsDelta {
                    index,
                    delta: arguments.to_string(),
                });
            }
        }
    }
    if payload
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
        .and_then(|choice| choice.get("finish_reason"))
        .is_some_and(|reason| !reason.is_null())
    {
        if payload
            .pointer("/choices/0/finish_reason")
            .and_then(Value::as_str)
            == Some("tool_calls")
        {
            let indices = payload
                .pointer("/choices/0/delta/tool_calls")
                .and_then(Value::as_array)
                .map(|calls| {
                    calls
                        .iter()
                        .filter_map(|call| call.get("index").and_then(Value::as_u64))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            events.extend(
                indices
                    .into_iter()
                    .map(|index| ParsedStreamEvent::ToolCallCompleted {
                        index: index as usize,
                    }),
            );
        }
        events.push(ParsedStreamEvent::Completed);
    }
    if let Some(usage) = payload.get("usage") {
        events.push(ParsedStreamEvent::Usage {
            input_tokens: usage.get("prompt_tokens").and_then(Value::as_i64),
            output_tokens: usage.get("completion_tokens").and_then(Value::as_i64),
        });
    }
    events
}

fn parse_anthropic(event_name: Option<&str>, payload: &Value) -> Vec<ParsedStreamEvent> {
    let event_type = event_name
        .or_else(|| payload.get("type").and_then(Value::as_str))
        .unwrap_or("");
    match event_type {
        "content_block_delta" => payload
            .get("delta")
            .map(|delta| match delta.get("type").and_then(Value::as_str) {
                Some("input_json_delta") => vec![ParsedStreamEvent::ToolArgumentsDelta {
                    index: payload.get("index").and_then(Value::as_u64).unwrap_or(0) as usize,
                    delta: delta
                        .get("partial_json")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                }],
                _ => delta
                    .get("text")
                    .and_then(Value::as_str)
                    .filter(|delta| !delta.is_empty())
                    .map(|delta| vec![ParsedStreamEvent::Delta(delta.to_string())])
                    .unwrap_or_default(),
            })
            .unwrap_or_default(),
        "content_block_start"
            if payload
                .pointer("/content_block/type")
                .and_then(Value::as_str)
                == Some("tool_use") =>
        {
            vec![ParsedStreamEvent::ToolCallStarted {
                index: payload.get("index").and_then(Value::as_u64).unwrap_or(0) as usize,
                id: payload
                    .pointer("/content_block/id")
                    .and_then(Value::as_str)
                    .unwrap_or("tool-call")
                    .to_string(),
                name: payload
                    .pointer("/content_block/name")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            }]
        }
        "content_block_stop" => vec![ParsedStreamEvent::ToolCallCompleted {
            index: payload.get("index").and_then(Value::as_u64).unwrap_or(0) as usize,
        }],
        "message_delta" => vec![ParsedStreamEvent::Usage {
            input_tokens: None,
            output_tokens: payload
                .get("usage")
                .and_then(|usage| usage.get("output_tokens"))
                .and_then(Value::as_i64),
        }],
        "message_start" => vec![ParsedStreamEvent::Usage {
            input_tokens: payload
                .pointer("/message/usage/input_tokens")
                .and_then(Value::as_i64),
            output_tokens: payload
                .pointer("/message/usage/output_tokens")
                .and_then(Value::as_i64),
        }],
        "message_stop" => vec![ParsedStreamEvent::Completed],
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_fragmented_responses_sse() {
        let mut parser = SseParser::new(ProviderProtocol::OpenaiResponses);
        assert!(parser
            .push(b"event: response.output_text.delta\nda")
            .unwrap()
            .is_empty());
        let events = parser.push(b"ta: {\"delta\":\"ola\"}\n\n").unwrap();
        assert_eq!(events, vec![ParsedStreamEvent::Delta("ola".to_string())]);
    }

    #[test]
    fn parses_fragmented_responses_tool_call() {
        let mut parser = SseParser::new(ProviderProtocol::OpenaiResponses);
        let started = parser
            .push(b"event: response.output_item.added\ndata: {\"type\":\"response.output_item.added\",\"output_index\":1,\"item\":{\"type\":\"function_call\",\"call_id\":\"call-1\",\"name\":\"read_repository_file\"}}\n\n")
            .unwrap();
        assert!(matches!(started[0], ParsedStreamEvent::ToolCallStarted { index: 1, .. }));
        assert!(parser
            .push(b"event: response.function_call_arguments.delta\nda")
            .unwrap()
            .is_empty());
        let arguments = parser
            .push(b"ta: {\"type\":\"response.function_call_arguments.delta\",\"output_index\":1,\"delta\":\"{\\\"path\\\":\\\"src/lib.rs\\\"}\"}\n\n")
            .unwrap();
        assert_eq!(
            arguments,
            vec![ParsedStreamEvent::ToolArgumentsDelta {
                index: 1,
                delta: "{\"path\":\"src/lib.rs\"}".into()
            }]
        );
    }

    #[test]
    fn parses_fragmented_chat_sse() {
        let mut parser = SseParser::new(ProviderProtocol::OpenaiChat);
        let events = parser
            .push(
                b"data: {\"choices\":[{\"delta\":{\"content\":\"a\"},\"finish_reason\":null}]}\n\n",
            )
            .unwrap();
        assert_eq!(events, vec![ParsedStreamEvent::Delta("a".to_string())]);
    }

    #[test]
    fn preserves_utf8_split_between_arbitrary_chunks() {
        let mut parser = SseParser::new(ProviderProtocol::OpenaiChat);
        let frame =
            "data: {\"choices\":[{\"delta\":{\"content\":\"olá 🙂\"},\"finish_reason\":null}]}\n\n"
                .as_bytes()
                .to_vec();
        let emoji_start = frame
            .windows(4)
            .position(|window| window == "🙂".as_bytes())
            .unwrap();
        assert!(parser.push(&frame[..emoji_start + 1]).unwrap().is_empty());
        assert_eq!(
            parser.push(&frame[emoji_start + 1..]).unwrap(),
            vec![ParsedStreamEvent::Delta("olá 🙂".to_string())]
        );
    }

    #[test]
    fn parses_anthropic_delta() {
        let mut parser = SseParser::new(ProviderProtocol::AnthropicMessages);
        let events = parser.push(b"event: content_block_delta\ndata: {\"delta\":{\"type\":\"text_delta\",\"text\":\"x\"}}\n\n").unwrap();
        assert_eq!(events, vec![ParsedStreamEvent::Delta("x".to_string())]);
    }

    #[test]
    fn parses_fragmented_openai_compatible_tool_arguments() {
        let mut parser = SseParser::new(ProviderProtocol::OpenaiChat);
        let first = parser
            .push(b"data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call-1\",\"function\":{\"name\":\"read_repository_file\",\"arguments\":\"{\\\"pa\"}}]},\"finish_reason\":null}]}\n\n")
            .unwrap();
        let second = parser
            .push(b"data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"th\\\":\\\"src/lib.rs\\\"}\"}}]},\"finish_reason\":null}]}\n\n")
            .unwrap();
        assert!(matches!(
            first[0],
            ParsedStreamEvent::ToolCallStarted { index: 0, .. }
        ));
        let fragments = first
            .into_iter()
            .chain(second)
            .filter_map(|event| match event {
                ParsedStreamEvent::ToolArgumentsDelta { delta, .. } => Some(delta),
                _ => None,
            })
            .collect::<String>();
        assert_eq!(fragments, "{\"path\":\"src/lib.rs\"}");
    }

    #[test]
    fn parses_anthropic_tool_use_fragments() {
        let mut parser = SseParser::new(ProviderProtocol::AnthropicMessages);
        let started = parser
            .push(b"event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"tool_use\",\"id\":\"tool-1\",\"name\":\"search_repository\",\"input\":{}}}\n\n")
            .unwrap();
        let delta = parser
            .push(b"event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"query\\\":\\\"AppView\\\"}\"}}\n\n")
            .unwrap();
        assert!(matches!(
            started[0],
            ParsedStreamEvent::ToolCallStarted { index: 1, .. }
        ));
        assert!(matches!(
            delta[0],
            ParsedStreamEvent::ToolArgumentsDelta { index: 1, .. }
        ));
    }
}
