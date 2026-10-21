//! Production OAuth/project I/O, with request-owned cancellation and environment.
use super::{Body, Port, Reply, Request};
use crate::{
    project::ProjectPort,
    transport::network::{Client, Options, Response},
};
use async_compression::tokio::write::BrotliDecoder;
use futures_util::TryStreamExt;
use std::{
    collections::HashMap,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio_util::sync::CancellationToken;

pub struct Network {
    http: reqwest::Client,
    agy: Client,
    options: Options,
}
impl Network {
    pub fn new(
        environment: HashMap<String, String>,
        signal: CancellationToken,
    ) -> Result<Self, String> {
        let http = Self::http_builder()
            .build()
            .map_err(|error| error.to_string())?;
        Ok(Self {
            http,
            agy: Client::default(),
            options: Options {
                environment,
                signal,
                ..Options::default()
            },
        })
    }
    pub fn http_builder() -> reqwest::ClientBuilder {
        let mut headers = reqwest::header::HeaderMap::new();
        for (name, value) in [
            ("connection", "keep-alive"),
            ("accept", "*/*"),
            ("accept-language", "*"),
            ("sec-fetch-mode", "cors"),
            ("user-agent", "node"),
            ("accept-encoding", "br, gzip, deflate"),
        ] {
            headers.insert(
                reqwest::header::HeaderName::from_static(name),
                reqwest::header::HeaderValue::from_static(value),
            );
        }
        // Undici's ordinary fetch is direct; the separate AGY transport owns proxy selection.
        let mut builder = reqwest::Client::builder()
            .tls_built_in_root_certs(false)
            .no_proxy()
            .http1_only()
            .no_gzip()
            .no_brotli()
            .no_deflate()
            .no_zstd()
            .default_headers(headers)
            .redirect(reqwest::redirect::Policy::limited(20));
        for certificate in crate::transport::trust::certificates() {
            builder = builder.add_root_certificate(
                reqwest::Certificate::from_der(certificate.as_ref())
                    .expect("validated certificate"),
            );
        }
        builder
    }
    pub fn with_clients(http: reqwest::Client, agy: Client, options: Options) -> Self {
        Self { http, agy, options }
    }
}
pub enum ResponseBody {
    Fetch {
        response: reqwest::Response,
        signal: CancellationToken,
        codings: Vec<String>,
    },
    Agy(Response),
}
impl Body for ResponseBody {
    async fn text(self) -> Result<String, String> {
        let bytes = match self {
            Self::Fetch {
                response,
                signal,
                codings,
            } => tokio::select! {
                biased;
                _=signal.cancelled()=>return Err("This operation was aborted".into()),
                bytes=fetch_bytes(response, &codings)=>bytes?,
            },
            Self::Agy(mut response) => {
                let mut bytes = Vec::new();
                while let Some(next) = response.body.next().await? {
                    bytes.extend(next);
                }
                bytes
            }
        };
        Ok(encoding_rs::UTF_8.decode(&bytes).0.into_owned())
    }
}
impl Port for Network {
    type Body = ResponseBody;
    async fn send(&self, request: Request) -> Result<Reply<Self::Body>, String> {
        if request.agy {
            let mut options = self.options.clone();
            options.timeout_ms = request.timeout_ms;
            let response = self
                .agy
                .fetch(
                    &request.url,
                    Some(&request.method),
                    &request.headers,
                    request.body.as_deref().unwrap_or_default().as_bytes(),
                    options,
                )
                .await?;
            return Ok(Reply {
                status: response.head.status,
                status_text: response.head.status_text.clone(),
                body: ResponseBody::Agy(response),
            });
        }
        let method = reqwest::Method::from_bytes(request.method.as_bytes())
            .map_err(|error| error.to_string())?;
        let mut send = self.http.request(method, &request.url);
        for (name, value) in request.headers {
            send = send.header(name, value);
        }
        if let Some(body) = request.body {
            send = send.body(body);
        }
        let response = tokio::select! {
            biased;
            _=self.options.signal.cancelled()=>return Err("This operation was aborted".into()),
            result=tokio::time::timeout(Duration::from_millis(request.timeout_ms),send.send())=>result
                .map_err(|_|"The operation was aborted due to timeout".to_owned())?
                .map_err(|_|"fetch failed".to_owned())?,
        };
        let status_text = response
            .extensions()
            .get::<hyper::ext::ReasonPhrase>()
            .map(|reason| {
                reason
                    .as_bytes()
                    .iter()
                    .map(|byte| char::from(*byte))
                    .collect()
            })
            .unwrap_or_else(|| {
                response
                    .status()
                    .canonical_reason()
                    .unwrap_or_default()
                    .into()
            });
        let encoding = response
            .headers()
            .get_all(reqwest::header::CONTENT_ENCODING)
            .iter()
            .map(|value| {
                value
                    .as_bytes()
                    .iter()
                    .map(|byte| char::from(*byte))
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join(",");
        let mut codings: Vec<String> = if encoding.is_empty() {
            Vec::new()
        } else {
            encoding
                .split(',')
                .map(|coding| coding.trim().to_ascii_lowercase())
                .collect()
        };
        if matches!(response.status().as_u16(), 204 | 205 | 304) {
            codings.clear();
        }
        if codings.len() > 5 {
            return Err("fetch failed".into());
        }
        if codings.iter().any(|coding| {
            !matches!(
                coding.as_str(),
                "gzip" | "x-gzip" | "deflate" | "br" | "zstd"
            )
        }) {
            codings.clear();
        }
        Ok(Reply {
            status: response.status().as_u16(),
            status_text,
            body: ResponseBody::Fetch {
                response,
                signal: self.options.signal.clone(),
                codings,
            },
        })
    }
    fn now_ms(&self) -> f64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_secs_f64() * 1000.0)
            .unwrap_or_default()
            .floor()
    }
    fn user_agent(&self) -> String {
        crate::transport::default_user_agent()
    }
}

async fn fetch_bytes(response: reqwest::Response, codings: &[String]) -> Result<Vec<u8>, String> {
    let stream = response.bytes_stream().map_err(std::io::Error::other);
    let mut reader: Box<dyn AsyncRead + Unpin + Send> =
        Box::new(tokio_util::io::StreamReader::new(stream));
    for coding in codings.iter().rev() {
        let mut input = BufReader::new(reader);
        reader = match coding.as_str() {
            "gzip" | "x-gzip" => inflate(Box::new(input), Default::default()),
            "br" => brotli(Box::new(input)),
            "zstd" => zstd(Box::new(input)).map_err(|_| "terminated".to_owned())?,
            "deflate" => {
                let header = input
                    .fill_buf()
                    .await
                    .map_err(|_| "terminated".to_owned())?;
                let zlib = header.first().is_some_and(|byte| byte & 15 == 8);
                inflate(
                    Box::new(input),
                    crate::transport::body::Inflater::deflate(zlib),
                )
            }
            _ => unreachable!("validated content encoding"),
        };
    }
    let mut bytes = Vec::new();
    match reader.read_to_end(&mut bytes).await {
        Ok(_) => Ok(bytes),
        // Undici uses Z_SYNC_FLUSH for gzip/deflate and a non-final Brotli flush.
        Err(error)
            if error.kind() == std::io::ErrorKind::UnexpectedEof
                && !codings.iter().any(|coding| coding == "zstd")
                && !codings.is_empty() =>
        {
            Ok(bytes)
        }
        Err(_) => Err("terminated".into()),
    }
}

fn inflate(
    reader: Box<dyn AsyncRead + Unpin + Send>,
    decoder: crate::transport::body::Inflater,
) -> Box<dyn AsyncRead + Unpin + Send> {
    struct State {
        reader: Box<dyn AsyncRead + Unpin + Send>,
        decoder: crate::transport::body::Inflater,
        input: Vec<u8>,
        position: usize,
    }
    let state = State {
        reader,
        decoder,
        input: Vec::new(),
        position: 0,
    };
    let stream = futures_util::stream::try_unfold(state, |mut state| async move {
        loop {
            while state.position < state.input.len() {
                let (consumed, output) = state.decoder.push(&state.input[state.position..])?;
                state.position += consumed;
                if !output.is_empty() {
                    return Ok::<_, std::io::Error>(Some((std::io::Cursor::new(output), state)));
                }
            }
            let (_, output) = state.decoder.drain(false)?;
            if !output.is_empty() {
                return Ok(Some((std::io::Cursor::new(output), state)));
            }
            state.input.resize(16384, 0);
            let count = state.reader.read(&mut state.input).await?;
            // Undici uses a sync flush at EOF, accepting truncated headers and trailers.
            if count == 0 {
                return Ok(None);
            }
            state.input.truncate(count);
            state.position = 0;
        }
    });
    Box::new(tokio_util::io::StreamReader::new(Box::pin(stream)))
}

fn brotli(reader: Box<dyn AsyncRead + Unpin + Send>) -> Box<dyn AsyncRead + Unpin + Send> {
    let state = (reader, BrotliDecoder::new(Vec::new()), false);
    let stream =
        futures_util::stream::try_unfold(state, |(mut reader, mut decoder, mut eof)| async move {
            loop {
                if eof {
                    return Ok::<_, std::io::Error>(None);
                }
                let mut input = [0; 16384];
                let count = reader.read(&mut input).await?;
                if count == 0 {
                    // Node flushes available Brotli output without requiring a complete final block.
                    decoder.flush().await?;
                    eof = true;
                } else {
                    decoder.write_all(&input[..count]).await?;
                }
                let output = std::mem::take(decoder.get_mut());
                if !output.is_empty() {
                    return Ok(Some((std::io::Cursor::new(output), (reader, decoder, eof))));
                }
            }
        });
    Box::new(tokio_util::io::StreamReader::new(Box::pin(stream)))
}
fn zstd(
    reader: Box<dyn AsyncRead + Unpin + Send>,
) -> std::io::Result<Box<dyn AsyncRead + Unpin + Send>> {
    use zstd::stream::raw::{Decoder, Operation};
    let state = (reader, Decoder::new()?, Vec::new(), 0, false);
    let stream = futures_util::stream::try_unfold(
        state,
        |(mut reader, mut decoder, mut input, mut position, mut complete)| async move {
            loop {
                if !complete || position < input.len() {
                    let mut output = vec![0; 16384];
                    let status = decoder.run_on_buffers(&input[position..], &mut output)?;
                    position += status.bytes_read;
                    complete = status.remaining == 0;
                    output.truncate(status.bytes_written);
                    if !output.is_empty() {
                        return Ok::<_, std::io::Error>(Some((
                            std::io::Cursor::new(output),
                            (reader, decoder, input, position, complete),
                        )));
                    }
                    if position < input.len() {
                        continue;
                    }
                }
                input.resize(16384, 0);
                let count = reader.read(&mut input).await?;
                if count == 0 {
                    return if complete {
                        Ok(None)
                    } else {
                        Err(std::io::Error::new(
                            std::io::ErrorKind::UnexpectedEof,
                            "incomplete zstd response",
                        ))
                    };
                }
                input.truncate(count);
                position = 0;
            }
        },
    );
    Ok(Box::new(tokio_util::io::StreamReader::new(Box::pin(
        stream,
    ))))
}
impl ProjectPort for Network {
    async fn wait(&self, milliseconds: u64) {
        tokio::time::sleep(Duration::from_millis(milliseconds)).await;
    }
}

impl crate::driver::Body for crate::transport::network::Body {
    async fn next(&mut self) -> Result<Option<Vec<u8>>, String> {
        crate::transport::network::Body::next(self).await
    }
}
impl crate::local::Port for Network {
    type StreamBody = crate::transport::network::Body;
    fn next_id(&self) -> String {
        uuid::Uuid::new_v4().to_string()
    }
    async fn dispatch(
        &self,
        prepared: crate::request::PreparedRequest,
        access: &str,
        project: &str,
        signal: CancellationToken,
    ) -> Result<crate::driver::Response<Self::StreamBody>, String> {
        let response=self.agy.fetch(
            "https://daily-cloudcode-pa.sandbox.googleapis.com/v1internal:streamGenerateContent?alt=sse",
            Some("POST"),
            &[("Authorization".into(),format!("Bearer {access}")),("Content-Type".into(),"application/json".into()),("User-Agent".into(),self.user_agent()),("Accept-Encoding".into(),"gzip".into())],
            prepared.body(project).as_bytes(),
            Options { signal, ..self.options.clone() }
        ).await?;
        Ok(crate::driver::Response {
            status: response.head.status,
            headers: response
                .head
                .headers
                .into_iter()
                .map(|(name, value)| (name, value.chars().map(|byte| byte as u8).collect()))
                .collect(),
            body: response.body,
        })
    }
}
