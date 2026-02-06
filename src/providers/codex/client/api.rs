use backon::{ExponentialBuilder, Retryable};

use crate::providers::UPSTREAM_BODY_PREVIEW_CHARS;
use crate::providers::manifest::CodexLease;
use pollux_schema::CodexRequestBody;

pub struct CodexApi;

impl CodexApi {
    pub fn build_codex_request(
        client: &reqwest::Client,
        responses_url: &url::Url,
        lease: &CodexLease,
        body: &CodexRequestBody,
    ) -> Result<reqwest::Request, reqwest::Error> {
        client
            .post(responses_url.clone())
            .header(
                reqwest::header::AUTHORIZATION,
                format!("Bearer {}", lease.access_token),
            )
            .header("Chatgpt-Account-Id", lease.account_id.as_str())
            .json(body)
            .build()
    }

    pub async fn try_post_codex(
        client: reqwest::Client,
        responses_url: url::Url,
        lease: &CodexLease,
        body: &CodexRequestBody,
        retry_policy: ExponentialBuilder,
    ) -> Result<reqwest::Response, reqwest::Error> {
        let lease = lease.clone();

        (|| {
            let client = client.clone();
            let responses_url = responses_url.clone();
            let lease = lease.clone();
            async move {
                let req = Self::build_codex_request(&client, &responses_url, &lease, body)?;
                let resp = client.execute(req).await?;
                if resp.status().is_server_error() {
                    let status = resp.status();
                    let err = resp.error_for_status_ref().unwrap_err();

                    let body_preview = match resp.bytes().await {
                        Ok(bytes) => {
                            let raw_body = String::from_utf8_lossy(&bytes);
                            format!("{:.len$}", raw_body, len = UPSTREAM_BODY_PREVIEW_CHARS)
                        }
                        Err(e) => format!("<failed to read body: {e}>"),
                    };

                    tracing::debug!(
                        %status,
                        body = %body_preview,
                        "Codex upstream server error (will retry)"
                    );
                    return Err(err);
                }
                Ok(resp)
            }
        })
        .retry(retry_policy)
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::Method;
    use std::collections::BTreeMap;
    use url::Url;

    #[test]
    fn build_codex_request_sets_expected_headers() {
        let responses_url =
            Url::parse("http://example.test/backend-api/codex/responses").expect("invalid url");
        let http = reqwest::Client::new();

        let lease = CodexLease {
            id: 1,
            account_id: "acct-test".to_string(),
            access_token: "at-test".to_string(),
        };

        let body = CodexRequestBody {
            instructions: "".to_string(),
            parallel_tool_calls: true,
            model: "x".to_string(),
            input: vec![],
            stream: true,
            store: false,
            reasoning: None,
            include: None,
            extra: BTreeMap::new(),
        };

        let req = CodexApi::build_codex_request(&http, &responses_url, &lease, &body)
            .expect("failed to build request");

        assert_eq!(req.method(), Method::POST);
        assert_eq!(req.url().as_str(), responses_url.as_str());
        assert_eq!(
            req.headers()
                .get(reqwest::header::AUTHORIZATION)
                .and_then(|v| v.to_str().ok()),
            Some("Bearer at-test")
        );
        assert_eq!(
            req.headers()
                .get("chatgpt-account-id")
                .and_then(|v| v.to_str().ok()),
            Some("acct-test")
        );
    }

    #[test]
    fn build_codex_request_sets_account_id() {
        let responses_url =
            Url::parse("http://example.test/backend-api/codex/responses").expect("invalid url");
        let http = reqwest::Client::new();

        let lease = CodexLease {
            id: 1,
            account_id: "acct-test".to_string(),
            access_token: "at-test".to_string(),
        };

        let body = CodexRequestBody {
            instructions: "".to_string(),
            parallel_tool_calls: true,
            model: "x".to_string(),
            input: vec![],
            stream: false,
            store: false,
            reasoning: None,
            include: None,
            extra: BTreeMap::new(),
        };

        let req = CodexApi::build_codex_request(&http, &responses_url, &lease, &body)
            .expect("failed to build request");

        assert_eq!(
            req.headers()
                .get("chatgpt-account-id")
                .and_then(|v| v.to_str().ok()),
            Some("acct-test")
        );
    }
}
