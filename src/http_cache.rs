use std::io::{BufReader, BufWriter};
use std::ops::DerefMut;
use std::os::unix::fs::MetadataExt;
use std::sync::Arc;

use async_compression::brotli::EncoderParams;
use async_compression::tokio::bufread::{BrotliDecoder, BrotliEncoder};
use async_compression::tokio::write;
use axum::http::uri::{Authority, Scheme};
use axum::http::HeaderMap;
use axum::http::{uri, HeaderValue};
use axum::response::{IntoResponse, Response};
use futures_util::TryStreamExt;
use helix_core::syntax::RopeProvider;
use helix_core::{Rope, RopeBuilder};
use helix_lsp::lsp::TextEdit;
use helix_stdx::rope::{Regex, RopeSliceExt};
use regex_cursor::Input;
use reqwest::header::{ACCEPT_ENCODING, CONTENT_ENCODING, HOST, IF_MODIFIED_SINCE, IF_NONE_MATCH};
use reqwest::header::{CONTENT_TYPE, ETAG};
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::{io::AsyncBufReadExt, sync::Mutex};
use tokio_util::io::StreamReader;
use tree_sitter::QueryCursor;

#[derive(Clone)]
pub struct HttpCache {
    pub client: reqwest::Client,
    pub cache_path: &'static std::path::Path,
    pub ts_parser: Arc<Mutex<(tree_sitter::Parser, QueryCursor)>>,
    pub js_lang: Arc<(tree_sitter::Language, tree_sitter::Query)>,
    pub css_lang: Arc<(tree_sitter::Language, tree_sitter::Query)>,
    pub regexes: Arc<Regexes>,
}

pub struct Regexes {
    pub content_to_compress: Regex,
    pub jackbox_urls: Regex,
}

#[derive(Deserialize, Serialize)]
struct JBHttpResponse {
    etag: String,
    content_type: Option<String>,
    compressed: bool,
}

impl JBHttpResponse {
    fn from_request(value: &reqwest::Response, content_to_compress: &Regex) -> Self {
        Self {
            etag: value
                .headers()
                .get(ETAG)
                .unwrap()
                .to_str()
                .unwrap()
                .to_owned(),
            content_type: value
                .headers()
                .get(CONTENT_TYPE)
                .map(|ct| ct.to_str().unwrap().to_owned()),
            compressed: value
                .headers()
                .get(CONTENT_TYPE)
                .is_some_and(|ct| content_to_compress.is_match(Input::new(ct.as_bytes()))),
        }
    }

    fn headers(&self) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(ETAG, self.etag.as_str().try_into().unwrap());
        if let Some(ref ct) = self.content_type {
            headers.insert(CONTENT_TYPE, ct.as_str().try_into().unwrap());
        }
        headers
    }
}

impl HttpCache {
    pub async fn get_cached(
        &self,
        mut uri: uri::Uri,
        mut headers: HeaderMap,
        offline: bool,
        accessible_host: &str,
    ) -> Result<Response, (StatusCode, String)> {
        let mut uri_parts = uri.into_parts();
        if uri_parts
            .path_and_query
            .as_ref()
            .map(|pq| pq.path().starts_with("/@"))
            .unwrap_or_default()
        {
            let path = uri_parts.path_and_query.as_ref().unwrap().as_str();

            let path_split = path.split_once('@').unwrap().1;
            let path_split = path_split.split_once('/').unwrap_or((path_split, ""));
            let host = path_split.0;
            if !self.regexes.jackbox_urls.is_match(Input::new(host)) {
                return Err((
                    StatusCode::BAD_REQUEST,
                    "Proxy can only be used with Jackbox services".to_owned(),
                ));
            }
            uri_parts.authority = Some(host.to_owned().try_into().unwrap());
            headers.insert(HOST, host.to_owned().try_into().unwrap());
            *uri_parts.path_and_query.as_mut().unwrap() =
                format!("/{}", path_split.1).try_into().unwrap();
        } else {
            uri_parts.authority = Some(Authority::from_static("jackbox.tv"));
            headers.insert(HOST, HeaderValue::from_static("jackbox.tv"));
        }
        uri_parts.scheme = Some(Scheme::try_from("https").unwrap());
        uri = uri::Uri::from_parts(uri_parts).unwrap();

        headers.remove(IF_MODIFIED_SINCE);

        let br = headers
            .get(ACCEPT_ENCODING)
            .as_ref()
            .map(|e| e.to_str().unwrap().split(','))
            .into_iter()
            .flatten()
            .any(|s| s.trim() == "br");

        let mut cached_resource_raw = self.cache_path.join(format!(
            "{}/{}",
            uri.host().unwrap(),
            if uri.path() == "/" {
                "index.html"
            } else {
                uri.path()
            }
        ));
        let mut reqwest_resp = if offline {
            None
        } else {
            Some(
                self.client
                    .get(format!("{}", uri))
                    .headers(headers.clone())
                    .send()
                    .await
                    .map_err(|e| {
                        (
                            StatusCode::INTERNAL_SERVER_ERROR,
                            format!("{}: {}", line!(), e),
                        )
                    })?,
            )
        };
        let mut resp = if let Some(ref resp) = reqwest_resp {
            JBHttpResponse::from_request(resp, &self.regexes.content_to_compress)
        } else {
            serde_json::from_reader(BufReader::new(
                std::fs::File::open(&cached_resource_raw).map_err(|e| {
                    (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        format!("{}: Failed deserializing offline resource: {}", line!(), e),
                    )
                })?,
            ))
            .map_err(|e| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("{}: {}", line!(), e),
                )
            })?
        };

        let mut cached_resource = if resp.compressed {
            self.cache_path
                .join(format!("{}/{}.br", uri.host().unwrap(), resp.etag))
        } else {
            self.cache_path
                .join(format!("{}/{}", uri.host().unwrap(), resp.etag))
        };

        let part_path = cached_resource.with_extension("part.br");
        tokio::fs::create_dir_all(cached_resource.parent().unwrap())
            .await
            .unwrap();

        if reqwest_resp
            .as_ref()
            .is_some_and(|r| r.status() == StatusCode::NOT_MODIFIED)
            && !cached_resource.exists()
        {
            headers.remove(IF_NONE_MATCH);
            reqwest_resp = if offline {
                None
            } else {
                Some(
                    self.client
                        .get(format!("{}", uri))
                        .headers(headers.clone())
                        .send()
                        .await
                        .map_err(|e| {
                            (
                                StatusCode::INTERNAL_SERVER_ERROR,
                                format!("{}: {}", line!(), e),
                            )
                        })?,
                )
            };
            cached_resource_raw = self.cache_path.join(format!(
                "{}/{}",
                uri.host().unwrap(),
                if uri.path() == "/" {
                    "index.html"
                } else {
                    uri.path()
                }
            ));
            resp = if let Some(ref resp) = reqwest_resp {
                JBHttpResponse::from_request(resp, &self.regexes.content_to_compress)
            } else {
                serde_json::from_reader(BufReader::new(
                    std::fs::File::open(&cached_resource_raw).map_err(|e| {
                        (
                            StatusCode::INTERNAL_SERVER_ERROR,
                            format!("{}: {}", line!(), e),
                        )
                    })?,
                ))
                .map_err(|e| {
                    (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        format!("{}: {}", line!(), e),
                    )
                })?
            };
            cached_resource = if resp.compressed {
                self.cache_path
                    .join(format!("{}/{}.br", uri.host().unwrap(), resp.etag))
            } else {
                self.cache_path
                    .join(format!("{}/{}", uri.host().unwrap(), resp.etag))
            };
        }

        if !cached_resource.exists() {
            let mut part_file = fd_lock::RwLock::new(
                tokio::fs::OpenOptions::new()
                    .create(true)
                    .write(true)
                    .open(&part_path)
                    .await
                    .map_err(|e| {
                        (
                            StatusCode::INTERNAL_SERVER_ERROR,
                            format!("{}: {}", line!(), e),
                        )
                    })?,
            );
            let mut part_file_w = part_file.write().unwrap();
            if reqwest_resp
                .as_ref()
                .map(|r| r.status() == StatusCode::NOT_MODIFIED)
                .unwrap_or(true)
                || cached_resource.exists()
            {
                drop(part_file_w);
                let _ = tokio::fs::remove_file(&part_path).await;
            } else {
                match resp.content_type.as_ref().map(|ct| ct.as_ref()) {
                    Some("text/javascript") | Some("application/json") => {
                        let mut rope = rope_from_reader(StreamReader::new(
                            reqwest_resp.unwrap().bytes_stream().map_err(|e| {
                                std::io::Error::new(std::io::ErrorKind::ConnectionReset, e)
                            }),
                        ))
                        .await
                        .map_err(|e| {
                            (
                                StatusCode::INTERNAL_SERVER_ERROR,
                                format!("{}: {}", line!(), e),
                            )
                        })?;

                        let mut lock = self.ts_parser.lock().await;
                        let (parser, query_cursor) = lock.deref_mut();

                        parser.set_language(&self.js_lang.0).unwrap();

                        let tree = parser
                            .parse_with(
                                &mut |byte, _| {
                                    if byte <= rope.len_bytes() {
                                        let (chunk, start_byte, _, _) = rope.chunk_at_byte(byte);
                                        &chunk.as_bytes()[byte - start_byte..]
                                    } else {
                                        // out of range
                                        &[]
                                    }
                                },
                                None,
                            )
                            .unwrap();

                        let mut edits: Vec<TextEdit> = Vec::new();
                        for query_match in query_cursor.matches(
                            &self.js_lang.1,
                            tree.root_node(),
                            RopeProvider(rope.slice(..)),
                        ) {
                            let range = query_match.captures[0].node.byte_range();
                            let slice = rope.byte_slice(range.clone());
                            let range =
                                rope.byte_to_char(range.start)..rope.byte_to_char(range.end);
                            for regex_match in
                                self.regexes.jackbox_urls.find_iter(slice.regex_input())
                            {
                                edits.push(TextEdit {
                                    range: helix_lsp::util::range_to_lsp_range(
                                        &rope,
                                        helix_core::selection::Range {
                                            anchor: range.start + regex_match.start(),
                                            head: range.start + regex_match.start(),
                                            // head: (range.start + regex_match.end()),
                                            old_visual_position: None,
                                        },
                                        helix_lsp::OffsetEncoding::Utf8,
                                    ),
                                    new_text: format!("{}/@", accessible_host),
                                });
                            }
                        }

                        let transaction = helix_lsp::util::generate_transaction_from_edits(
                            &rope,
                            edits,
                            helix_lsp::OffsetEncoding::Utf8,
                        );

                        if !transaction.apply(&mut rope) {
                            return Err((
                                StatusCode::INTERNAL_SERVER_ERROR,
                                "Failed to patch JS file".to_owned(),
                            ));
                        }

                        if resp.compressed {
                            let mut stream = write::BrotliEncoder::with_quality_and_params(
                                tokio::io::BufWriter::new(part_file_w.deref_mut()),
                                async_compression::Level::Best,
                                EncoderParams::default().text_mode(),
                            );

                            for chunk in rope.chunks() {
                                stream.write_all(chunk.as_bytes()).await.map_err(|e| {
                                    (
                                        StatusCode::INTERNAL_SERVER_ERROR,
                                        format!("{}: {}", line!(), e),
                                    )
                                })?;
                            }

                            stream.shutdown().await.map_err(|e| {
                                (
                                    StatusCode::INTERNAL_SERVER_ERROR,
                                    format!("{}: {}", line!(), e),
                                )
                            })?;
                        } else {
                            let mut stream = tokio::io::BufWriter::new(part_file_w.deref_mut());

                            for chunk in rope.chunks() {
                                stream.write_all(chunk.as_bytes()).await.map_err(|e| {
                                    (
                                        StatusCode::INTERNAL_SERVER_ERROR,
                                        format!("{}: {}", line!(), e),
                                    )
                                })?;
                            }

                            stream.shutdown().await.map_err(|e| {
                                (
                                    StatusCode::INTERNAL_SERVER_ERROR,
                                    format!("{}: {}", line!(), e),
                                )
                            })?;
                        }
                    }
                    Some("text/css") => {
                        let mut rope = rope_from_reader(StreamReader::new(
                            reqwest_resp.unwrap().bytes_stream().map_err(|e| {
                                std::io::Error::new(std::io::ErrorKind::ConnectionReset, e)
                            }),
                        ))
                        .await
                        .map_err(|e| {
                            (
                                StatusCode::INTERNAL_SERVER_ERROR,
                                format!("{}: {}", line!(), e),
                            )
                        })?;

                        let mut lock = self.ts_parser.lock().await;
                        let (parser, query_cursor) = lock.deref_mut();

                        parser.set_language(&self.css_lang.0).unwrap();

                        let tree = parser
                            .parse_with(
                                &mut |byte, _| {
                                    if byte <= rope.len_bytes() {
                                        let (chunk, start_byte, _, _) = rope.chunk_at_byte(byte);
                                        &chunk.as_bytes()[byte - start_byte..]
                                    } else {
                                        // out of range
                                        &[]
                                    }
                                },
                                None,
                            )
                            .unwrap();

                        let mut edits: Vec<TextEdit> = Vec::new();
                        for query_match in query_cursor.matches(
                            &self.css_lang.1,
                            tree.root_node(),
                            RopeProvider(rope.slice(..)),
                        ) {
                            let range = query_match.captures[0].node.byte_range();
                            let slice = rope.byte_slice(range.clone());
                            let range =
                                rope.byte_to_char(range.start)..rope.byte_to_char(range.end);
                            for regex_match in
                                self.regexes.jackbox_urls.find_iter(slice.regex_input())
                            {
                                edits.push(TextEdit {
                                    range: helix_lsp::util::range_to_lsp_range(
                                        &rope,
                                        helix_core::selection::Range {
                                            anchor: range.start + regex_match.start(),
                                            head: range.start + regex_match.start(),
                                            // head: (range.start + regex_match.end()),
                                            old_visual_position: None,
                                        },
                                        helix_lsp::OffsetEncoding::Utf8,
                                    ),
                                    new_text: format!("{}/@", accessible_host),
                                });
                            }
                        }

                        let transaction = helix_lsp::util::generate_transaction_from_edits(
                            &rope,
                            edits,
                            helix_lsp::OffsetEncoding::Utf8,
                        );

                        if !transaction.apply(&mut rope) {
                            return Err((
                                StatusCode::INTERNAL_SERVER_ERROR,
                                "Failed to patch CSS file".to_owned(),
                            ));
                        }

                        if resp.compressed {
                            let mut stream = write::BrotliEncoder::with_quality_and_params(
                                tokio::io::BufWriter::new(part_file_w.deref_mut()),
                                async_compression::Level::Best,
                                EncoderParams::default().text_mode(),
                            );

                            for chunk in rope.chunks() {
                                stream.write_all(chunk.as_bytes()).await.map_err(|e| {
                                    (
                                        StatusCode::INTERNAL_SERVER_ERROR,
                                        format!("{}: {}", line!(), e),
                                    )
                                })?;
                            }

                            stream.shutdown().await.map_err(|e| {
                                (
                                    StatusCode::INTERNAL_SERVER_ERROR,
                                    format!("{}: {}", line!(), e),
                                )
                            })?;
                        } else {
                            let mut stream = tokio::io::BufWriter::new(part_file_w.deref_mut());

                            for chunk in rope.chunks() {
                                stream.write_all(chunk.as_bytes()).await.map_err(|e| {
                                    (
                                        StatusCode::INTERNAL_SERVER_ERROR,
                                        format!("{}: {}", line!(), e),
                                    )
                                })?;
                            }

                            stream.shutdown().await.map_err(|e| {
                                (
                                    StatusCode::INTERNAL_SERVER_ERROR,
                                    format!("{}: {}", line!(), e),
                                )
                            })?;
                        }
                    }
                    _ if resp.compressed => {
                        let mut stream = BrotliEncoder::with_quality(
                            StreamReader::new(reqwest_resp.unwrap().bytes_stream().map_err(|e| {
                                std::io::Error::new(std::io::ErrorKind::ConnectionReset, e)
                            })),
                            async_compression::Level::Best,
                        );

                        let mut file = tokio::io::BufWriter::new(part_file_w.deref_mut());

                        tokio::io::copy(&mut stream, &mut file).await.map_err(|e| {
                            (
                                StatusCode::INTERNAL_SERVER_ERROR,
                                format!("Failed to encode: {}", e),
                            )
                        })?;

                        file.shutdown().await.map_err(|e| {
                            (
                                StatusCode::INTERNAL_SERVER_ERROR,
                                format!("{}: {}", line!(), e),
                            )
                        })?;
                    }
                    _ => {
                        let mut stream =
                            StreamReader::new(reqwest_resp.unwrap().bytes_stream().map_err(|e| {
                                std::io::Error::new(std::io::ErrorKind::ConnectionReset, e)
                            }));
                        let mut file = tokio::io::BufWriter::new(part_file_w.deref_mut());

                        tokio::io::copy(&mut stream, &mut file).await.map_err(|e| {
                            (
                                StatusCode::INTERNAL_SERVER_ERROR,
                                format!("Failed to encode: {}", e),
                            )
                        })?;

                        file.shutdown().await.map_err(|e| {
                            (
                                StatusCode::INTERNAL_SERVER_ERROR,
                                format!("{}: {}", line!(), e),
                            )
                        })?;
                    }
                }
                tokio::fs::create_dir_all(cached_resource_raw.parent().unwrap())
                    .await
                    .map_err(|e| {
                        (
                            StatusCode::INTERNAL_SERVER_ERROR,
                            format!("{}: {}", line!(), e),
                        )
                    })?;
                serde_json::to_writer(
                    BufWriter::new(std::fs::File::create(cached_resource_raw).map_err(|e| {
                        (
                            StatusCode::INTERNAL_SERVER_ERROR,
                            format!("{}: {}", line!(), e),
                        )
                    })?),
                    &resp,
                )
                .map_err(|e| {
                    (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        format!("{}: {}", line!(), e),
                    )
                })?;
                tokio::fs::rename(part_path, &cached_resource)
                    .await
                    .map_err(|e| {
                        (
                            StatusCode::INTERNAL_SERVER_ERROR,
                            format!("{}: {}", line!(), e),
                        )
                    })?;
            }
        }

        // let status_code = if headers
        //     .get(IF_NONE_MATCH)
        //     .map(|ct| ct.as_bytes() != resp.etag.as_bytes())
        //     .unwrap_or(true)
        // {
        //     StatusCode::OK
        // } else {
        //     StatusCode::NOT_MODIFIED
        // };

        let status_code = StatusCode::OK;

        let mut resp_headers = resp.headers();
        let content_type = resp_headers.get(CONTENT_TYPE).cloned();
        if br {
            if resp.compressed {
                resp_headers.insert(CONTENT_ENCODING, HeaderValue::from_static("br"));
            }
            let mut resp = (
                status_code,
                resp_headers,
                tokio::fs::read(cached_resource)
                    .await
                    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?,
            )
                .into_response();
            if let Some(t) = content_type {
                resp.headers_mut().insert(CONTENT_TYPE, t);
            }
            return Ok(resp);
        } else {
            resp_headers.remove(CONTENT_ENCODING);
            let mut buf = Vec::with_capacity(
                tokio::fs::metadata(&cached_resource)
                    .await
                    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
                    .size() as usize,
            );
            let mut resp = if resp.compressed {
                (status_code, resp_headers, {
                    BrotliDecoder::new(tokio::io::BufReader::new(
                        tokio::fs::File::open(&cached_resource)
                            .await
                            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?,
                    ))
                    .read_to_end(&mut buf)
                    .await
                    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
                    buf
                })
                    .into_response()
            } else {
                (status_code, resp_headers, {
                    tokio::io::BufReader::new(
                        tokio::fs::File::open(&cached_resource)
                            .await
                            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?,
                    )
                    .read_to_end(&mut buf)
                    .await
                    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
                    buf
                })
                    .into_response()
            };
            if let Some(t) = content_type {
                resp.headers_mut().insert(CONTENT_TYPE, t);
            }
            return Ok(resp);
        }
    }
}

async fn rope_from_reader<T: tokio::io::AsyncBufRead>(reader: T) -> std::io::Result<Rope> {
    tokio::pin!(reader);
    let mut builder = RopeBuilder::new();
    let mut off_char: Option<heapless::Vec<u8, 4>> = None;
    let mut off_buff = Vec::new();
    loop {
        match (&mut reader).fill_buf().await {
            Ok(buffer) => {
                let buffer_len = buffer.len();
                if buffer_len == 0 {
                    break;
                }

                let buffer = if let Some(ref off_char) = off_char {
                    off_buff.clear();
                    off_buff.extend_from_slice(&off_char);
                    off_buff.extend_from_slice(buffer);
                    off_buff.as_slice()
                } else {
                    buffer
                };

                // Determine how much of the buffer is valid utf8.
                let valid_count = match std::str::from_utf8(buffer) {
                    Ok(_) => buffer.len(),
                    Err(e) => e.valid_up_to(),
                };

                // Append the valid part of the buffer to the rope.
                if valid_count > 0 {
                    // The unsafe block here is reinterpreting the bytes as
                    // utf8.  This is safe because the bytes being
                    // reinterpreted have already been validated as utf8
                    // just above.
                    builder
                        .append(unsafe { std::str::from_utf8_unchecked(&buffer[..valid_count]) });
                    let invalid = &buffer[valid_count..];

                    off_char = None;
                    if !invalid.is_empty() {
                        let mut buf = heapless::Vec::new();
                        buf.extend_from_slice(invalid).unwrap();
                        off_char = Some(buf);
                    }
                } else {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "stream did not contain valid UTF-8",
                    ));
                }

                // Shift the un-read part of the buffer to the beginning.
                (&mut reader).consume(buffer_len);
            }

            Err(e) => {
                // Read error
                return Err(e);
            }
        }
    }

    return Ok(builder.finish());
}
