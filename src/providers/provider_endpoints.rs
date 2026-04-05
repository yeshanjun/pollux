use url::Url;

fn build_provider_url(base: &Url, path: &str, query: Option<&str>) -> Url {
    let mut url = base.join(path).expect("valid endpoint path");
    url.set_query(query);
    url
}

#[derive(Debug, Clone)]
pub(crate) struct ProviderEndpoints {
    stream: Url,
    no_stream: Url,
}

impl ProviderEndpoints {
    pub(crate) fn new(
        base: Url,
        stream_path: &str,
        stream_query: Option<&str>,
        no_stream_path: &str,
        no_stream_query: Option<&str>,
    ) -> Self {
        Self {
            stream: build_provider_url(&base, stream_path, stream_query),
            no_stream: build_provider_url(&base, no_stream_path, no_stream_query),
        }
    }

    pub(crate) fn select(&self, stream: bool) -> &Url {
        if stream {
            &self.stream
        } else {
            &self.no_stream
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_base_url() {
        let base = Url::parse("https://api.example.com").unwrap();
        let ep = ProviderEndpoints::new(base, "./v1:stream", Some("alt=sse"), "./v1:gen", None);
        assert_eq!(
            ep.select(true).as_str(),
            "https://api.example.com/v1:stream?alt=sse"
        );
        assert_eq!(ep.select(false).as_str(), "https://api.example.com/v1:gen");
    }

    #[test]
    fn base_with_path_prefix() {
        let base = Url::parse("http://proxy.local:8080/prefix/").unwrap();
        let ep = ProviderEndpoints::new(base, "./v1:stream", Some("alt=sse"), "./v1:gen", None);
        assert_eq!(
            ep.select(true).as_str(),
            "http://proxy.local:8080/prefix/v1:stream?alt=sse"
        );
        assert_eq!(
            ep.select(false).as_str(),
            "http://proxy.local:8080/prefix/v1:gen"
        );
    }
}
