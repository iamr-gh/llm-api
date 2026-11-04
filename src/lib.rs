use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::io::{BufRead, BufReader, Read};
use std::time::Duration;

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
pub const LIMIT: usize = 16 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    Openai,
    Anthropic,
    Xai,
    Gemini,
    Openrouter,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Message {
    pub role: String,
    pub content: Vec<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replay: Option<Value>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub model: String,
    pub messages: Vec<Message>,
    #[serde(default)]
    pub system: String,
    #[serde(default)]
    pub tools: Vec<Value>,
    #[serde(default)]
    pub json_schema: Option<Value>,
    pub max_tokens: u64,
    #[serde(default)]
    pub options: Map<String, Value>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Response {
    pub message: Message,
    pub finish_reason: String,
    pub usage: Value,
}

pub struct Client {
    provider: Provider,
    key: String,
    endpoint: String,
    agent: ureq::Agent,
}

impl Client {
    pub fn new(provider: Provider, key: impl Into<String>) -> Self {
        let endpoint = match provider {
            Provider::Openai => "https://api.openai.com/v1/responses",
            Provider::Xai => "https://api.x.ai/v1/responses",
            Provider::Openrouter => "https://openrouter.ai/api/v1/responses",
            Provider::Anthropic => "https://api.anthropic.com/v1/messages",
            Provider::Gemini => "https://generativelanguage.googleapis.com/v1beta/models",
        };
        Self {
            provider,
            key: key.into(),
            endpoint: endpoint.into(),
            agent: ureq::Agent::config_builder()
                .timeout_global(Some(Duration::from_secs(180)))
                .max_redirects(0)
                .http_status_as_error(false)
                .build()
                .into(),
        }
    }

    pub fn with_endpoint(mut self, endpoint: impl Into<String>) -> Result<Self> {
        let endpoint = endpoint.into();
        let uri: ureq::http::Uri = endpoint.parse()?;
        let authority = uri.authority().ok_or("endpoint needs a host")?;
        let loopback = matches!(uri.host(), Some("127.0.0.1" | "[::1]"));
        if authority.as_str().contains('@')
            || !(uri.scheme_str() == Some("https") || uri.scheme_str() == Some("http") && loopback)
        {
            return Err("endpoint requires HTTPS or loopback HTTP, without userinfo".into());
        }
        self.endpoint = endpoint;
        Ok(self)
    }

    pub fn run(&self, request: &Request, emit: impl FnMut(Value) -> bool) -> Result<Response> {
        let body = encode(self.provider, request)?;
        let url = if self.provider == Provider::Gemini {
            if !request
                .model
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-._".contains(&b))
            {
                return Err("Gemini model must be a model ID, not a path".into());
            }
            format!(
                "{}/{}:streamGenerateContent?alt=sse",
                self.endpoint, request.model
            )
        } else {
            self.endpoint.clone()
        };
        let call = self
            .agent
            .post(url)
            .header("Content-Type", "application/json");
        let call = match self.provider {
            Provider::Gemini => call.header("x-goog-api-key", &self.key),
            Provider::Anthropic => call
                .header("x-api-key", &self.key)
                .header("anthropic-version", "2023-06-01"),
            _ => call.header("Authorization", format!("Bearer {}", self.key)),
        };
        let bytes = serde_json::to_vec(&body)?;
        if bytes.len() > LIMIT {
            return Err("request exceeds 16 MiB".into());
        }
        let mut response = call.send(bytes.as_slice())?;
        if !response.status().is_success() {
            let status = response.status();
            return Err(format!("provider HTTP {status} (body omitted)").into());
        }
        let reader = BufReader::new(response.body_mut().as_reader());
        decode(self.provider, &request.model, reader, emit)
    }
}

fn string<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("missing string {key}").into())
}

fn settings<'a>(body: &'a mut Value, key: &str) -> Result<&'a mut Map<String, Value>> {
    let map = body.as_object_mut().ok_or("expected object")?;
    map.entry(key)
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or_else(|| format!("{key} must be an object").into())
}

fn output(value: &Value) -> String {
    value
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| value.to_string())
}

fn image(block: &Value) -> Result<(&str, &str)> {
    let mime = string(block, "mime_type")?;
    if !mime.starts_with("image/") {
        return Err("image mime_type must start with image/".into());
    }
    Ok((mime, string(block, "data")?))
}

pub fn encode(provider: Provider, request: &Request) -> Result<Value> {
    if request.model.is_empty() || request.max_tokens == 0 {
        return Err("model and positive max_tokens are required".into());
    }
    let mut body = Value::Object(request.options.clone());
    let mut messages = Vec::new();
    let mut names = std::collections::HashMap::new();
    for message in &request.messages {
        for block in &message.content {
            if block["type"] == "tool_call" {
                if message.role != "assistant" || !block["arguments"].is_object() {
                    return Err("tool calls require assistant role and object arguments".into());
                }
                names.insert(string(block, "id")?, string(block, "name")?);
            }
            if block["type"] == "tool_result"
                && (message.role != "user" || block.get("output").is_none())
            {
                return Err("tool results require user role and output".into());
            }
            if block["type"] == "image" && message.role != "user" {
                return Err("image input requires user role".into());
            }
        }
        if !matches!(message.role.as_str(), "user" | "assistant") {
            return Err("role must be user or assistant; use system for instructions".into());
        }
        if let Some(replay) = &message.replay {
            if message.role != "assistant"
                || replay["provider"] != json!(provider)
                || replay["model"] != request.model
            {
                return Err("replay requires original provider/model and assistant role".into());
            }
            let native = replay.get("data").ok_or("missing replay data")?;
            if !matches!(provider, Provider::Anthropic | Provider::Gemini) {
                let items = native.as_array().ok_or("replay data must be an array")?;
                messages.extend(items.clone());
            } else {
                messages.push(native.clone());
            }
            continue;
        }
        let mut parts = Vec::new();
        for block in &message.content {
            let kind = string(block, "type")?;
            let part = match (provider, kind) {
                (_, "reasoning") => {
                    return Err("reasoning history requires its original replay payload".into());
                }
                (
                    Provider::Openai | Provider::Xai | Provider::Openrouter,
                    "tool_call" | "tool_result",
                ) => {
                    let id = string(block, "id")?;
                    let item = if kind == "tool_call" {
                        json!({"type":"function_call","call_id":id,"name":string(block,"name")?,"arguments":block["arguments"].to_string()})
                    } else {
                        json!({"type":"function_call_output","call_id":id,"output":output(&block["output"])})
                    };
                    messages.push(item);
                    continue;
                }
                (Provider::Gemini, "text") => json!({"text":string(block,"text")?}),
                (Provider::Gemini, "image") => {
                    let (mime, data) = image(block)?;
                    json!({"inlineData":{"mimeType":mime,"data":data}})
                }
                (Provider::Gemini, "tool_call") => {
                    json!({"functionCall":{"id":string(block,"id")?,"name":string(block,"name")?,"args":block["arguments"]}})
                }
                (Provider::Gemini, "tool_result") => {
                    let id = string(block, "id")?;
                    let name = names.get(id).ok_or("tool result has no preceding call")?;
                    json!({"functionResponse":{"id":id,"name":name,"response":{"result":block["output"]}}})
                }
                (Provider::Anthropic, "text") => {
                    json!({"type":"text","text":string(block,"text")?})
                }
                (Provider::Anthropic, "image") => {
                    let (mime, data) = image(block)?;
                    json!({"type":"image","source":{"type":"base64","media_type":mime,"data":data}})
                }
                (Provider::Anthropic, "tool_call") => {
                    json!({"type":"tool_use","id":string(block,"id")?,"name":string(block,"name")?,"input":block["arguments"]})
                }
                (Provider::Anthropic, "tool_result") => {
                    json!({"type":"tool_result","tool_use_id":string(block,"id")?,"content":output(&block["output"]),"is_error":block["is_error"].as_bool().unwrap_or(false)})
                }
                (_, "text") => {
                    json!({"type":if message.role == "assistant" {"output_text"} else {"input_text"},"text":string(block,"text")?})
                }
                (_, "image") => {
                    let (mime, data) = image(block)?;
                    let url = format!("data:{mime};base64,{data}");
                    json!({"type":"input_image","image_url":url})
                }
                _ => return Err(format!("unsupported content block {kind}").into()),
            };
            if !matches!(provider, Provider::Anthropic | Provider::Gemini) {
                messages.push(json!({"role":message.role,"content":[part]}));
            } else {
                parts.push(part);
            }
        }
        if !parts.is_empty() {
            let native = match provider {
                Provider::Gemini => {
                    json!({"role":if message.role == "assistant" {"model"} else {"user"},"parts":parts})
                }
                _ => json!({"role":message.role,"content":parts}),
            };
            messages.push(native);
        }
    }
    let mut tools = Vec::new();
    for tool in &request.tools {
        let name = string(tool, "name")?;
        let schema = tool
            .get("parameters")
            .filter(|v| v.is_object())
            .ok_or("tool parameters must be an object")?;
        let description = tool["description"].as_str().unwrap_or("");
        tools.push(match provider {
            Provider::Anthropic => json!({"name":name,"description":description,"input_schema":schema}),
            Provider::Gemini => json!({"name":name,"description":description,"parameters":schema}),
            _ => json!({"type":"function","name":name,"description":description,"parameters":schema,"strict":false}),
        });
    }
    match provider {
        Provider::Gemini => {
            body["contents"] = json!(messages);
            if !request.system.is_empty() {
                body["systemInstruction"] = json!({"parts":[{"text":request.system}]});
            }
            let config = settings(&mut body, "generationConfig")?;
            config.insert("maxOutputTokens".into(), json!(request.max_tokens));
            if let Some(schema) = &request.json_schema {
                config.insert("responseMimeType".into(), json!("application/json"));
                config.insert("responseJsonSchema".into(), schema.clone());
            }
            if !tools.is_empty() {
                body["tools"] = json!([{"functionDeclarations":tools}]);
            }
        }
        Provider::Anthropic => {
            body["messages"] = json!(messages);
            body["system"] = json!(request.system);
            body["max_tokens"] = json!(request.max_tokens);
            if let Some(schema) = &request.json_schema {
                settings(&mut body, "output_config")?.insert(
                    "format".into(),
                    json!({"type":"json_schema","schema":schema}),
                );
            }
            body["tools"] = json!(tools);
        }
        _ => {
            body["input"] = json!(messages);
            body["instructions"] = json!(request.system);
            body["max_output_tokens"] = json!(request.max_tokens);
            body["store"] = json!(false);
            body["include"] = json!(["reasoning.encrypted_content"]);
            if let Some(schema) = &request.json_schema {
                settings(&mut body, "text")?.insert(
                    "format".into(),
                    json!({"type":"json_schema","name":"result","strict":true,"schema":schema}),
                );
            }
            body["tools"] = json!(tools);
        }
    }
    if provider != Provider::Gemini {
        body["model"] = json!(request.model);
        body["stream"] = json!(true);
    }
    Ok(body)
}

fn append(target: &mut Value, key: &str, text: &str) {
    let previous = target[key].as_str().unwrap_or("");
    target[key] = json!(format!("{previous}{text}"));
}

pub fn decode(
    provider: Provider,
    model: &str,
    mut reader: impl BufRead,
    mut emit: impl FnMut(Value) -> bool,
) -> Result<Response> {
    let mut native = json!({});
    let mut usage = json!({});
    let mut stop = String::new();
    let mut done = false;
    let mut data = String::new();
    let mut total = 0;
    loop {
        let mut line = String::new();
        let read = reader
            .by_ref()
            .take((LIMIT + 1) as u64)
            .read_line(&mut line)?;
        total += read;
        if total > LIMIT {
            return Err("stream exceeds 16 MiB".into());
        }
        if read != 0 && !line.trim_end_matches(['\r', '\n']).is_empty() {
            if let Some(value) = line.trim_start_matches('\u{feff}').strip_prefix("data:") {
                data.push_str(
                    value
                        .strip_prefix(' ')
                        .unwrap_or(value)
                        .trim_end_matches(['\r', '\n']),
                );
                data.push('\n');
            }
            continue;
        }
        if data.is_empty() {
            if read == 0 {
                break;
            }
            continue;
        }
        let event: Value = serde_json::from_str(&data)?;
        data.clear();
        if event.get("error").is_some()
            || event["type"] == "error"
            || event["type"] == "response.failed"
        {
            return Err("provider reported a stream error (payload omitted)".into());
        }
        let mut events = Vec::new();
        match provider {
            Provider::Openai | Provider::Xai | Provider::Openrouter => {
                let index = event["output_index"].as_u64().unwrap_or(0);
                match event["type"].as_str().unwrap_or("") {
                    "response.output_text.delta" | "response.reasoning_text.delta" | "response.reasoning_summary_text.delta" => {
                        events.push(json!({"type":if event["type"] == "response.output_text.delta" {"text_delta"} else {"reasoning_delta"},"index":index,"delta":event["delta"]}));
                    }
                    "response.output_item.added" if event["item"]["type"] == "function_call" => events.push(json!({"type":"tool_call_start","index":index,"id":event["item"]["call_id"],"name":event["item"]["name"]})),
                    "response.function_call_arguments.delta" => events.push(json!({"type":"tool_arguments_delta","index":index,"delta":event["delta"]})),
                    "response.completed" | "response.incomplete" => {
                        native = event["response"].clone();
                        native["output"].as_array().ok_or("response has no output array")?;
                        usage = native["usage"].clone();
                        stop = if event["type"] == "response.incomplete" {
                            native["incomplete_details"]["reason"].as_str().unwrap_or("incomplete")
                        } else { "stop" }.into();
                        done = true;
                    }
                    _ => {}
                }
            }
            Provider::Anthropic => {
                let index = event["index"].as_u64().unwrap_or(0) as usize;
                if index > 4096 {
                    return Err("content index too large".into());
                }
                match event["type"].as_str().unwrap_or("") {
                    "message_start" => {
                        native = event["message"].clone();
                        usage = native["usage"].clone();
                    }
                    "content_block_start" => {
                        event["content_block"]
                            .as_object()
                            .ok_or("invalid content block")?;
                        let blocks = native
                            .get_mut("content")
                            .and_then(Value::as_array_mut)
                            .ok_or("missing message_start")?;
                        blocks.resize(blocks.len().max(index + 1), Value::Null);
                        blocks[index] = event["content_block"].clone();
                        if blocks[index]["type"] == "tool_use" {
                            events.push(json!({"type":"tool_call_start","index":index,"id":blocks[index]["id"],"name":blocks[index]["name"]}));
                        }
                    }
                    "content_block_delta" => {
                        let block = native
                            .pointer_mut(&format!("/content/{index}"))
                            .filter(|value| value.is_object())
                            .ok_or("delta without content block")?;
                        for (key, source, kind) in [
                            ("text", "text", "text_delta"),
                            ("thinking", "thinking", "reasoning_delta"),
                            ("_arguments", "partial_json", "tool_arguments_delta"),
                            ("signature", "signature", ""),
                        ] {
                            if let Some(text) = event["delta"][source].as_str() {
                                append(block, key, text);
                                if !kind.is_empty() {
                                    events.push(json!({"type":kind,"index":index,"delta":text}));
                                }
                            }
                        }
                    }
                    "message_delta" => {
                        stop = string(&event["delta"], "stop_reason")?.into();
                        if let Some(delta) = event["usage"].as_object() {
                            usage
                                .as_object_mut()
                                .ok_or("invalid message usage")?
                                .extend(delta.clone());
                        }
                    }
                    "message_stop" => done = true,
                    _ => {}
                }
            }
            Provider::Gemini => {
                if event["usageMetadata"].is_object() {
                    usage = event["usageMetadata"].clone();
                }
                let candidate = &event["candidates"][0];
                if let Some(parts) = candidate["content"]["parts"].as_array() {
                    if !native["parts"].is_array() {
                        native = json!({"role":"model","parts":[]});
                    }
                    let blocks = native["parts"].as_array_mut().unwrap();
                    for part in parts {
                        let index = blocks.len();
                        if let Some(text) = part["text"].as_str() {
                            events.push(json!({"type":if part["thought"] == true {"reasoning_delta"} else {"text_delta"},"index":index,"delta":text}));
                        }
                        if part["functionCall"].is_object() {
                            let call = &part["functionCall"];
                            events.push(json!({"type":"tool_call_start","index":index,"id":call["id"].as_str().map(str::to_owned).unwrap_or(format!("call_{index}")),"name":call["name"]}));
                            events.push(json!({"type":"tool_arguments_delta","index":index,"delta":call["args"].to_string()}));
                        }
                        blocks.push(part.clone());
                    }
                }
                if let Some(reason) = candidate["finishReason"].as_str() {
                    stop = reason.into();
                    done = true;
                }
                if event["promptFeedback"]["blockReason"].is_string() {
                    return Err("Gemini blocked the prompt".into());
                }
            }
        }
        for event in events {
            if !emit(event) {
                return Err("cancelled by event callback".into());
            }
        }
        if done && provider != Provider::Gemini {
            break;
        }
    }
    if !done || stop.is_empty() {
        return Err("incomplete stream: missing terminal event".into());
    }
    finish(provider, model, native, usage, stop)
}

fn finish(
    provider: Provider,
    model: &str,
    mut native: Value,
    usage: Value,
    mut stop: String,
) -> Result<Response> {
    let mut content = Vec::new();
    let mut text = |kind: &str, value: &Value| {
        if let Some(value) = value.as_str().filter(|s| !s.is_empty()) {
            content.push(json!({"type":kind,"text":value}));
        }
    };
    for item in native["output"].as_array().into_iter().flatten() {
        let kind = match item["type"].as_str() {
            Some("reasoning") => "reasoning",
            _ => "text",
        };
        for (field, kind) in [("content", kind), ("summary", "reasoning")] {
            if let Some(parts) = item[field].as_array() {
                for part in parts {
                    text(kind, &part["text"]);
                    text("refusal", &part["refusal"]);
                }
            }
        }
    }
    let entries = match provider {
        Provider::Openai | Provider::Xai | Provider::Openrouter => native["output"].as_array_mut(),
        Provider::Anthropic => native.get_mut("content").and_then(Value::as_array_mut),
        Provider::Gemini => native.get_mut("parts").and_then(Value::as_array_mut),
    };
    if let Some(entries) = entries {
        for (index, item) in entries.iter_mut().enumerate() {
            if let Some(arguments) = item.as_object_mut().and_then(|o| o.remove("_arguments")) {
                item["input"] =
                    serde_json::from_str(arguments.as_str().ok_or("invalid tool JSON delta")?)?;
            }
            if provider == Provider::Anthropic {
                if item["type"] == "text" {
                    content.push(json!({"type":"text","text":item["text"]}));
                }
                if item["type"] == "thinking" {
                    content.push(json!({"type":"reasoning","text":item["thinking"]}));
                }
            }
            if provider == Provider::Gemini && item["text"].is_string() {
                content.push(json!({"type":if item["thought"] == true {"reasoning"} else {"text"},"text":item["text"]}));
            }
            let call = match provider {
                Provider::Openai | Provider::Xai | Provider::Openrouter
                    if item["type"] == "function_call" =>
                {
                    Some((
                        &item["call_id"],
                        &item["name"],
                        serde_json::from_str::<Value>(string(item, "arguments")?)?,
                    ))
                }
                Provider::Anthropic if item["type"] == "tool_use" => {
                    Some((&item["id"], &item["name"], item["input"].clone()))
                }
                Provider::Gemini if item["functionCall"].is_object() => Some((
                    &item["functionCall"]["id"],
                    &item["functionCall"]["name"],
                    item["functionCall"]["args"].clone(),
                )),
                _ => None,
            };
            if let Some((id, name, arguments)) = call {
                if !arguments.is_object() || !name.is_string() {
                    return Err("invalid function call".into());
                }
                content.push(json!({"type":"tool_call","id":id.as_str().map(str::to_owned).unwrap_or(format!("call_{index}")),"name":name,"arguments":arguments}));
            }
        }
    }
    stop = match stop.as_str() {
        "end_turn" | "STOP" => "stop".into(),
        "max_tokens" | "max_output_tokens" | "MAX_TOKENS" => "length".into(),
        "tool_use" => "tool_calls".into(),
        _ => stop,
    };
    if stop == "stop" && content.iter().any(|b| b["type"] == "tool_call") {
        stop = "tool_calls".into();
    }
    let normalized_usage = match provider {
        Provider::Gemini => {
            json!({"input_tokens":usage["promptTokenCount"],"output_tokens":usage["candidatesTokenCount"],"cached_tokens":usage["cachedContentTokenCount"],"reasoning_tokens":usage["thoughtsTokenCount"]})
        }
        _ => {
            json!({"input_tokens":usage["input_tokens"],"output_tokens":usage["output_tokens"],"cached_tokens":if provider == Provider::Anthropic {&usage["cache_read_input_tokens"]} else {&usage["input_tokens_details"]["cached_tokens"]},"reasoning_tokens":usage["output_tokens_details"]["reasoning_tokens"]})
        }
    };
    let data = if !matches!(provider, Provider::Anthropic | Provider::Gemini) {
        native["output"].clone()
    } else if provider == Provider::Anthropic {
        json!({"role":"assistant","content":native["content"]})
    } else {
        native
    };
    Ok(Response {
        message: Message {
            role: "assistant".into(),
            content,
            replay: Some(json!({"provider":provider,"model":model,"data":data})),
        },
        finish_reason: stop,
        usage: json!({"normalized":normalized_usage,"native":usage}),
    })
}
