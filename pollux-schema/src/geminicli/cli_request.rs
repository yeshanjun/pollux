use crate::gemini::GeminiGenerateContentRequest;
use serde::Serialize;

/// Vertex AI `generateContent` upstream envelope that borrows the heavy
/// request body, avoiding deep-clones on every retry attempt.
#[derive(Debug, Serialize)]
pub struct VertexGenerateContentRequest<'a> {
    pub model: &'a str,
    pub project: &'a str,
    pub request: &'a GeminiGenerateContentRequest,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn serializes_with_borrowed_fields() {
        let request: GeminiGenerateContentRequest = serde_json::from_value(json!({
            "contents": [{
                "role": "user",
                "parts": [{"text": "hello"}]
            }]
        }))
        .unwrap();

        let payload = VertexGenerateContentRequest {
            model: "gemini-2.5-flash",
            project: "project-1",
            request: &request,
        };

        let value = serde_json::to_value(&payload).unwrap();
        assert_eq!(value["model"], "gemini-2.5-flash");
        assert_eq!(value["project"], "project-1");
        assert_eq!(value["request"]["contents"][0]["parts"][0]["text"], "hello");
    }
}
