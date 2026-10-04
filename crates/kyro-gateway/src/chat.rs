//! Bounded provider SSE decoder. Only visible content crosses the provisional-output boundary.
use crate::{
    client::{SendFailure, contains_loaded_secret, parse_response},
    config::{Destination, RegisteredModel},
};
use kyro_domain::model::{
    ModelEffectContext, ModelEffectStore, ModelRequest, ModelResponse, reject_recognizable_secrets,
};
use serde_json::{Value, json};
use tokio::time::{Duration, interval};
use uuid::Uuid;

enum Frame {
    Data(Value),
    Done,
}
#[derive(Default)]
struct Decoder {
    buffer: Vec<u8>,
    data: String,
    done: bool,
    bytes: usize,
    frames: usize,
}
impl Decoder {
    fn feed(&mut self, bytes: &[u8]) -> Result<Vec<Frame>, SendFailure> {
        self.bytes += bytes.len();
        if self.bytes > 1_048_576 {
            return Err(SendFailure::ResponseTooLarge);
        }
        self.buffer.extend_from_slice(bytes);
        let mut frames = Vec::new();
        while let Some(end) = self.buffer.iter().position(|b| *b == b'\n') {
            let line = self.buffer.drain(..=end).collect::<Vec<_>>();
            let line = std::str::from_utf8(&line[..line.len() - 1])
                .map_err(|_| SendFailure::InvalidResponse)?
                .trim_end_matches('\r');
            if line.is_empty() && !self.data.is_empty() {
                if self.done {
                    return Err(SendFailure::InvalidResponse);
                }
                self.frames += 1;
                if self.frames > 8192 {
                    return Err(SendFailure::ResponseTooLarge);
                }
                let data = std::mem::take(&mut self.data);
                if data.trim() == "[DONE]" {
                    self.done = true;
                    frames.push(Frame::Done);
                } else {
                    frames.push(Frame::Data(
                        serde_json::from_str(&data).map_err(|_| SendFailure::InvalidResponse)?,
                    ));
                }
            } else if let Some(data) = line.strip_prefix("data:") {
                if !self.data.is_empty() {
                    self.data.push('\n');
                }
                self.data.push_str(data.strip_prefix(' ').unwrap_or(data));
            }
            if self.data.len() > 65536 {
                return Err(SendFailure::ResponseTooLarge);
            }
        }
        if self.buffer.len() > 65536 {
            return Err(SendFailure::ResponseTooLarge);
        }
        Ok(frames)
    }
}

#[derive(Default)]
struct Completion {
    text: String,
    emitted: usize,
    id: Option<String>,
    usage: Option<Value>,
    finish: Option<String>,
}
impl Completion {
    fn accept(
        &mut self,
        value: Value,
        model: &RegisteredModel,
        secret: Option<&str>,
    ) -> Result<(), SendFailure> {
        if secret.is_some_and(|key| contains_loaded_secret(&value, key)) {
            return Err(SendFailure::InvalidResponse);
        }
        let id = value["id"]
            .as_str()
            .filter(|id| kyro_domain::model::valid_provider_request_id(id))
            .ok_or(SendFailure::InvalidResponse)?;
        if value["model"].as_str() != Some(model.registration.model.as_str())
            || self.id.as_deref().is_some_and(|old| old != id)
        {
            return Err(SendFailure::InvalidResponse);
        }
        self.id = Some(id.to_owned());
        let choices = value["choices"]
            .as_array()
            .ok_or(SendFailure::InvalidResponse)?;
        if choices.len() > 1 {
            return Err(SendFailure::InvalidResponse);
        }
        if let Some(choice) = choices.first() {
            if choice["index"].as_u64() != Some(0) || !choice["delta"].is_object() {
                return Err(SendFailure::InvalidResponse);
            }
            let delta = &choice["delta"];
            if delta.get("refusal").is_some_and(|v| !v.is_null())
                || delta
                    .get("tool_calls")
                    .is_some_and(|v| !v.is_null() && v.as_array().is_none_or(|a| !a.is_empty()))
            {
                return Err(SendFailure::InvalidResponse);
            }
            if let Some(content) = delta.get("content").filter(|v| !v.is_null()) {
                let content = content.as_str().ok_or(SendFailure::InvalidResponse)?;
                if self.finish.is_some() && !content.is_empty() {
                    return Err(SendFailure::InvalidResponse);
                }
                self.text.push_str(content);
                if self.text.len() > 32768 {
                    return Err(SendFailure::ResponseTooLarge);
                }
                // Cumulative checks catch strings assembled across JSON/SSE fragments.
                reject_recognizable_secrets(&json!(self.text))
                    .map_err(|_| SendFailure::InvalidResponse)?;
                if secret.is_some_and(|key| self.text.contains(key)) {
                    return Err(SendFailure::InvalidResponse);
                }
            }
            if let Some(reason) = choice.get("finish_reason").filter(|v| !v.is_null()) {
                let reason = reason
                    .as_str()
                    .filter(|r| matches!(*r, "stop" | "length"))
                    .ok_or(SendFailure::InvalidResponse)?;
                if self.finish.is_some() {
                    return Err(SendFailure::InvalidResponse);
                }
                self.finish = Some(reason.to_owned());
            }
        }
        if let Some(usage) = value.get("usage").filter(|v| !v.is_null()) {
            if self.usage.is_some() || self.finish.is_none() {
                return Err(SendFailure::InvalidUsage);
            }
            self.usage = Some(usage.clone());
        }
        Ok(())
    }

    fn safe_end(&self, secret: Option<&str>) -> usize {
        // Withhold any possible secret prefix, including an unfinished token-shaped word.
        let hold = secret.map_or(32, |s| s.len().max(32)).saturating_sub(1);
        let mut end = self.text.len().saturating_sub(hold);
        while !self.text.is_char_boundary(end) {
            end -= 1;
        }
        let word_start = self
            .text
            .char_indices()
            .rev()
            .find(|(_, c)| !(c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')))
            .map_or(0, |(i, c)| i + c.len_utf8());
        let word = &self.text[word_start..];
        if word.starts_with("sk-") || word.starts_with("v1.") {
            end = end.min(word_start);
        }
        end.max(self.emitted)
    }
    async fn flush<S: ModelEffectStore>(
        &mut self,
        end: usize,
        store: &S,
        context: &ModelEffectContext,
        effect: Uuid,
    ) -> Result<(), SendFailure> {
        while self.emitted < end {
            let mut next = (self.emitted + 4096).min(end);
            while !self.text.is_char_boundary(next) {
                next -= 1;
            }
            store
                .append_chat_delta(context, effect, &self.text[self.emitted..next])
                .await
                .map_err(|_| SendFailure::TransportUncertain)?;
            self.emitted = next;
        }
        Ok(())
    }
    fn response(
        &self,
        model: &RegisteredModel,
        destination: &Destination,
        request: &ModelRequest,
    ) -> Result<ModelResponse, SendFailure> {
        if self.finish.is_none()
            || self.usage.is_none()
            || (self.text.trim().is_empty() && self.finish.as_deref() != Some("length"))
        {
            return Err(SendFailure::InvalidResponse);
        }
        let output = json!({"schema_id":model.registration.output_schema_id,"schema_version":model.registration.output_schema_version,
            "data":{"text":self.text,"truncated":self.finish.as_deref()==Some("length")}});
        // The wrapper is constructed here; it was not a JSON response from the model.
        let wrapped = json!({"id":self.id,"model":model.registration.model,"usage":self.usage,
            "choices":[{"finish_reason":"stop","message":{"content":output.to_string()}}]});
        parse_response(
            serde_json::from_value(wrapped).map_err(|_| SendFailure::InvalidResponse)?,
            destination,
            model,
            request.max_output_tokens,
        )
    }
}

pub(crate) async fn read_stream<S: ModelEffectStore>(
    mut response: reqwest::Response,
    destination: &Destination,
    model: &RegisteredModel,
    request: &ModelRequest,
    store: &S,
    context: &ModelEffectContext,
    effect: Uuid,
) -> Result<ModelResponse, SendFailure> {
    if response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_none_or(|v| v.split(';').next() != Some("text/event-stream"))
    {
        return Err(SendFailure::InvalidResponse);
    }
    let secret = destination.secret.as_ref().map(|s| s.expose());
    let mut decoder = Decoder::default();
    let mut completion = Completion::default();
    let mut tick = interval(Duration::from_millis(100));
    loop {
        tokio::select! {
            _=tick.tick()=> {
                store.check_chat_active(context,effect).await.map_err(|_| SendFailure::TransportUncertain)?;
                completion.flush(completion.safe_end(secret),store,context,effect).await?;
            }
            bytes=response.chunk()=> {
                let Some(bytes)=bytes.map_err(|_|SendFailure::TransportUncertain)? else { return Err(SendFailure::TransportUncertain); };
                for frame in decoder.feed(&bytes)? {
                    match frame {
                        Frame::Data(value)=>completion.accept(value,model,secret)?,
                        Frame::Done=> {
                            let result=completion.response(model,destination,request)?;
                            completion.flush(completion.text.len(),store,context,effect).await?;
                            return Ok(result);
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sse_handles_split_utf8_crlf_comments_and_done() {
        let wire = ": ping\r\ndata: {\"text\":\"été 🍋\"}\r\n\r\ndata: [DONE]\r\n\r\n";
        let mut decoder = Decoder::default();
        let mut values = Vec::new();
        for byte in wire.as_bytes() {
            values.extend(decoder.feed(&[*byte]).unwrap());
        }
        assert!(matches!(&values[0],Frame::Data(value) if value["text"]=="été 🍋"));
        assert!(matches!(values[1], Frame::Done));
    }
    #[test]
    fn secret_prefix_and_unicode_tail_are_withheld() {
        let mut completion = Completion {
            text: "a ".repeat(100) + "sk-",
            ..Completion::default()
        };
        assert!(completion.safe_end(None) <= 200);
        completion.text = "🍋".repeat(30);
        assert!(
            completion
                .text
                .is_char_boundary(completion.safe_end(Some("fake-test-key")))
        );
    }
    #[test]
    fn oversized_frame_and_multiple_done_are_rejected() {
        assert!(Decoder::default().feed(&vec![b'x'; 65537]).is_err());
        assert!(
            Decoder::default()
                .feed(b"data: [DONE]\n\ndata: [DONE]\n\n")
                .is_err()
        );
    }

    fn fixture() -> (crate::config::GatewayConfig, ModelRequest) {
        let mut registry: Value =
            serde_json::from_str(include_str!("../../../config/models.synthetic.json")).unwrap();
        let model = &mut registry["destinations"][0]["models"][0];
        model["output_mode"] = json!("text_chat");
        model["output_schema"] = json!({"id":"chat-reply","version":"1","schema":{"type":"object","required":["text","truncated"],"additionalProperties":false,"properties":{"text":{"type":"string","maxLength":32768},"truncated":{"type":"boolean"}}}});
        let config = crate::config::GatewayConfig::from_registry_json(
            &serde_json::to_vec(&registry).unwrap(),
            kyro_domain::Environment::Development,
            true,
            Some("fake-key-for-stream-test"),
        )
        .unwrap();
        let request = ModelRequest {
            destination_id: "synthetic-local".into(),
            model: "synthetic-structured".into(),
            input: kyro_domain::model::ModelInput {
                purpose: kyro_domain::model::ModelPurpose::Conversation,
                categories: [kyro_domain::model::DataCategory::UserRequest]
                    .into_iter()
                    .collect(),
                content: json!({"messages":[{"role":"user","content":"Bonjour"}],"context_tokens":8192}),
            },
            max_output_tokens: 2048,
            deadline_ms: 10000,
        };
        (config, request)
    }
    fn chunk(delta: Value, finish: Value) -> Value {
        json!({"id":"chatcmpl-test","model":"synthetic-structured","choices":[{"index":0,"delta":delta,"finish_reason":finish}]})
    }
    #[test]
    fn completion_requires_usage_and_wraps_truncation_without_reasoning() {
        let (config, request) = fixture();
        let destination = &config.destinations[0];
        let model = &destination.models[0];
        let mut completion = Completion::default();
        completion
            .accept(
                chunk(
                    json!({"content":"été 🍋","reasoning_content":"internal-only"}),
                    Value::Null,
                ),
                model,
                None,
            )
            .unwrap();
        assert!(completion.response(model, destination, &request).is_err());
        completion
            .accept(chunk(json!({}), json!("length")), model, None)
            .unwrap();
        assert!(completion.response(model, destination, &request).is_err());
        completion.accept(json!({"id":"chatcmpl-test","model":"synthetic-structured","choices":[],"usage":{"prompt_tokens":17,"completion_tokens":11,"total_tokens":28}}),model,None).unwrap();
        let response = completion.response(model, destination, &request).unwrap();
        assert_eq!(
            response.output.data,
            json!({"text":"été 🍋","truncated":true})
        );
        assert_eq!(
            response.provider_request_id.as_deref(),
            Some("chatcmpl-test")
        );
        assert!(
            !serde_json::to_string(&response)
                .unwrap()
                .contains("internal-only")
        );
        // A reasoning-only completion can exhaust its token allowance before visible content.
        // Its final receipt still settles the known usage and reports truncation.
        completion.text.clear();
        let empty = completion.response(model, destination, &request).unwrap();
        assert_eq!(empty.output.data, json!({"text":"","truncated":true}));
        completion.finish = Some("stop".into());
        assert!(completion.response(model, destination, &request).is_err());
    }
    #[test]
    fn secrets_split_across_fragments_are_rejected_before_publication() {
        let (config, _) = fixture();
        let model = &config.destinations[0].models[0];
        for secret in [
            "sk-123456789012345678901234567890",
            "fake-key-for-stream-test",
        ] {
            let mut completion = Completion::default();
            let mut rejected = false;
            for c in secret.chars() {
                let result = completion.accept(
                    chunk(json!({"content":c.to_string()}), Value::Null),
                    model,
                    Some(secret),
                );
                if result.is_err() {
                    rejected = true;
                    break;
                }
                assert_eq!(completion.safe_end(Some(secret)), 0);
            }
            assert!(rejected);
        }
    }
    #[test]
    fn receipt_changes_and_content_after_finish_are_rejected() {
        let (config, _) = fixture();
        let model = &config.destinations[0].models[0];
        let mut completion = Completion::default();
        completion
            .accept(
                chunk(json!({"content":"Bonjour"}), json!("stop")),
                model,
                None,
            )
            .unwrap();
        assert!(
            completion
                .accept(chunk(json!({"content":"late"}), Value::Null), model, None)
                .is_err()
        );
        let mut different = chunk(json!({}), Value::Null);
        different["id"] = json!("another-receipt");
        assert!(completion.accept(different, model, None).is_err());
    }
}
