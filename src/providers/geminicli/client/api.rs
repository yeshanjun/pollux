use backon::{ExponentialBuilder, Retryable};
use tracing::error;

pub struct GeminiApi;

const GEMINI_GENERATE_URL: &str = "https://cloudcode-pa.googleapis.com/v1internal:generateContent";
const GEMINI_STREAM_URL: &str =
    "https://cloudcode-pa.googleapis.com/v1internal:streamGenerateContent?alt=sse";

impl GeminiApi {
    pub async fn try_post_cli<T>(
        client: reqwest::Client,
        token: impl AsRef<str>,
        stream: bool,
        retry_policy: ExponentialBuilder,
        body: &T,
    ) -> Result<reqwest::Response, reqwest::Error>
    where
        T: serde::Serialize,
    {
        let url = if stream {
            GEMINI_STREAM_URL
        } else {
            GEMINI_GENERATE_URL
        };

        (|| async {
            let resp = client
                .post(url)
                .bearer_auth(token.as_ref())
                .json(body)
                .send()
                .await?;
            if resp.status().is_server_error() {
                let status = resp.status();
                let err = resp.error_for_status().unwrap_err();
                error!("Gemini CLI server error (will retry): {}", status);
                return Err(err);
            }
            Ok(resp)
        })
        .retry(retry_policy)
        .await
    }
}
