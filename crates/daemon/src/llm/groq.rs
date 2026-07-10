//! Cliente OpenAI-compatible de Groq; la clave queda encapsulada aquí.
use super::{
    ChatMessage, GenerationRequest, GenerationStream, LlmProvider, ProviderError, StreamItem,
    ToolCallRequest,
};
use async_openai::{
    Client,
    config::OpenAIConfig,
    error::OpenAIError,
    types::chat::{
        ChatCompletionMessageToolCall, ChatCompletionMessageToolCalls,
        ChatCompletionRequestAssistantMessageArgs, ChatCompletionRequestMessage,
        ChatCompletionRequestSystemMessageArgs, ChatCompletionRequestToolMessageArgs,
        ChatCompletionRequestUserMessageArgs, ChatCompletionTool, ChatCompletionTools,
        CreateChatCompletionRequestArgs, FunctionCall, FunctionObject,
    },
};
use async_trait::async_trait;
use futures::stream;
use rutsubo_core::{
    api::ProviderHealth,
    events::{StopReason, Usage},
    ids::{ProviderId, ToolCallId},
};

const GROQ_BASE: &str = "https://api.groq.com/openai/v1";
pub struct GroqProvider {
    id: ProviderId,
    model: String,
    client: Client<OpenAIConfig>,
}
impl GroqProvider {
    pub fn new(model: impl Into<String>, api_key: String) -> Self {
        let model = model.into();
        let client = Client::with_config(
            OpenAIConfig::new()
                .with_api_base(GROQ_BASE)
                .with_api_key(api_key),
        );
        Self {
            id: ProviderId(format!("groq:{model}")),
            model,
            client,
        }
    }
    fn map_error(err: OpenAIError) -> ProviderError {
        match err {
            OpenAIError::ApiError(response) if response.status_code.as_u16() == 429 => {
                ProviderError::RateLimited { retry_after_s: 30 }
            }
            OpenAIError::ApiError(response) if response.status_code.is_server_error() => {
                ProviderError::Transport("Groq no está disponible".into())
            }
            OpenAIError::ApiError(response) if response.status_code.as_u16() == 400 => {
                ProviderError::InvalidResponse("Groq rechazó el contexto".into())
            }
            other => ProviderError::Transport(other.to_string()),
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
        let request = CreateChatCompletionRequestArgs::default()
            .model(&self.model)
            .messages(Self::messages(req.messages)?)
            .tools(tools)
            .max_completion_tokens(req.max_tokens)
            .temperature(req.temperature)
            .build()
            .map_err(|e| ProviderError::InvalidResponse(e.to_string()))?;
        let response = self
            .client
            .chat()
            .create(request)
            .await
            .map_err(Self::map_error)?;
        let choice = response
            .choices
            .into_iter()
            .next()
            .ok_or_else(|| ProviderError::InvalidResponse("respuesta sin choices".into()))?;
        let mut items = Vec::new();
        if let Some(content) = choice.message.content.filter(|s| !s.is_empty()) {
            items.push(Ok(StreamItem::Delta(content)));
        }
        if let Some(calls) = choice.message.tool_calls {
            for call in calls {
                if let ChatCompletionMessageToolCalls::Function(call) = call {
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
            }
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
