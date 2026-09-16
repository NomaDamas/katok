use crate::{
    config::{KatokConfig, DEFAULT_EMBEDDER_MODEL},
    Error, Result,
};
use fastembed::{EmbeddingModel, TextEmbedding, TextInitOptions};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

pub(crate) trait SemanticEmbedder {
    fn id(&self) -> &str;
    fn embed(&mut self, texts: &[String], batch_size: usize) -> Result<Vec<Vec<f32>>>;
    fn embed_query(&mut self, query: &str) -> Result<Vec<f32>> {
        let embeddings = self.embed(&[query.to_string()], 1)?;
        embeddings
            .into_iter()
            .next()
            .ok_or_else(|| Error::Embedding("embedder returned no query vector".to_string()))
    }
}

pub(crate) fn create_embedder(config: &KatokConfig) -> Result<Box<dyn SemanticEmbedder>> {
    if config.embedding_provider == "loopback-http" {
        return Ok(Box::new(LoopbackHttpEmbedder::new(config)?));
    }
    if config.embedding_provider != "local" {
        return Err(Error::Embedding(format!(
            "unsupported embedding provider: {}",
            config.embedding_provider
        )));
    }
    match std::env::var("KATOK_EMBEDDER").ok().as_deref() {
        Some("mock" | "local-test") => Ok(Box::new(DeterministicEmbedder::new(usize::from(
            config.vector_dimension,
        )))),
        _ => Ok(Box::new(FastEmbedder::new(config)?)),
    }
}

struct FastEmbedder {
    inner: TextEmbedding,
}

impl FastEmbedder {
    fn new(config: &KatokConfig) -> Result<Self> {
        if config.embedder_model != DEFAULT_EMBEDDER_MODEL {
            return Err(Error::Embedding(format!(
                "unsupported local embedder model: {}",
                config.embedder_model
            )));
        }
        let options = TextInitOptions::new(EmbeddingModel::EmbeddingGemma300MQ4)
            .with_show_download_progress(false);
        let inner = TextEmbedding::try_new(options).map_err(to_embedding_error)?;
        Ok(Self { inner })
    }
}

impl SemanticEmbedder for FastEmbedder {
    fn id(&self) -> &str {
        "embeddinggemma/local"
    }

    fn embed(&mut self, texts: &[String], batch_size: usize) -> Result<Vec<Vec<f32>>> {
        let documents = texts
            .iter()
            .map(|text| format!("passage: {text}"))
            .collect::<Vec<_>>();
        self.inner
            .embed(documents, Some(batch_size.max(1)))
            .map_err(to_embedding_error)
    }

    fn embed_query(&mut self, query: &str) -> Result<Vec<f32>> {
        let embeddings = self
            .inner
            .embed([format!("query: {}", query.trim())], Some(1))
            .map_err(to_embedding_error)?;
        embeddings
            .into_iter()
            .next()
            .ok_or_else(|| Error::Embedding("embedder returned no query vector".to_string()))
    }
}

struct LoopbackHttpEmbedder {
    endpoint: Endpoint,
    model: String,
    dimension: usize,
    timeout: Duration,
    query_prefix: String,
    passage_prefix: String,
    identity: String,
}

#[derive(Clone)]
struct Endpoint {
    host: String,
    port: u16,
    path: String,
}

#[derive(Deserialize)]
struct EmbedResponse {
    embeddings: Vec<Vec<f32>>,
}

impl LoopbackHttpEmbedder {
    fn new(config: &KatokConfig) -> Result<Self> {
        let raw = config.embedding_endpoint.as_deref().ok_or_else(|| {
            Error::Embedding("loopback-http requires embedding_endpoint".to_string())
        })?;
        let endpoint = parse_loopback_endpoint(raw)?;
        Ok(Self {
            endpoint,
            model: config.embedder_model.clone(),
            dimension: usize::from(config.vector_dimension),
            timeout: Duration::from_millis(config.embedding_timeout_ms.max(1)),
            query_prefix: config.embedding_query_prefix.clone(),
            passage_prefix: config.embedding_passage_prefix.clone(),
            identity: format!(
                "embeddinggemma/loopback-http:{}:{}:{}:{}:{}:{}",
                config.embedder_model,
                config.vector_dimension,
                config.embedding_query_prefix,
                config.embedding_passage_prefix,
                config.embedding_timeout_ms,
                config.embedding_endpoint.as_deref().unwrap_or_default()
            ),
        })
    }

    fn request(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let body = serde_json::to_vec(&serde_json::json!({
            "model": self.model,
            "input": texts,
        }))?;
        let address = (self.endpoint.host.as_str(), self.endpoint.port)
            .to_socket_addrs()
            .map_err(|error| Error::Embedding(format!("resolve loopback endpoint: {error}")))?
            .next()
            .ok_or_else(|| Error::Embedding("loopback endpoint has no address".to_string()))?;
        let mut stream = TcpStream::connect_timeout(&address, self.timeout)
            .map_err(|error| Error::Embedding(format!("connect loopback endpoint: {error}")))?;
        stream
            .set_read_timeout(Some(self.timeout))
            .and_then(|_| stream.set_write_timeout(Some(self.timeout)))
            .map_err(|error| Error::Embedding(format!("configure endpoint timeout: {error}")))?;
        write!(
            stream,
            "POST {} HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            self.endpoint.path,
            self.endpoint.host,
            body.len()
        )
        .map_err(|error| Error::Embedding(format!("write embedding request: {error}")))?;
        stream
            .write_all(&body)
            .map_err(|error| Error::Embedding(format!("write embedding body: {error}")))?;
        let mut response = Vec::new();
        stream
            .read_to_end(&mut response)
            .map_err(|error| Error::Embedding(format!("read embedding response: {error}")))?;
        let separator = response
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .ok_or_else(|| Error::Embedding("embedding response has no headers".to_string()))?;
        let headers = std::str::from_utf8(&response[..separator])
            .map_err(|error| Error::Embedding(format!("invalid embedding headers: {error}")))?;
        if !headers.starts_with("HTTP/1.1 200 ") && !headers.starts_with("HTTP/1.0 200 ") {
            return Err(Error::Embedding(format!(
                "embedding endpoint returned {}",
                headers.lines().next().unwrap_or("unknown status")
            )));
        }
        let parsed: EmbedResponse = serde_json::from_slice(&response[separator + 4..])?;
        if parsed.embeddings.len() != texts.len()
            || parsed
                .embeddings
                .iter()
                .any(|vector| vector.len() != self.dimension)
        {
            return Err(Error::Embedding(format!(
                "endpoint returned {} embeddings with expected {} vectors of dimension {}",
                parsed.embeddings.len(),
                texts.len(),
                self.dimension
            )));
        }
        Ok(parsed.embeddings)
    }
}

impl SemanticEmbedder for LoopbackHttpEmbedder {
    fn id(&self) -> &str {
        &self.identity
    }

    fn embed(&mut self, texts: &[String], _batch_size: usize) -> Result<Vec<Vec<f32>>> {
        self.request(
            &texts
                .iter()
                .map(|text| format!("{}{}", self.passage_prefix, text))
                .collect::<Vec<_>>(),
        )
    }

    fn embed_query(&mut self, query: &str) -> Result<Vec<f32>> {
        self.request(&[format!("{}{}", self.query_prefix, query.trim())])?
            .into_iter()
            .next()
            .ok_or_else(|| Error::Embedding("embedder returned no query vector".to_string()))
    }
}

fn parse_loopback_endpoint(raw: &str) -> Result<Endpoint> {
    let rest = raw
        .strip_prefix("http://")
        .ok_or_else(|| Error::Embedding("loopback endpoint must use http://".to_string()))?;
    let (authority, path) = rest.split_once('/').unwrap_or((rest, "embed"));
    let (host, port) = authority
        .rsplit_once(':')
        .ok_or_else(|| Error::Embedding("loopback endpoint requires a port".to_string()))?;
    let host = host.trim_matches(['[', ']']);
    let port = port
        .parse::<u16>()
        .map_err(|error| Error::Embedding(format!("invalid loopback port: {error}")))?;
    if !matches!(host, "localhost" | "127.0.0.1" | "::1") {
        return Err(Error::Embedding(format!(
            "embedding endpoint must be loopback, got {host}"
        )));
    }
    Ok(Endpoint {
        host: host.to_string(),
        port,
        path: format!("/{path}"),
    })
}

struct DeterministicEmbedder {
    dimension: usize,
}

impl DeterministicEmbedder {
    const fn new(dimension: usize) -> Self {
        Self { dimension }
    }
}

impl SemanticEmbedder for DeterministicEmbedder {
    fn id(&self) -> &str {
        "embeddinggemma/local-test"
    }

    fn embed(&mut self, texts: &[String], _batch_size: usize) -> Result<Vec<Vec<f32>>> {
        texts
            .iter()
            .map(|text| deterministic_vector(text, self.dimension))
            .collect()
    }
}

fn deterministic_vector(text: &str, dimension: usize) -> Result<Vec<f32>> {
    if dimension == 0 {
        return Err(Error::Embedding(
            "embedding dimension must be nonzero".to_string(),
        ));
    }
    let mut vector = vec![0.0_f32; dimension];
    for term in text.split_whitespace() {
        let hash = Sha256::digest(term.as_bytes());
        let mut bytes = [0_u8; 8];
        bytes.copy_from_slice(&hash[..8]);
        let dimension =
            u64::try_from(dimension).map_err(|error| Error::Embedding(error.to_string()))?;
        let index = usize::try_from(u64::from_le_bytes(bytes) % dimension)
            .map_err(|error| Error::Embedding(error.to_string()))?;
        vector[index] += 1.0;
    }
    normalize(&mut vector);
    Ok(vector)
}

fn normalize(vector: &mut [f32]) {
    let norm = vector.iter().map(|value| value * value).sum::<f32>().sqrt();
    if norm == 0.0 {
        return;
    }
    for value in vector {
        *value /= norm;
    }
}

fn to_embedding_error(error: impl std::fmt::Display) -> Error {
    Error::Embedding(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::net::TcpListener;

    /// The loopback runtime must see embedding text and the model name only:
    /// archive identifiers, source paths, and store metadata stay inside katok.
    #[test]
    fn loopback_http_request_contains_only_model_and_text() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind capture server");
        let port = listener.local_addr().expect("local addr").port();
        let server = std::thread::spawn(move || {
            let mut bodies = Vec::new();
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().expect("accept");
                let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));
                let mut content_length = 0usize;
                loop {
                    let mut line = String::new();
                    let read = reader.read_line(&mut line).expect("header line");
                    if read == 0 || line == "\r\n" {
                        break;
                    }
                    if let Some((name, value)) = line.trim_end().split_once(':') {
                        if name.eq_ignore_ascii_case("content-length") {
                            content_length = value.trim().parse().expect("content length");
                        }
                    }
                }
                let mut body = vec![0u8; content_length];
                reader.read_exact(&mut body).expect("read body");
                bodies.push(String::from_utf8(body).expect("utf8 body"));
                let response = "{\"embeddings\":[[0.0,0.0,0.0]]}";
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    response.len(),
                    response
                )
                .expect("write response");
            }
            bodies
        });

        let config = KatokConfig {
            embedding_provider: "loopback-http".to_string(),
            embedding_endpoint: Some(format!("http://127.0.0.1:{port}/embed")),
            vector_dimension: 3,
            ..KatokConfig::default()
        };
        let mut embedder = LoopbackHttpEmbedder::new(&config).expect("create embedder");

        let embedded = embedder
            .embed(
                &["\u{ba54}\u{c2dc}\u{c9c0} \u{bcf8}\u{bb38}".to_string()],
                1,
            )
            .expect("embed passage");
        assert_eq!(embedded.len(), 1);
        embedder
            .embed_query("\u{ac80}\u{c0c9} \u{c9c8}\u{c758}")
            .expect("embed query");

        let bodies = server.join().expect("join capture server");
        assert_eq!(bodies.len(), 2);

        let passage: serde_json::Value =
            serde_json::from_str(&bodies[0]).expect("passage request json");
        let query: serde_json::Value =
            serde_json::from_str(&bodies[1]).expect("query request json");
        for body in [&passage, &query] {
            let keys: std::collections::BTreeSet<&str> = body
                .as_object()
                .expect("request object")
                .keys()
                .map(String::as_str)
                .collect();
            assert_eq!(
                keys,
                ["input", "model"].into_iter().collect(),
                "request must carry embedding text and model only: {body}"
            );
        }
        assert_eq!(
            passage["input"],
            serde_json::json!(["passage: \u{ba54}\u{c2dc}\u{c9c0} \u{bcf8}\u{bb38}"])
        );
        assert_eq!(
            query["input"],
            serde_json::json!(["query: \u{ac80}\u{c0c9} \u{c9c8}\u{c758}"])
        );
        assert_eq!(passage["model"], serde_json::json!(DEFAULT_EMBEDDER_MODEL));
    }
}
