use std::sync::Arc;

use super::{ChatLocation, copilot_request_headers};
use anyhow::{Result, anyhow};
use futures::{AsyncBufReadExt, AsyncReadExt, StreamExt, io::BufReader, stream::BoxStream};
use http_client::{AsyncBody, HttpClient, HttpRequestExt, Method, Request as HttpRequest};
use serde::{Deserialize, Serialize};
use serde_json::Value;
pub use settings::OpenAiReasoningEffort as ReasoningEffort;

#[derive(Serialize, Debug)]
pub struct Request {
    pub model: String,
    pub input: Vec<ResponseInputItem>,
    #[serde(default)]
    pub stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<ToolDefinition>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<ToolChoice>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<ReasoningConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub include: Option<Vec<ResponseIncludable>>,
    pub store: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "snake_case")]
pub enum ResponseIncludable {
    #[serde(rename = "reasoning.encrypted_content")]
    ReasoningEncryptedContent,
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolDefinition {
    Function {
        name: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        description: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        parameters: Option<Value>,
        #[serde(skip_serializing_if = "Option::is_none")]
        strict: Option<bool>,
    },
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(rename_all = "lowercase")]
pub enum ToolChoice {
    Auto,
    Required,
    None,
    #[serde(untagged)]
    Other(ToolDefinition),
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(rename_all = "lowercase")]
pub enum ReasoningSummary {
    Auto,
    Concise,
    Detailed,
}

#[derive(Serialize, Debug)]
pub struct ReasoningConfig {
    pub effort: ReasoningEffort,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<ReasoningSummary>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
#[serde(rename_all = "snake_case")]
pub enum ResponseImageDetail {
    Low,
    High,
    #[default]
    Auto,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ResponseInputContent {
    InputText {
        text: String,
    },
    OutputText {
        text: String,
    },
    InputImage {
        #[serde(skip_serializing_if = "Option::is_none")]
        image_url: Option<String>,
        #[serde(default)]
        detail: ResponseImageDetail,
    },
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "snake_case")]
pub enum ItemStatus {
    InProgress,
    Completed,
    Incomplete,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(untagged)]
pub enum ResponseFunctionOutput {
    Text(String),
    Content(Vec<ResponseInputContent>),
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ResponseInputItem {
    Message {
        role: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        content: Option<Vec<ResponseInputContent>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        status: Option<String>,
    },
    FunctionCall {
        call_id: String,
        name: String,
        arguments: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        status: Option<ItemStatus>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        thought_signature: Option<String>,
    },
    FunctionCallOutput {
        call_id: String,
        output: ResponseFunctionOutput,
        #[serde(skip_serializing_if = "Option::is_none")]
        status: Option<ItemStatus>,
    },
    Reasoning {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        summary: Vec<ResponseReasoningItem>,
        encrypted_content: String,
    },
}

#[derive(Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum IncompleteReason {
    #[serde(rename = "max_output_tokens")]
    MaxOutputTokens,
    #[serde(rename = "content_filter")]
    ContentFilter,
    #[serde(other)]
    Unknown,
}

#[derive(Deserialize, Debug, Clone)]
pub struct IncompleteDetails {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<IncompleteReason>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ResponseReasoningItem {
    #[serde(rename = "type")]
    pub kind: String,
    pub text: String,
}

#[derive(Deserialize, Debug)]
#[serde(tag = "type")]
pub enum StreamEvent {
    #[serde(rename = "error")]
    GenericError { error: ResponseError },

    #[serde(rename = "response.created")]
    Created { response: Response },

    #[serde(rename = "response.output_item.added")]
    OutputItemAdded {
        output_index: usize,
        #[serde(default)]
        sequence_number: Option<u64>,
        item: ResponseOutputItem,
    },

    #[serde(rename = "response.output_text.delta")]
    OutputTextDelta {
        item_id: String,
        output_index: usize,
        delta: String,
    },

    #[serde(rename = "response.output_item.done")]
    OutputItemDone {
        output_index: usize,
        #[serde(default)]
        sequence_number: Option<u64>,
        item: ResponseOutputItem,
    },

    #[serde(rename = "response.incomplete")]
    Incomplete { response: Response },

    #[serde(rename = "response.completed")]
    Completed { response: Response },

    #[serde(rename = "response.failed")]
    Failed { response: Response },

    #[serde(other)]
    Unknown,
}

#[derive(Deserialize, Debug, Clone)]
pub struct ResponseError {
    pub code: String,
    pub message: String,
}

#[derive(Deserialize, Debug, Default, Clone)]
pub struct Response {
    pub id: Option<String>,
    pub status: Option<String>,
    pub usage: Option<ResponseUsage>,
    pub output: Vec<ResponseOutputItem>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub incomplete_details: Option<IncompleteDetails>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ResponseError>,
}

#[derive(Deserialize, Debug, Default, Clone)]
pub struct ResponseUsage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
}

#[derive(Deserialize, Debug, Clone)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ResponseOutputItem {
    Message {
        id: String,
        role: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        content: Option<Vec<ResponseOutputContent>>,
    },
    FunctionCall {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        call_id: String,
        name: String,
        arguments: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        status: Option<ItemStatus>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        thought_signature: Option<String>,
    },
    Reasoning {
        id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        summary: Option<Vec<ResponseReasoningItem>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        encrypted_content: Option<String>,
    },
}

#[derive(Deserialize, Debug, Clone)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ResponseOutputContent {
    OutputText { text: String },
    Refusal { refusal: String },
}

pub async fn stream_response(
    client: Arc<dyn HttpClient>,
    oauth_token: String,
    api_url: String,
    request: Request,
    is_user_initiated: bool,
    location: ChatLocation,
) -> Result<BoxStream<'static, Result<StreamEvent>>> {
    let is_vision_request = request.input.iter().any(|item| match item {
        ResponseInputItem::Message {
            content: Some(parts),
            ..
        } => parts
            .iter()
            .any(|p| matches!(p, ResponseInputContent::InputImage { .. })),
        _ => false,
    });

    let request_builder = copilot_request_headers(
        HttpRequest::builder().method(Method::POST).uri(&api_url),
        &oauth_token,
        Some(is_user_initiated),
        Some(location),
    )
    .when(is_vision_request, |builder| {
        builder.header("Copilot-Vision-Request", "true")
    });

    let is_streaming = request.stream;
    let json = serde_json::to_string(&request)?;
    let request = request_builder.body(AsyncBody::from(json))?;
    let mut response = client.send(request).await?;

    if !response.status().is_success() {
        let mut body = String::new();
        response.body_mut().read_to_string(&mut body).await?;
        anyhow::bail!("Failed to connect to API: {} {}", response.status(), body);
    }

    if is_streaming {
        let reader = BufReader::new(response.into_body());
        Ok(reader
            .lines()
            .filter_map(|line| async move {
                match line {
                    Ok(line) => {
                        let line = line.strip_prefix("data: ")?;
                        if line.starts_with("[DONE]") || line.is_empty() {
                            return None;
                        }

                        match serde_json::from_str::<StreamEvent>(line) {
                            Ok(event) => Some(Ok(event)),
                            Err(error) => {
                                log::error!(
                                    "Failed to parse Copilot responses stream event: `{}`\nResponse: `{}`",
                                    error,
                                    line,
                                );
                                Some(Err(anyhow!(error)))
                            }
                        }
                    }
                    Err(error) => Some(Err(anyhow!(error))),
                }
            })
            .boxed())
    } else {
        // Simulate streaming this makes the mapping of this function return more straight-forward to handle if all callers assume it streams.
        // Removes the need of having a method to map StreamEvent and another to map Response to a LanguageCompletionEvent
        let mut body = String::new();
        response.body_mut().read_to_string(&mut body).await?;

        match serde_json::from_str::<Response>(&body) {
            Ok(response) => {
                let events = vec![StreamEvent::Created {
                    response: response.clone(),
                }];

                let mut all_events = events;
                for (output_index, item) in response.output.iter().enumerate() {
                    all_events.push(StreamEvent::OutputItemAdded {
                        output_index,
                        sequence_number: None,
                        item: item.clone(),
                    });

                    if let ResponseOutputItem::Message {
                        id,
                        content: Some(content),
                        ..
                    } = item
                    {
                        for part in content {
                            if let ResponseOutputContent::OutputText { text } = part {
                                all_events.push(StreamEvent::OutputTextDelta {
                                    item_id: id.clone(),
                                    output_index,
                                    delta: text.clone(),
                                });
                            }
                        }
                    }

                    all_events.push(StreamEvent::OutputItemDone {
                        output_index,
                        sequence_number: None,
                        item: item.clone(),
                    });
                }

                let final_event = if response.error.is_some() {
                    StreamEvent::Failed { response }
                } else if response.incomplete_details.is_some() {
                    StreamEvent::Incomplete { response }
                } else {
                    StreamEvent::Completed { response }
                };
                all_events.push(final_event);

                Ok(futures::stream::iter(all_events.into_iter().map(Ok)).boxed())
            }
            Err(error) => {
                log::error!(
                    "Failed to parse Copilot non-streaming response: `{}`\nResponse: `{}`",
                    error,
                    body,
                );
                Err(anyhow!(error))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tool_choice_required_serializes_as_required() {
        // Regression test: ToolChoice::Required must serialize as "required" (not "any")
        // for OpenAI Responses API. Reverting the rename would break this.
        assert_eq!(
            serde_json::to_string(&ToolChoice::Required).unwrap(),
            "\"required\""
        );
        assert_eq!(
            serde_json::to_string(&ToolChoice::Auto).unwrap(),
            "\"auto\""
        );
        assert_eq!(
            serde_json::to_string(&ToolChoice::None).unwrap(),
            "\"none\""
        );
    }

    #[test]
    fn test_reasoning_effort_all_variants() {
        assert_eq!(
            serde_json::to_string(&ReasoningEffort::Low).unwrap(),
            "\"low\""
        );
        assert_eq!(
            serde_json::to_string(&ReasoningEffort::Medium).unwrap(),
            "\"medium\""
        );
        assert_eq!(
            serde_json::to_string(&ReasoningEffort::High).unwrap(),
            "\"high\""
        );
    }

    #[test]
    fn test_reasoning_summary_all_variants() {
        assert_eq!(
            serde_json::to_string(&ReasoningSummary::Auto).unwrap(),
            "\"auto\""
        );
        assert_eq!(
            serde_json::to_string(&ReasoningSummary::Concise).unwrap(),
            "\"concise\""
        );
        assert_eq!(
            serde_json::to_string(&ReasoningSummary::Detailed).unwrap(),
            "\"detailed\""
        );
    }

    #[test]
    fn test_response_includable_serialization() {
        let includables = vec![ResponseIncludable::ReasoningEncryptedContent];
        let json = serde_json::to_value(&includables).unwrap();

        assert_eq!(json[0], "reasoning.encrypted_content");
    }

    // --- Error handling tests (Phase 3) ---

    #[test]
    fn test_stream_event_generic_error_parsing() {
        let json = r#"{
            "type": "error",
            "error": {
                "code": "401",
                "message": "Unauthorized: Invalid token"
            }
        }"#;

        let event: StreamEvent = serde_json::from_str(json).unwrap();

        match event {
            StreamEvent::GenericError { error } => {
                assert_eq!(error.code, "401");
                assert_eq!(error.message, "Unauthorized: Invalid token");
            }
            _ => panic!("Expected GenericError, got {:?}", event),
        }
    }

    #[test]
    fn test_stream_event_failed_with_error_parsing() {
        let json = r#"{
            "type": "response.failed",
            "response": {
                "id": "resp_123",
                "status": "failed",
                "error": {
                    "code": "429",
                    "message": "Rate limit exceeded"
                },
                "output": []
            }
        }"#;

        let event: StreamEvent = serde_json::from_str(json).unwrap();

        match event {
            StreamEvent::Failed { response } => {
                assert_eq!(response.status, Some("failed".to_string()));
                let error = response.error.unwrap();
                assert_eq!(error.code, "429");
                assert_eq!(error.message, "Rate limit exceeded");
            }
            _ => panic!("Expected Failed, got {:?}", event),
        }
    }

    #[test]
    fn test_stream_event_incomplete_max_tokens_parsing() {
        let json = r#"{
            "type": "response.incomplete",
            "response": {
                "id": "resp_456",
                "status": "incomplete",
                "incomplete_details": {
                    "reason": "max_output_tokens"
                },
                "usage": {
                    "input_tokens": 100,
                    "output_tokens": 4096,
                    "total_tokens": 4196
                },
                "output": []
            }
        }"#;

        let event: StreamEvent = serde_json::from_str(json).unwrap();

        match event {
            StreamEvent::Incomplete { response } => {
                let details = response.incomplete_details.unwrap();
                assert_eq!(details.reason, Some(IncompleteReason::MaxOutputTokens));
                let usage = response.usage.unwrap();
                assert_eq!(usage.output_tokens, Some(4096));
            }
            _ => panic!("Expected Incomplete, got {:?}", event),
        }
    }

    #[test]
    fn test_stream_event_incomplete_content_filter_parsing() {
        let json = r#"{
            "type": "response.incomplete",
            "response": {
                "incomplete_details": {
                    "reason": "content_filter"
                },
                "output": []
            }
        }"#;

        let event: StreamEvent = serde_json::from_str(json).unwrap();

        match event {
            StreamEvent::Incomplete { response } => {
                let details = response.incomplete_details.unwrap();
                assert_eq!(details.reason, Some(IncompleteReason::ContentFilter));
            }
            _ => panic!("Expected Incomplete, got {:?}", event),
        }
    }

    #[test]
    fn test_stream_event_unknown_type_falls_back() {
        let json = r#"{
            "type": "response.some_future_event",
            "data": "whatever"
        }"#;

        let event: StreamEvent = serde_json::from_str(json).unwrap();
        assert!(matches!(event, StreamEvent::Unknown));
    }

    #[test]
    fn test_incomplete_reason_unknown_variant() {
        let json = r#"{
            "type": "response.incomplete",
            "response": {
                "incomplete_details": {
                    "reason": "some_new_reason"
                },
                "output": []
            }
        }"#;

        let event: StreamEvent = serde_json::from_str(json).unwrap();

        match event {
            StreamEvent::Incomplete { response } => {
                let details = response.incomplete_details.unwrap();
                assert_eq!(details.reason, Some(IncompleteReason::Unknown));
            }
            _ => panic!("Expected Incomplete"),
        }
    }

    #[test]
    fn test_response_error_parsing() {
        let json = r#"{
            "code": "400",
            "message": "Invalid request: model not found"
        }"#;

        let error: ResponseError = serde_json::from_str(json).unwrap();

        assert_eq!(error.code, "400");
        assert_eq!(error.message, "Invalid request: model not found");
    }

    #[test]
    fn test_malformed_json_returns_error() {
        let json = r#"{"type": "response.completed", "response": {not valid json}"#;

        let result: Result<StreamEvent, _> = serde_json::from_str(json);
        assert!(result.is_err());
    }

    #[test]
    fn test_empty_json_object_returns_error() {
        let json = r#"{}"#;

        let result: Result<StreamEvent, _> = serde_json::from_str(json);
        assert!(
            result.is_err(),
            "Empty object should fail without 'type' field"
        );
    }

    #[test]
    fn test_stream_event_completed_with_usage() {
        let json = r#"{
            "type": "response.completed",
            "response": {
                "id": "resp_789",
                "status": "completed",
                "usage": {
                    "input_tokens": 50,
                    "output_tokens": 150,
                    "total_tokens": 200
                },
                "output": []
            }
        }"#;

        let event: StreamEvent = serde_json::from_str(json).unwrap();

        match event {
            StreamEvent::Completed { response } => {
                let usage = response.usage.unwrap();
                assert_eq!(usage.input_tokens, Some(50));
                assert_eq!(usage.output_tokens, Some(150));
            }
            _ => panic!("Expected Completed"),
        }
    }

    #[test]
    fn test_stream_event_with_missing_optional_fields() {
        let json = r#"{
            "type": "response.completed",
            "response": {
                "output": []
            }
        }"#;

        let event: StreamEvent = serde_json::from_str(json).unwrap();

        match event {
            StreamEvent::Completed { response } => {
                assert!(response.id.is_none());
                assert!(response.status.is_none());
                assert!(response.usage.is_none());
                assert!(response.error.is_none());
            }
            _ => panic!("Expected Completed"),
        }
    }
}
