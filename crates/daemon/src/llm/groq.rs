//! Cliente OpenAI-compatible de Groq; la clave queda encapsulada aquí.
use super::{
    ChatMessage, GenerationRequest, GenerationStream, LlmProvider, ProviderError, StreamItem,
    ToolCallRequest,
};
use async_openai::types::chat::{
    ChatCompletionMessageToolCall, ChatCompletionMessageToolCalls,
    ChatCompletionRequestAssistantMessageArgs, ChatCompletionRequestMessage,
    ChatCompletionRequestSystemMessageArgs, ChatCompletionRequestToolMessageArgs,
    ChatCompletionRequestUserMessageArgs, ChatCompletionTool, ChatCompletionTools,
    CreateChatCompletionRequestArgs, FunctionCall, FunctionObject,
};
use async_trait::async_trait;
use futures::stream;
use rutsubo_core::{
    api::ProviderHealth,
    events::{StopReason, Usage},
    ids::{ProviderId, ToolCallId},
};
use serde::Deserialize;

const GROQ_BASE: &str = "https://api.groq.com/openai/v1";

// Respuesta de chat de Groq deserializada con un struct propio y laxo: Groq
// añade campos que el enum estricto de async-openai rechaza (p. ej.
// `service_tier: "on_demand"`, `x_groq`, `usage_breakdown`). Solo se extrae lo
// que el adapter necesita; todo lo demás se ignora (`#[serde(default)]`), lo
// que hace al proveedor inmune a extensiones del formato OpenAI de Groq.
#[derive(Deserialize)]
struct GroqResponse {
    #[serde(default)]
    choices: Vec<GroqChoice>,
    #[serde(default)]
    usage: Option<GroqUsage>,
}
#[derive(Deserialize)]
struct GroqChoice {
    message: GroqMessage,
}
#[derive(Deserialize)]
struct GroqMessage {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<GroqToolCall>>,
}
#[derive(Deserialize)]
struct GroqToolCall {
    id: String,
    function: GroqFunction,
}
#[derive(Deserialize)]
struct GroqFunction {
    name: String,
    arguments: String,
}
#[derive(Deserialize)]
struct GroqUsage {
    #[serde(default)]
    prompt_tokens: u32,
    #[serde(default)]
    completion_tokens: u32,
}

pub struct GroqProvider {
    id: ProviderId,
    model: String,
    api_key: String,
    http: reqwest::Client,
}
impl GroqProvider {
    pub fn new(model: impl Into<String>, api_key: String) -> Self {
        let model = model.into();
        Self {
            id: ProviderId(format!("groq:{model}")),
            model,
            api_key,
            http: reqwest::Client::new(),
        }
    }
    fn messages(
        messages: Vec<ChatMessage>,
    ) -> Result<Vec<ChatCompletionRequestMessage>, ProviderError> {
        messages
            .into_iter()
            .map(|m| {
                match m.role.as_str() {
                    "system" => ChatCompletionRequestSystemMessageArgs::default()
                        .content(m.content)
                        .build()
                        .map(ChatCompletionRequestMessage::System),
                    "assistant" => ChatCompletionRequestAssistantMessageArgs::default()
                        .content(m.content)
                        .tool_calls(
                            m.tool_calls
                                .into_iter()
                                .filter_map(|tc| {
                                    tc.provider_call_id.map(|id| {
                                        ChatCompletionMessageToolCalls::Function(
                                            ChatCompletionMessageToolCall {
                                                id,
                                                function: FunctionCall {
                                                    name: tc.tool,
                                                    arguments: tc.args.to_string(),
                                                },
                                            },
                                        )
                                    })
                                })
                                .collect::<Vec<_>>(),
                        )
                        .build()
                        .map(ChatCompletionRequestMessage::Assistant),
                    "tool" => ChatCompletionRequestToolMessageArgs::default()
                        .content(m.content)
                        .tool_call_id(
                            m.provider_tool_call_id
                                .or_else(|| m.tool_call_id.map(|id| id.to_string()))
                                .unwrap_or_default(),
                        )
                        .build()
                        .map(ChatCompletionRequestMessage::Tool),
                    _ => ChatCompletionRequestUserMessageArgs::default()
                        .content(m.content)
                        .build()
                        .map(ChatCompletionRequestMessage::User),
                }
                .map_err(|e| ProviderError::InvalidResponse(e.to_string()))
            })
            .collect()
    }
}
#[async_trait]
impl LlmProvider for GroqProvider {
    fn id(&self) -> ProviderId {
        self.id.clone()
    }
    async fn generate(&self, req: GenerationRequest) -> Result<GenerationStream, ProviderError> {
        if req.cancel.is_cancelled() {
            return Err(ProviderError::Cancelled);
        }
        let tools: Vec<ChatCompletionTools> = req
            .tools
            .into_iter()
            .map(|t| {
                ChatCompletionTools::Function(ChatCompletionTool {
                    function: FunctionObject {
                        name: t.name,
                        description: Some(t.description),
                        parameters: Some(t.parameters),
                        strict: None,
                    },
                })
            })
            .collect();
        // La request se construye con los tipos de async-openai (serializan al
        // formato OpenAI correcto) pero se envía y deserializa a mano para no
        // depender del deserializador estricto de respuesta.
        let request = CreateChatCompletionRequestArgs::default()
            .model(&self.model)
            .messages(Self::messages(req.messages)?)
            .tools(tools)
            .max_completion_tokens(req.max_tokens)
            .temperature(req.temperature)
            .build()
            .map_err(|e| ProviderError::InvalidResponse(e.to_string()))?;

        let http = self
            .http
            .post(format!("{GROQ_BASE}/chat/completions"))
            .bearer_auth(&self.api_key)
            .json(&request)
            .send()
            .await
            .map_err(|e| ProviderError::Transport(e.to_string()))?;

        let status = http.status();
        if status.as_u16() == 429 {
            return Err(ProviderError::RateLimited { retry_after_s: 30 });
        }
        if status.is_server_error() {
            return Err(ProviderError::Transport("Groq no está disponible".into()));
        }
        if !status.is_success() {
            return Err(ProviderError::InvalidResponse(format!(
                "Groq rechazó la petición ({status})"
            )));
        }

        let response: GroqResponse = http
            .json()
            .await
            .map_err(|e| ProviderError::InvalidResponse(format!("respuesta ilegible: {e}")))?;
        let choice = response
            .choices
            .into_iter()
            .next()
            .ok_or_else(|| ProviderError::InvalidResponse("respuesta sin choices".into()))?;
        let mut items = Vec::new();
        if let Some(content) = choice.message.content.filter(|s| !s.is_empty()) {
            items.push(Ok(StreamItem::Delta(content)));
        }
        for call in choice.message.tool_calls.unwrap_or_default() {
            let args = serde_json::from_str(&call.function.arguments).map_err(|_| {
                ProviderError::InvalidResponse("argumentos de herramienta inválidos".into())
            })?;
            items.push(Ok(StreamItem::ToolCall(ToolCallRequest {
                tool_call_id: ToolCallId::new(),
                tool: call.function.name,
                args,
                provider_call_id: Some(call.id),
            })));
        }
        let usage = response
            .usage
            .map(|u| Usage {
                prompt_tokens: u.prompt_tokens,
                completion_tokens: u.completion_tokens,
            })
            .unwrap_or(Usage {
                prompt_tokens: 0,
                completion_tokens: 0,
            });
        items.push(Ok(StreamItem::Done(StopReason::EndTurn, usage)));
        Ok(Box::pin(stream::iter(items)))
    }
    async fn health(&self) -> ProviderHealth {
        ProviderHealth::Ready
    }
}

#[cfg(test)]
mod tests {
    use super::GroqResponse;

    #[test]
    fn deserializa_respuesta_de_groq_con_service_tier_desconocido() {
        // Respuesta real de Groq: incluye `service_tier: "on_demand"`,
        // `reasoning`, `x_groq`, `usage_breakdown` — campos que el enum estricto
        // de async-openai rechazaba. El struct laxo los ignora.
        let raw = r#"{
          "id":"chatcmpl-1","object":"chat.completion","created":1,"model":"qwen/qwen3.6-27b",
          "choices":[{"index":0,"message":{"role":"assistant","reasoning":"voy a escribir",
            "tool_calls":[{"id":"abc","type":"function","function":{
              "name":"write_file","arguments":"{\"path\":\"index.html\",\"content\":\"x\"}"}}]},
            "logprobs":null,"finish_reason":"tool_calls"}],
          "usage":{"prompt_tokens":787,"completion_tokens":183,"total_tokens":970},
          "usage_breakdown":null,"system_fingerprint":"fp_1",
          "x_groq":{"id":"req_1"},"service_tier":"on_demand"
        }"#;
        let parsed: GroqResponse = serde_json::from_str(raw).expect("debe deserializar");
        let msg = &parsed.choices[0].message;
        let call = &msg.tool_calls.as_ref().unwrap()[0];
        assert_eq!(call.function.name, "write_file");
        assert_eq!(parsed.usage.unwrap().completion_tokens, 183);
    }
}
