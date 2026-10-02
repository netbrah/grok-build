//! `WebFetchClient` - shared HTTP client with cache, HTML-to-markdown
//! conversion, URL validation, and SSRF protection.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use reqwest::header::{ACCEPT, ACCEPT_LANGUAGE, CONTENT_TYPE, USER_AGENT};
use url::Url;

use super::cache::FetchCache;
use super::config::{MAX_REDIRECTS, MAX_URL_LENGTH, USER_AGENT_STRING, WebFetchParams};
use super::domain::DomainMatcher;
use super::error::WebFetchError;
use super::http::HttpClient;
use super::overflow::{OverflowHandler, RecoveryTools, inline_budget};
use super::ssrf;
use crate::implementations::grok_build::storage::SessionFileWriter;
use crate::types::output::{WebFetchContent, WebFetchOutput, WebFetchSourceArtifact};
use scraper::{Html, Selector};

const DEFAULT_DOWNLOAD_DIR: &str = "downloads";

/// Shared HTTP client and cache for web fetching.
#[derive(Clone)]
pub struct WebFetchClient {
    http: HttpClient,
    cache: Arc<parking_lot::RwLock<FetchCache>>,
    converter: Arc<htmd::HtmlToMarkdown>,
    params: WebFetchParams,
    download_writer: SessionFileWriter,
    image_writer: SessionFileWriter,
    video_writer: SessionFileWriter,
    overflow: OverflowHandler,
    /// Optional Gemini-native utility client for the urlContext HYBRID routing
    /// (apex-ylqz Phase 2, SDD §13.2). `None` (e.g. web_search disabled) keeps
    /// the direct HTTP fetch for every URL.
    gemini: Option<crate::implementations::web_search::client::WebSearchClient>,
    /// Internal-host matcher for the allowlist-for-direct routing
    /// (apex-blvg-A), built from `params.internal_host_allowlist()`. A match
    /// means the host is internal and routes to the DIRECT fetch, bypassing
    /// the Gemini urlContext wire.
    internal_host_matcher: DomainMatcher,
}

struct ProcessedText {
    content: String,
    content_type: String,
    bytes: usize,
    was_truncated: bool,
    artifact_path: Option<PathBuf>,
    inline_fallback: Option<String>,
}

impl WebFetchClient {
    pub fn new(
        params: &WebFetchParams,
        gemini: Option<crate::implementations::web_search::client::WebSearchClient>,
    ) -> Result<Self, WebFetchError> {
        // One-time warn-log (apex-ylqz Phase 2, SDD §13.2): without a
        // Gemini client the urlContext routing is valid-but-inert — every
        // URL keeps the direct HTTP fetch. (apex-blvg-A: an empty
        // internal_host_allowlist is NOT inert — it routes the whole public
        // web to Gemini, so no warn is due there.)
        if params.url_context() && gemini.is_none() {
            tracing::warn!(
                "web_fetch url_context is enabled but no web_search client is available; urlContext routing is inert"
            );
        }
        let converter = Arc::new(
            htmd::HtmlToMarkdown::builder()
                .skip_tags(vec![
                    "script", "style", "noscript", "svg", "iframe", "object", "embed",
                ])
                .build(),
        );

        Ok(Self {
            // Reqwest client can fail to build.
            http: HttpClient::new(params)?,
            cache: Arc::new(parking_lot::RwLock::new(FetchCache::new(
                params.cache_ttl_secs(),
                params.max_cache_entries(),
            ))),
            converter,
            params: params.clone(),
            download_writer: SessionFileWriter::new(DEFAULT_DOWNLOAD_DIR, "pdf"),
            image_writer: SessionFileWriter::new("images", "jpg"),
            video_writer: SessionFileWriter::new("videos", "mp4"),
            overflow: OverflowHandler::new(),
            gemini,
            internal_host_matcher: DomainMatcher::new(&params.internal_host_allowlist()),
        })
    }

    /// Fetch a URL and return its content as markdown. Handles: validation, HTTPS upgrade, SSRF check, HTTP fetch with
    /// same-host redirects, HTML-to-markdown conversion, truncation, and caching. On transport errors, the HTTP client is
    /// invalidated so the next call gets a fresh connection pool (see [`HttpClient`]).
    pub async fn fetch(
        &self,
        raw_url: &str,
        session_folder: Option<&Path>,
        read_tool_name: Option<&str>,
        execute_tool_name: Option<&str>,
    ) -> Result<WebFetchOutput, WebFetchError> {
        let mut url = validate_url(raw_url)?;
        upgrade_to_https(&mut url);

        let url_str = url.to_string();

        // Check cache.
        {
            let cache = self.cache.read();
            if let Some(cached) = cache.get(&url_str) {
                tracing::debug!("web_fetch cache hit for {url_str}");
                return Ok(cached.clone());
            }
        }

        // SSRF check (policy from tool params — not process env at call time).
        ssrf::check_ssrf(&url, self.params.allow_local()).await?;

        // urlContext HYBRID routing (apex-ylqz Phase 2, SDD §13.2): AFTER
        // check_ssrf, so both engines are DNS-guarded. A `None` from
        // `fetch_url_context` falls through to the direct client fetch below —
        // that single client attempt is the fallback (R8: no second LLM hop).
        // A urlContext success is returned WITHOUT a cache insert: the cache
        // key is the bare URL, and a Gemini-grounded answer must not poison
        // the direct-fetch cache (cross-engine cache poisoning, SDD §13.2).
        if self.uses_url_context(&url)
            && let Some(output) = self.fetch_url_context(&url, &url_str).await
        {
            return Ok(output);
        }

        // Make request and build output.
        let http = self.http.get_or_rebuild()?;
        let http_span = tracing::info_span!("web_fetch.http", bytes = tracing::field::Empty);
        let result = match fetch_url(
            &http,
            &url,
            self.params.max_content_length(),
            self.params.allow_local(),
        )
        .await
        {
            Ok(result) => result,
            Err(e @ WebFetchError::HttpRequest(_)) => {
                self.http.invalidate();
                return Err(e);
            }
            Err(e) => return Err(e),
        };

        let (body, content_type, final_url, status_code) = match result {
            FetchResult::Content {
                body,
                content_type,
                final_url,
                status_code,
            } => (body, content_type, final_url, status_code),
            FetchResult::CrossHostRedirect {
                original_host,
                redirect_url,
            } => {
                return Ok(WebFetchOutput::CrossHostRedirect {
                    original_host,
                    redirect_url,
                });
            }
        };
        http_span.record("bytes", body.len() as i64);
        drop(http_span);

        // PDF: save raw bytes to disk instead of lossy UTF-8 conversion.
        if is_pdf(&content_type) {
            let media_session_folder = require_media_session_folder(session_folder)?;
            let output = save_pdf(
                &self.download_writer,
                media_session_folder,
                &body,
                final_url,
                content_type,
                status_code,
                read_tool_name,
            )
            .await?;
            return Ok(output);
        }

        // Image: validate magic bytes, save to disk.
        if is_image(&content_type) {
            if !validate_media_magic_bytes(&content_type, &body) {
                return Err(WebFetchError::ContentTypeMismatch {
                    content_type,
                    url: final_url,
                });
            }
            let media_session_folder = require_media_session_folder(session_folder)?;
            let output = save_image(
                &self.image_writer,
                media_session_folder,
                &body,
                final_url,
                content_type,
                status_code,
                read_tool_name,
            )
            .await?;
            return Ok(output);
        }

        // Video: validate magic bytes, save to disk.
        if is_video(&content_type) {
            if !validate_media_magic_bytes(&content_type, &body) {
                return Err(WebFetchError::ContentTypeMismatch {
                    content_type,
                    url: final_url,
                });
            }
            let media_session_folder = require_media_session_folder(session_folder)?;
            let output = save_video(
                &self.video_writer,
                media_session_folder,
                &body,
                final_url,
                content_type,
                status_code,
            )
            .await?;
            return Ok(output);
        }

        // Reject binary content types that would produce garbage through lossy UTF-8.
        if is_binary_content_type(&content_type) {
            return Err(WebFetchError::UnsupportedContentType {
                content_type,
                url: final_url,
            });
        }

        let render_span = tracing::info_span!("web_fetch.render", bytes = tracing::field::Empty);
        let processed = self
            .process_text_content(
                &body,
                &content_type,
                session_folder,
                RecoveryTools {
                    read: read_tool_name,
                    execute: execute_tool_name,
                },
            )
            .await;
        render_span.record("bytes", processed.bytes as i64);
        drop(render_span);
        let was_truncated = processed.was_truncated;

        let output = WebFetchOutput::Content(WebFetchContent {
            url: final_url,
            content: processed.content,
            content_type: processed.content_type,
            status_code,
            bytes: processed.bytes,
            source_artifact: processed
                .artifact_path
                .map(|path| WebFetchSourceArtifact { path }),
            inline_fallback: processed.inline_fallback,
            output_location: None,
        });

        // Insert into cache.
        {
            let mut cache = self.cache.write();
            cache.insert_text(url_str, output.clone(), was_truncated);
        }

        Ok(output)
    }

    /// The normative urlContext routing predicate (apex-blvg-A — inverting
    /// SDD §13.2's allowlist-for-gemini to allowlist-for-direct): route to
    /// the Gemini `urlContext` wire iff a Gemini-native client is available,
    /// the opt-in is on, the host is NOT a private IP (always-on safety net —
    /// private/loopback hosts never cross the Gemini wire; subsumes the old
    /// `ssrf::is_explicit_local_host` guard) and the host is NOT in the
    /// `internal_host_allowlist` (exact normalized-host or `*.domain` glob;
    /// path-scoped entries count at host level). Everything else — public
    /// web — routes to Gemini. The superseded `url_context_domains` knob is
    /// no longer read by the predicate.
    ///
    /// (apex-blvg-C) the urlContext routing engages only for a Gemini-native
    /// web_search backend (`is_gemini_wire`): a non-Gemini web_search, e.g. a
    /// stock gpt-pinned web_search, stays on the direct path even with
    /// url_context=true.
    fn uses_url_context(&self, url: &Url) -> bool {
        let host = url.host_str().unwrap_or("");
        self.gemini.as_ref().is_some_and(|g| g.is_gemini_wire())
            && self.params.url_context()
            && !ssrf::is_private_ip(host)
            && !self.internal_host_matcher.host_matches(host)
    }

    /// One urlContext attempt (R8: no retry — the caller's direct client fetch
    /// is the fallback). Returns the shaped `WebFetchOutput::Content` on a
    /// grounded, non-empty result; `None` on ANY failure (transport error,
    /// timeout, any non-200 including 401, parse failure, not-grounded, empty
    /// text) so the caller falls back to the direct client fetch.
    async fn fetch_url_context(&self, _url: &Url, url_str: &str) -> Option<WebFetchOutput> {
        let gemini = self.gemini.as_ref()?;
        let prompt = super::url_context::build_prompt(url_str);
        let result = gemini.url_context(&prompt).await.ok()?;
        super::url_context::shape_result(url_str, &result)
    }

    async fn process_text_content(
        &self,
        body: &[u8],
        content_type: &str,
        session_folder: Option<&Path>,
        tools: RecoveryTools<'_>,
    ) -> ProcessedText {
        let raw_content = String::from_utf8_lossy(body);
        let content = if is_html(content_type) {
            html_to_markdown(&self.converter, &raw_content)
        } else {
            raw_content.into_owned()
        };
        let content = strip_base64_data_uris(content);
        let bytes = content.len();
        let output_content_type = if is_html(content_type) {
            "markdown".to_string()
        } else {
            content_type.to_owned()
        };
        let overflow = self
            .overflow
            .process(
                content,
                inline_budget(
                    self.params.context_window_tokens(),
                    self.params.max_markdown_length(),
                ),
                session_folder,
                &output_content_type,
                tools,
            )
            .await;
        ProcessedText {
            content: overflow.content,
            content_type: output_content_type,
            bytes,
            was_truncated: overflow.was_truncated,
            artifact_path: overflow.artifact_path,
            inline_fallback: overflow.path_free_content,
        }
    }
}

// ───────────────────────────────────────────────────────────────────────────
// URL Validation
// ───────────────────────────────────────────────────────────────────────────

/// Validates URL scheme, length, credentials, and hostname labels.
fn validate_url(raw: &str) -> Result<Url, WebFetchError> {
    if raw.len() > MAX_URL_LENGTH {
        return Err(WebFetchError::UrlTooLong {
            max: MAX_URL_LENGTH,
        });
    }

    let parsed = Url::parse(raw)?; // uses #[from] url::ParseError

    match parsed.scheme() {
        "http" | "https" => {}
        scheme => {
            return Err(WebFetchError::UnsupportedScheme {
                scheme: scheme.to_string(),
            });
        }
    }

    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(WebFetchError::CredentialsInUrl);
    }

    if let Some(host) = parsed.host_str()
        && host.split('.').count() < 2
        // `localhost` is a single-label name; SSRF still requires
        // allow_local for explicit local hosts.
        && !ssrf::is_explicit_local_host(host)
    {
        return Err(WebFetchError::SingleLabelHost {
            host: host.to_string(),
        });
    }

    Ok(parsed)
}

/// Upgrade `http://` to `https://`, except for explicit loopback hosts. Local dev servers almost
/// always speak plain HTTP; forcing TLS would break `http://127.0.0.1` / `http://localhost` when
/// local binding is opted in.
fn upgrade_to_https(url: &mut Url) {
    if url.scheme() != "http" {
        return;
    }
    if let Some(host) = url.host_str()
        && ssrf::is_explicit_local_host(host)
    {
        return;
    }
    let _ = url.set_scheme("https");
}

// ───────────────────────────────────────────────────────────────────────────
// HTTP Fetching
// ───────────────────────────────────────────────────────────────────────────

enum FetchResult {
    Content {
        body: Vec<u8>,
        content_type: String,
        final_url: String,
        status_code: u16,
    },
    CrossHostRedirect {
        original_host: String,
        redirect_url: String,
    },
}

/// Fetch a URL with manual same-host redirect handling. Re-runs SSRF checks on every hop so DNS
/// rebinding between redirects cannot sneak a previously-blocked address past the initial check
/// (partial TOCTOU mitigation; peer IP on the live TCP connection is not available from reqwest).
async fn fetch_url(
    client: &reqwest::Client,
    url: &Url,
    max_content_length: usize,
    allow_local: bool,
) -> Result<FetchResult, WebFetchError> {
    let mut current_url = url.clone();
    let mut hops = 0;

    // Loop to follow redirects under the same host.
    loop {
        // Re-check on every hop (including the first) so a rebinding name that
        // was public at the pre-fetch check cannot become loopback/private here.
        ssrf::check_ssrf(&current_url, allow_local).await?;

        let resp = client
            .get(current_url.as_str())
            .header(USER_AGENT, USER_AGENT_STRING)
            .header(
                ACCEPT,
                "text/markdown,text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
            )
            .header(ACCEPT_LANGUAGE, "en-US,en;q=0.9")
            .send()
            .await?;

        let status = resp.status();

        if status.is_redirection() {
            hops += 1;
            if hops > MAX_REDIRECTS {
                return Err(WebFetchError::TooManyRedirects { max: MAX_REDIRECTS });
            }

            // Follow same host; break on cross-host.
            if let Some(location) = resp.headers().get("location") {
                let location_str = location.to_str().unwrap_or("");
                let mut next_url = current_url
                    .join(location_str)
                    .map_err(|e| WebFetchError::InvalidRedirect(format!("{e}")))?;
                if is_same_host(&current_url, &next_url) {
                    // Re-apply https upgrade on every hop: Location may be
                    // absolute `http://…` and would otherwise silently
                    // downgrade an https fetch. Local hosts still skip TLS.
                    upgrade_to_https(&mut next_url);
                    // check_ssrf runs at the top of the next loop iteration.
                    current_url = next_url;
                    continue;
                }
                return Ok(FetchResult::CrossHostRedirect {
                    original_host: current_url.host_str().unwrap_or("unknown").to_string(),
                    redirect_url: next_url.to_string(),
                });
            }
        }

        let content_type = resp
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("text/html")
            .to_string();
        let final_url = resp.url().to_string();
        let status_code = status.as_u16();

        let body = resp.bytes().await?;

        if body.len() > max_content_length {
            return Err(WebFetchError::ResponseTooLarge {
                max: max_content_length,
            });
        }

        return Ok(FetchResult::Content {
            body: body.to_vec(),
            content_type,
            final_url,
            status_code,
        });
    }
}

/// Exact host equality — no `www.` stripping. Distinct DNS labels (even when
/// one is a `www` subdomain of the other) have independent A records and must
/// surface as cross-host redirects rather than auto-follow.
fn is_same_host(a: &Url, b: &Url) -> bool {
    a.host_str() == b.host_str()
}

// ───────────────────────────────────────────────────────────────────────────
// Content Processing
// ───────────────────────────────────────────────────────────────────────────

fn require_media_session_folder(session_folder: Option<&Path>) -> Result<&Path, WebFetchError> {
    session_folder.ok_or_else(|| {
        WebFetchError::IoError(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "session folder is unavailable",
        ))
    })
}

fn is_html(content_type: &str) -> bool {
    content_type.contains("text/html") || content_type.contains("application/xhtml")
}

fn is_pdf(content_type: &str) -> bool {
    content_type.contains("application/pdf")
}

/// Returns `true` for image content types, excluding SVG (which can contain
/// `<script>` tags and event handlers — an XSS vector if saved and opened).
fn is_image(content_type: &str) -> bool {
    let mime = content_type
        .split(';')
        .next()
        .unwrap_or(content_type)
        .trim()
        .to_ascii_lowercase();
    mime.starts_with("image/") && mime != "image/svg+xml"
}

fn is_video(content_type: &str) -> bool {
    content_type
        .split(';')
        .next()
        .unwrap_or(content_type)
        .trim()
        .to_ascii_lowercase()
        .starts_with("video/")
}

/// Validate that the first bytes of `body` match the magic bytes expected for
/// the claimed `content_type`. Returns `true` when the bytes match or when the
/// subtype is unknown (fail-open for niche formats).
fn validate_media_magic_bytes(content_type: &str, body: &[u8]) -> bool {
    let mime = content_type
        .split(';')
        .next()
        .unwrap_or(content_type)
        .trim()
        .to_ascii_lowercase();
    match mime.as_str() {
        "image/png" => body.starts_with(&[0x89, 0x50, 0x4E, 0x47]),
        "image/jpeg" => body.starts_with(&[0xFF, 0xD8, 0xFF]),
        "image/gif" => body.starts_with(b"GIF8"),
        "image/webp" => body.len() >= 12 && &body[..4] == b"RIFF" && &body[8..12] == b"WEBP",
        "video/mp4" => body.len() >= 8 && &body[4..8] == b"ftyp",
        "video/webm" => body.starts_with(&[0x1A, 0x45, 0xDF, 0xA3]),
        _ => true, // unknown subtypes: allow (fail-open for niche formats)
    }
}

/// Map a media Content-Type to the correct file extension.
fn media_extension(content_type: &str) -> &'static str {
    let mime = content_type
        .split(';')
        .next()
        .unwrap_or(content_type)
        .trim()
        .to_ascii_lowercase();
    match mime.as_str() {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/bmp" => "bmp",
        "image/tiff" => "tiff",
        "video/mp4" => "mp4",
        "video/webm" => "webm",
        "video/quicktime" => "mov",
        "video/x-msvideo" => "avi",
        _ => "bin",
    }
}

/// Returns `true` for content types that are binary and would produce garbage through
/// `String::from_utf8_lossy`. Text-like types (`text/*`, `application/json`, `application/xml`,
/// `application/javascript`, etc.) return `false`.
fn is_binary_content_type(content_type: &str) -> bool {
    let mime = content_type
        .split(';')
        .next()
        .unwrap_or(content_type)
        .trim()
        .to_ascii_lowercase();

    if mime.starts_with("text/") {
        return false;
    }
    !matches!(
        mime.as_str(),
        "application/json"
            | "application/xml"
            | "application/javascript"
            | "application/ecmascript"
            | "application/x-javascript"
            | "application/xhtml+xml"
            | "application/rss+xml"
            | "application/atom+xml"
            | "application/soap+xml"
            | "application/xslt+xml"
            | "application/mathml+xml"
            | "application/svg+xml"
            | "application/x-www-form-urlencoded"
            | "application/graphql"
            | "application/ld+json"
            | "application/schema+json"
            | "application/vnd.api+json"
            | "application/x-yaml"
            | "application/yaml"
            | "application/toml"
    )
}

/// Save fetched PDF bytes to the session download directory and return a
/// `WebFetchOutput` pointing the model at the saved file.
async fn save_pdf(
    writer: &SessionFileWriter,
    session_folder: &Path,
    body: &[u8],
    final_url: String,
    content_type: String,
    status_code: u16,
    read_tool_name: Option<&str>,
) -> Result<WebFetchOutput, WebFetchError> {
    let path = writer
        .save(session_folder, body, None)
        .await
        .map_err(|e| WebFetchError::IoError(std::io::Error::other(e.to_string())))?;

    tracing::info!(
        path = %path.display(),
        bytes = body.len(),
        "PDF saved to disk"
    );

    let read_hint = read_tool_name.map_or_else(String::new, |name| {
        format!(" Use the {name} tool to view its contents.")
    });
    let content = format!(
        "PDF downloaded ({} bytes) and saved to {}.{}",
        body.len(),
        path.display(),
        read_hint,
    );
    Ok(WebFetchOutput::Content(WebFetchContent {
        url: final_url,
        content: content.clone(),
        content_type,
        status_code,
        bytes: body.len(),
        source_artifact: None,
        inline_fallback: Some(content),
        output_location: None,
    }))
}

/// Save fetched image bytes to the session images directory and return a
/// `WebFetchOutput` pointing the model at the saved file.
async fn save_image(
    writer: &SessionFileWriter,
    session_folder: &Path,
    body: &[u8],
    final_url: String,
    content_type: String,
    status_code: u16,
    read_tool_name: Option<&str>,
) -> Result<WebFetchOutput, WebFetchError> {
    let ext = media_extension(&content_type);
    let path = writer
        .save(session_folder, body, Some(ext))
        .await
        .map_err(|e| WebFetchError::IoError(std::io::Error::other(e.to_string())))?;

    tracing::info!(
        path = %path.display(),
        bytes = body.len(),
        "Image saved to disk"
    );

    let read_hint = read_tool_name.map_or_else(String::new, |name| {
        format!(" Use the {name} tool to view its contents.")
    });
    let content = format!(
        "Image downloaded ({} bytes, {}) and saved to {}.{}",
        body.len(),
        content_type,
        path.display(),
        read_hint,
    );
    Ok(WebFetchOutput::Content(WebFetchContent {
        url: final_url,
        content: content.clone(),
        content_type,
        status_code,
        bytes: body.len(),
        source_artifact: None,
        inline_fallback: Some(content),
        output_location: None,
    }))
}

/// Save fetched video bytes to the session videos directory and return a
/// `WebFetchOutput` pointing the model at the saved file.
async fn save_video(
    writer: &SessionFileWriter,
    session_folder: &Path,
    body: &[u8],
    final_url: String,
    content_type: String,
    status_code: u16,
) -> Result<WebFetchOutput, WebFetchError> {
    let ext = media_extension(&content_type);
    let path = writer
        .save(session_folder, body, Some(ext))
        .await
        .map_err(|e| WebFetchError::IoError(std::io::Error::other(e.to_string())))?;

    tracing::info!(
        path = %path.display(),
        bytes = body.len(),
        "Video saved to disk"
    );

    let content = format!(
        "Video downloaded ({} bytes, {}) and saved to {}.",
        body.len(),
        content_type,
        path.display(),
    );
    Ok(WebFetchOutput::Content(WebFetchContent {
        url: final_url,
        content: content.clone(),
        content_type,
        status_code,
        bytes: body.len(),
        source_artifact: None,
        inline_fallback: Some(content),
        output_location: None,
    }))
}

fn html_to_markdown(converter: &htmd::HtmlToMarkdown, html: &str) -> String {
    let cleaned = clean_html(html);
    converter
        .convert(&cleaned)
        .unwrap_or_else(|_| html.to_string())
}

/// Remove common noisy elements from HTML before markdown conversion.
fn clean_html(html: &str) -> String {
    let mut document = Html::parse_document(html);

    // Never detach the root element itself - the broad selectors below can match
    // attributes on <html> (e.g. class="...advert..."), which would leave the
    // tree with no root element. Looked up fallibly so we never assume one exists.
    let root_id = document
        .tree
        .root()
        .children()
        .find(|child| child.value().is_element())
        .map(|node| node.id());

    let selectors: Vec<Selector> = [
        "nav",
        "header",
        "footer",
        "[class*='cookie']",
        "[class*='sidebar']",
        "[class*='ad-']",
        "[class*='advert']",
        "[id*='cookie']",
        "[id*='sidebar']",
        "[id*='ad-']",
        "[id*='advert']",
    ]
    .iter()
    .filter_map(|s| Selector::parse(s).ok())
    .collect();

    selectors.iter().for_each(|selector| {
        document
            .select(selector)
            .map(|e| e.id())
            .collect::<Vec<_>>()
            .into_iter()
            .for_each(|id| {
                if Some(id) == root_id {
                    return;
                }
                if let Some(mut node) = document.tree.get_mut(id) {
                    node.detach();
                }
            });
    });

    // Serialize the whole document rather than `root_element()`, which panics if
    // the tree somehow has no root element.
    document.html()
}

/// Strip base64 data URIs from content to prevent token bloat. Uses manual scanning (`find` + byte
/// matching) instead of regex for lower overhead — no compilation cost and O(n) linear scanning.
fn strip_base64_data_uris(content: String) -> String {
    // A valid base64 quantum is 4 characters; anything shorter is noise.
    const MIN_BASE64_PAYLOAD: usize = 4;
    // RFC 2397 headers are short (MIME + parameters); reject oversized ones
    // to avoid echoing attacker-controlled megabyte strings into the output.
    const MAX_HEADER_LEN: usize = 120;

    if !content.contains("data:") {
        return content;
    }

    let s = content.as_str();
    let mut result = String::with_capacity(s.len());
    let mut last_end = 0;
    let mut search_from = 0;

    while let Some(rel) = s[search_from..].find("data:") {
        let start = search_from + rel;

        // "data:" must look like a URI scheme start, not a substring of
        // another word (e.g. "metadata:", "validata:").
        if start > 0 && s.as_bytes()[start - 1].is_ascii_alphanumeric() {
            search_from = start + 5;
            continue;
        }

        if let Some(rel_comma) = s[start..].find(',') {
            let comma = start + rel_comma;
            let header = &s[start + 5..comma];

            // RFC 2397 forbids whitespace in the header, and real headers
            // are short ASCII. Reject anything that violates this.
            if header.len() > MAX_HEADER_LEN || header.bytes().any(|b| b.is_ascii_whitespace()) {
                search_from = start + 5;
                continue;
            }

            let mut parts = header.split(';');
            let mime = parts
                .next()
                .expect("split always yields at least one element");
            let mime = if mime.is_empty() { "unknown" } else { mime };

            if parts.any(|p| p.eq_ignore_ascii_case("base64")) {
                // Consume valid base64 characters after the comma.
                let payload_start = comma + 1;
                let payload_len = s[payload_start..]
                    .bytes()
                    .take_while(|b| {
                        matches!(b, b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'+' | b'/' | b'=')
                    })
                    .count();

                if payload_len >= MIN_BASE64_PAYLOAD {
                    result.push_str(&s[last_end..start]);
                    result.push_str("[base64 ");
                    result.push_str(mime);
                    result.push_str(" data removed]");
                    last_end = payload_start + payload_len;
                    search_from = last_end;
                    continue;
                }
            }
        }

        search_from = start + 5;
    }

    if last_end == 0 {
        return content;
    }
    result.push_str(&s[last_end..]);
    result
}

// ───────────────────────────────────────────────────────────────────────────
// Tests
// ───────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn test_converter() -> htmd::HtmlToMarkdown {
        htmd::HtmlToMarkdown::builder()
            .skip_tags(vec![
                "script", "style", "noscript", "svg", "iframe", "object", "embed",
            ])
            .build()
    }

    #[tokio::test]
    async fn oversized_html_persists_exact_pre_truncation_markdown() {
        let tmp = tempfile::tempdir().unwrap();
        let client = WebFetchClient::new(
            &WebFetchParams {
                context_window_tokens: Some(100),
                ..WebFetchParams::default()
            },
            None,
        )
        .unwrap();
        let tail = "TAIL-MUST-REMAIN-RECOVERABLE";
        let html = format!(
            "<h1>Heading</h1>{}<p>{tail}</p>",
            "<p>body line</p>".repeat(1_000)
        );
        let expected = strip_base64_data_uris(html_to_markdown(&test_converter(), &html));

        let processed = client
            .process_text_content(
                html.as_bytes(),
                "text/html; charset=utf-8",
                Some(tmp.path()),
                RecoveryTools {
                    read: Some("ReadAsset"),
                    execute: Some("ExecuteAsset"),
                },
            )
            .await;

        assert!(processed.was_truncated);
        assert_eq!(processed.content_type, "markdown");
        assert_eq!(processed.bytes, expected.len());
        assert!(!processed.content.contains(tail));
        assert!(expected.contains(tail));
        let artifact = tmp.path().join("web_fetch/1.md");
        assert!(
            processed
                .content
                .contains(artifact.to_string_lossy().as_ref())
        );
        assert_eq!(tokio::fs::read_to_string(artifact).await.unwrap(), expected);
    }

    // ── URL validation ──────────────────────────────────────────────────

    #[test]
    fn validate_url_accepts_valid() {
        assert!(validate_url("https://docs.rs/reqwest/latest").is_ok());
        assert!(validate_url("https://github.com/seanmonstar/reqwest").is_ok());
        assert!(validate_url("http://example.com/path?q=1#frag").is_ok());
    }

    #[test]
    fn validate_url_rejects_single_label_hosts() {
        // localhost is an explicit local host; SSRF still blocks it unless
        // allow_local is set on tool params.
        assert!(validate_url("http://localhost:8080/foo").is_ok());
        assert!(validate_url("http://intranet/foo").is_err());
        assert!(validate_url("http://metadata/computeMetadata").is_err());
    }

    #[test]
    fn upgrade_to_https_skips_explicit_local_hosts() {
        let mut local = Url::parse("http://127.0.0.1:8080/").unwrap();
        upgrade_to_https(&mut local);
        assert_eq!(local.scheme(), "http");

        let mut localhost = Url::parse("http://localhost:3000/").unwrap();
        upgrade_to_https(&mut localhost);
        assert_eq!(localhost.scheme(), "http");

        let mut public = Url::parse("http://example.com/").unwrap();
        upgrade_to_https(&mut public);
        assert_eq!(public.scheme(), "https");
    }

    #[test]
    fn validate_url_rejects_credentials() {
        assert!(validate_url("https://user:pass@example.com/foo").is_err());
        assert!(validate_url("https://admin@example.com/foo").is_err());
    }

    #[test]
    fn validate_url_rejects_long_urls() {
        let long = format!("https://example.com/{}", "a".repeat(MAX_URL_LENGTH));
        assert!(validate_url(&long).is_err());
    }

    #[test]
    fn validate_url_rejects_unsupported_schemes() {
        assert!(validate_url("ftp://example.com/file.txt").is_err());
        assert!(validate_url("file:///etc/passwd").is_err());
        assert!(validate_url("data:text/html,<h1>hi</h1>").is_err());
    }

    #[test]
    fn validate_url_rejects_garbage() {
        assert!(validate_url("not a url").is_err());
        assert!(validate_url("").is_err());
    }

    #[test]
    fn upgrade_http_to_https() {
        let mut url = Url::parse("http://example.com/path").unwrap();
        upgrade_to_https(&mut url);
        assert_eq!(url.scheme(), "https");
    }

    #[test]
    fn upgrade_https_is_noop() {
        let mut url = Url::parse("https://example.com/path").unwrap();
        upgrade_to_https(&mut url);
        assert_eq!(url.scheme(), "https");
    }

    // ── Same-host redirect check ────────────────────────────────────────

    #[test]
    fn same_host_exact_match() {
        let a = Url::parse("https://example.com/a").unwrap();
        let b = Url::parse("https://example.com/b").unwrap();
        assert!(is_same_host(&a, &b));
    }

    #[test]
    fn www_subdomain_is_cross_host() {
        let a = Url::parse("https://example.com/a").unwrap();
        let c = Url::parse("https://www.example.com/a").unwrap();
        assert!(!is_same_host(&a, &c));
        assert!(!is_same_host(&c, &a));
    }

    #[test]
    fn different_host_rejected() {
        let a = Url::parse("https://example.com/a").unwrap();
        let d = Url::parse("https://other.com/a").unwrap();
        assert!(!is_same_host(&a, &d));
    }

    #[test]
    fn same_host_redirect_location_reupgrades_http() {
        // Absolute http Location on an https origin must not stay http when
        // followed as a same-host hop (upgrade_to_https reapplied each hop).
        let origin = Url::parse("https://example.com/start").unwrap();
        let mut next = origin.join("http://example.com/next").unwrap();
        assert_eq!(next.scheme(), "http");
        assert!(is_same_host(&origin, &next));
        upgrade_to_https(&mut next);
        assert_eq!(next.scheme(), "https");
        assert_eq!(next.as_str(), "https://example.com/next");
    }

    // ── Content type detection ──────────────────────────────────────────

    #[test]
    fn is_html_detects_html_types() {
        assert!(is_html("text/html"));
        assert!(is_html("text/html; charset=utf-8"));
        assert!(is_html("application/xhtml+xml"));
    }

    #[test]
    fn is_html_rejects_non_html() {
        assert!(!is_html("text/plain"));
        assert!(!is_html("text/markdown"));
        assert!(!is_html("application/json"));
        assert!(!is_html("application/pdf"));
    }

    // ── PDF content type detection ────────────────────────────────────

    #[test]
    fn is_pdf_detects_pdf_types() {
        assert!(is_pdf("application/pdf"));
        assert!(is_pdf("application/pdf; charset=utf-8"));
    }

    #[test]
    fn is_pdf_rejects_non_pdf() {
        assert!(!is_pdf("text/html"));
        assert!(!is_pdf("text/plain"));
        assert!(!is_pdf("application/json"));
        assert!(!is_pdf("application/octet-stream"));
    }

    // ── Binary content type detection ────────────────────────────────

    #[test]
    fn binary_detects_images_and_media() {
        assert!(is_binary_content_type("image/png"));
        assert!(is_binary_content_type("image/jpeg"));
        assert!(is_binary_content_type("image/gif"));
        assert!(is_binary_content_type("audio/mpeg"));
        assert!(is_binary_content_type("video/mp4"));
        assert!(is_binary_content_type("application/octet-stream"));
        assert!(is_binary_content_type("application/zip"));
        assert!(is_binary_content_type("application/wasm"));
        assert!(is_binary_content_type("font/woff2"));
    }

    #[test]
    fn binary_allows_text_types() {
        assert!(!is_binary_content_type("text/plain"));
        assert!(!is_binary_content_type("text/html"));
        assert!(!is_binary_content_type("text/html; charset=utf-8"));
        assert!(!is_binary_content_type("text/markdown"));
        assert!(!is_binary_content_type("text/csv"));
        assert!(!is_binary_content_type("text/xml"));
    }

    #[test]
    fn binary_allows_text_like_application_types() {
        assert!(!is_binary_content_type("application/json"));
        assert!(!is_binary_content_type("application/json; charset=utf-8"));
        assert!(!is_binary_content_type("application/xml"));
        assert!(!is_binary_content_type("application/javascript"));
        assert!(!is_binary_content_type("application/xhtml+xml"));
        assert!(!is_binary_content_type("application/svg+xml"));
        assert!(!is_binary_content_type("application/ld+json"));
        assert!(!is_binary_content_type("application/yaml"));
        assert!(!is_binary_content_type("application/graphql"));
    }

    // ── HTML to markdown conversion ─────────────────────────────────────

    #[test]
    fn html_to_markdown_basic() {
        let md = html_to_markdown(&test_converter(), "<h1>Hello</h1><p>World</p>");
        assert!(md.contains("Hello"));
        assert!(md.contains("World"));
    }

    #[test]
    fn html_to_markdown_strips_script_and_style() {
        let html = r#"<h1>Title</h1>
            <script>var x = 1; alert("hi");</script>
            <style>body { color: red; }</style>
            <p>Content</p>"#;
        let md = html_to_markdown(&test_converter(), html);
        assert!(md.contains("Title"));
        assert!(md.contains("Content"));
        assert!(!md.contains("alert"), "script content should be stripped");
        assert!(
            !md.contains("color: red"),
            "style content should be stripped"
        );
    }

    #[test]
    fn html_to_markdown_strips_noscript() {
        let html = "<p>Visible</p><noscript>Enable JavaScript</noscript>";
        let md = html_to_markdown(&test_converter(), html);
        assert!(md.contains("Visible"));
        assert!(
            !md.contains("Enable JavaScript"),
            "noscript content should be stripped"
        );
    }

    #[test]
    fn html_to_markdown_strips_svg() {
        let html = r#"<p>Text</p><svg xmlns="http://www.w3.org/2000/svg"><circle r="5"/></svg>"#;
        let md = html_to_markdown(&test_converter(), html);
        assert!(md.contains("Text"));
        assert!(!md.contains("circle"), "SVG content should be stripped");
    }

    #[test]
    fn html_to_markdown_preserves_links() {
        let html = r#"<a href="https://example.com">Click here</a>"#;
        let md = html_to_markdown(&test_converter(), html);
        assert!(md.contains("[Click here]"));
        assert!(md.contains("(https://example.com)"));
    }

    #[test]
    fn html_to_markdown_preserves_code_blocks() {
        let html = "<pre><code>fn main() {}</code></pre>";
        let md = html_to_markdown(&test_converter(), html);
        assert!(md.contains("fn main() {}"));
    }

    #[test]
    fn html_to_markdown_preserves_lists() {
        let html = "<ul><li>One</li><li>Two</li><li>Three</li></ul>";
        let md = html_to_markdown(&test_converter(), html);
        assert!(md.contains("One"));
        assert!(md.contains("Two"));
        assert!(md.contains("Three"));
    }

    #[test]
    fn html_to_markdown_handles_nested_elements() {
        let html = r#"<p>This is <strong>bold <em>and italic</em></strong> text.</p>"#;
        let md = html_to_markdown(&test_converter(), html);
        assert!(md.contains("bold"));
        assert!(md.contains("and italic"));
    }

    #[test]
    fn html_to_markdown_handles_empty_input() {
        let md = html_to_markdown(&test_converter(), "");
        assert!(md.is_empty() || md.trim().is_empty());
    }

    #[test]
    fn html_to_markdown_strips_iframe() {
        let html = r#"<p>Content</p><iframe src="https://evil.com/embed"></iframe><p>More</p>"#;
        let md = html_to_markdown(&test_converter(), html);
        assert!(md.contains("Content"));
        assert!(md.contains("More"));
        assert!(!md.contains("evil.com"), "iframe should be stripped");
    }

    #[test]
    fn html_to_markdown_full_page_strips_boilerplate() {
        let html = r#"<!DOCTYPE html>
<html>
<head>
  <title>Test Page</title>
  <script>var tracking = true;</script>
  <style>body { margin: 0; }</style>
</head>
<body>
  <nav><a href="/">Home</a><a href="/about">About</a></nav>
  <main>
    <h1>Article Title</h1>
    <p>This is the main content of the article.</p>
  </main>
  <footer>Copyright 2025</footer>
  <script>analytics.track('pageview');</script>
</body>
</html>"#;
        let md = html_to_markdown(&test_converter(), html);
        assert!(md.contains("Article Title"), "article title should survive");
        assert!(md.contains("main content"), "article body should survive");
        assert!(
            !md.contains("tracking"),
            "head script content should be stripped"
        );
        assert!(
            !md.contains("analytics.track"),
            "body script content should be stripped"
        );
        assert!(
            !md.contains("margin: 0"),
            "style content should be stripped"
        );
    }

    #[test]
    fn html_to_markdown_handles_tables() {
        let html = r#"<table>
            <tr><th>Name</th><th>Value</th></tr>
            <tr><td>Alpha</td><td>1</td></tr>
            <tr><td>Beta</td><td>2</td></tr>
        </table>"#;
        let md = html_to_markdown(&test_converter(), html);
        assert!(md.contains("Name"));
        assert!(md.contains("Alpha"));
        assert!(md.contains("Beta"));
    }

    // ── Base64 data URI stripping ─────────────────────────────────────

    /// Golden test: verify exact output format for the most common case.
    #[test]
    fn strip_base64_output_format() {
        let result = strip_base64_data_uris(
            "Before ![logo](data:image/png;base64,iVBORw0KGgoAAAANSUhEUg==) after".to_owned(),
        );
        assert_eq!(
            result,
            "Before ![logo]([base64 image/png data removed]) after"
        );
    }

    /// Inputs the scanner must leave untouched.
    #[test]
    fn strip_base64_rejects_invalid() {
        let cases: &[&str] = &[
            // whitespace in header (RFC 2397 forbids)
            "data: image/png ;base64,AAAA== end",
            // payload too short (< 4 chars)
            "data:image/png;base64,AA= end",
            // empty payload
            "data:image/png;base64, end",
            // no comma after data:
            "data:image/png;base64 with no comma",
            // truncated data: at EOF
            "trailing data:",
        ];
        for input in cases {
            assert_eq!(
                strip_base64_data_uris(input.to_string()),
                *input,
                "should not strip: {input:?}"
            );
        }
    }

    /// Oversized header must be rejected (can't be a static string).
    #[test]
    fn strip_base64_rejects_oversized_header() {
        let huge_mime = "x".repeat(10_000);
        let input = format!("data:{huge_mime};base64,AAAA== end");
        assert_eq!(strip_base64_data_uris(input.clone()), input);
    }

    /// Multi-param headers and case-insensitive marker — capabilities
    /// the manual scanner has beyond the original regex.
    #[test]
    fn strip_base64_extended_headers() {
        assert_eq!(
            strip_base64_data_uris("data:text/plain;charset=utf-8;base64,SGVsbG8= end".to_owned()),
            "[base64 text/plain data removed] end"
        );
        assert!(
            strip_base64_data_uris("data:image/png;Base64,AAAA==".to_owned())
                .contains("[base64 image/png data removed]")
        );
    }

    // ── Regex equivalence ──────────────────────────────────────────────

    /// Reference implementation: the original regex-based stripper.
    fn strip_base64_data_uris_regex(content: &str) -> String {
        let re = regex::Regex::new(r"data:([^;,\s]{1,80});base64,[A-Za-z0-9+/=]+")
            .expect("valid base64 data URI regex");
        re.replace_all(content, |caps: &regex::Captures| {
            let mime = caps.get(1).map_or("unknown", |m| m.as_str());
            format!("[base64 {mime} data removed]")
        })
        .into_owned()
    }

    /// Both implementations must produce identical output on all realistic inputs. Covers: markdown
    /// images, standalone URIs, multiple URIs, normal URLs, non-base64 data URIs, HTML/CSS
    /// contexts, various positions, and real-world payloads.
    #[test]
    fn strip_base64_equivalence_with_regex() {
        let cases: &[&str] = &[
            "no data URIs here",
            "![logo](data:image/png;base64,iVBORw0KGgoAAAANSUhEUg==) after",
            "See data:image/svg+xml;base64,PHN2ZyB4bWxucz0iaHR0 here",
            "![a](data:image/png;base64,AAAA==) text ![b](data:image/jpeg;base64,/9j/4AAQ==)",
            "![img](https://example.com/logo.png) and [link](https://example.com)",
            "data:text/html,%3Ch1%3EHello%3C%2Fh1%3E",
            "Just normal markdown with **bold** and [link](https://x.com)",
            r#"<img src="data:image/png;base64,iVBORw0KGgo="> text"#,
            "background: url(data:image/svg+xml;base64,PHN2Zz4=) no-repeat",
            "before data:image/gif;base64,R0lGODlhAQABAIAAAAAAAP///yH5BAEAAAAALAAAAAABAAEAAAIBRAA7 after",
            "data:image/png;base64,AA==",
            "data:application/octet-stream;base64,AQID",
            "x data:a/b;base64,AAAA== y data:c/d;base64,BBBB== z",
            "data:image/png;base64,AAAA== rest",
            "rest data:image/png;base64,AAAA==",
            "data:image/png;base64,AAAA==",
            "data:image/x-icon;base64,AAABAAEAEBAQAAEABAAoAQAAFgAAACgAAAAQAAAAIAAAAAEABAAAAAAAgAAA",
        ];
        for input in cases {
            let regex_out = strip_base64_data_uris_regex(input);
            let manual_out = strip_base64_data_uris(input.to_string());
            assert_eq!(manual_out, regex_out, "mismatch on input: {input:?}");
        }
    }

    /// The manual scanner correctly rejects word-internal "data:" prefixes
    /// that the regex would false-positive on.
    #[test]
    fn strip_base64_rejects_word_internal_data_prefix() {
        for input in ["metadata:foo;base64,AAAA==", "validata:bar;base64,BBBB=="] {
            let manual_out = strip_base64_data_uris(input.to_string());
            let regex_out = strip_base64_data_uris_regex(input);
            assert_eq!(
                manual_out, input,
                "manual scanner should NOT strip: {input:?}"
            );
            assert_ne!(
                regex_out, manual_out,
                "expected divergence from regex on: {input:?}"
            );
        }
    }

    // ── Proxy configs ─────────────────────────────────────

    #[test]
    fn proxy_endpoint_round_trips_through_config() {
        let json = r#"{"proxy_endpoint": "https://proxy.corp.example.com", "allowed_domains": ["example.com"]}"#;
        let params: WebFetchParams = serde_json::from_str(json).unwrap();
        assert_eq!(
            params.proxy_endpoint.as_deref(),
            Some("https://proxy.corp.example.com")
        );

        // Client builds successfully with the proxy endpoint set.
        let client = WebFetchClient::new(&params, None);
        assert!(client.is_ok());
    }

    #[test]
    fn proxy_endpoint_defaults_to_none() {
        let json = r#"{"allowed_domains": ["example.com"]}"#;
        let params: WebFetchParams = serde_json::from_str(json).unwrap();
        assert!(params.proxy_endpoint.is_none());
    }

    // ── HTML cleaning (scraper-based) ─────────────────────────────────

    #[test]
    fn clean_html_removes_boilerplate_tags() {
        let html = r#"<body><nav>Menu</nav><header>Header</header><main>Content</main><footer>Footer</footer></body>"#;
        let cleaned = clean_html(html);
        assert!(cleaned.contains("Content"));
        assert!(!cleaned.contains("Menu"));
        assert!(!cleaned.contains("Header"));
        assert!(!cleaned.contains("Footer"));
    }

    #[test]
    fn clean_html_removes_noisy_classes_and_ids() {
        let html = r#"<div class="cookie-banner">cookies</div><aside id="sidebar">side</aside><div class="ad-banner">ad</div>"#;
        let cleaned = clean_html(html);
        assert!(!cleaned.contains("cookie-banner"));
        assert!(!cleaned.contains("sidebar"));
        assert!(!cleaned.contains("ad-banner"));
    }

    #[test]
    fn clean_html_keeps_root_when_it_matches_selector() {
        let html =
            r#"<html class="advert"><body><nav>Menu</nav><main>Content</main></body></html>"#;
        let cleaned = clean_html(html);
        assert!(cleaned.contains("Content"));
        assert!(!cleaned.contains("Menu"));
    }

    // ── Image content type detection ──────────────────────────────────

    #[test]
    fn is_image_detects_image_types() {
        assert!(is_image("image/png"));
        assert!(is_image("image/jpeg"));
        assert!(is_image("image/gif"));
        assert!(is_image("image/webp"));
        assert!(is_image("image/bmp"));
        assert!(is_image("image/tiff"));
        assert!(is_image("image/png; charset=utf-8"));
    }

    #[test]
    fn is_image_rejects_svg() {
        assert!(!is_image("image/svg+xml"));
        assert!(!is_image("image/svg+xml; charset=utf-8"));
    }

    #[test]
    fn is_image_rejects_non_image() {
        assert!(!is_image("text/html"));
        assert!(!is_image("video/mp4"));
        assert!(!is_image("application/pdf"));
        assert!(!is_image("application/octet-stream"));
    }

    // ── Video content type detection ──────────────────────────────────

    #[test]
    fn is_video_detects_video_types() {
        assert!(is_video("video/mp4"));
        assert!(is_video("video/webm"));
        assert!(is_video("video/quicktime"));
        assert!(is_video("video/x-msvideo"));
        assert!(is_video("video/mp4; codecs=avc1"));
    }

    #[test]
    fn is_video_rejects_non_video() {
        assert!(!is_video("text/html"));
        assert!(!is_video("image/png"));
        assert!(!is_video("application/pdf"));
        assert!(!is_video("audio/mpeg"));
    }

    // ── Magic byte validation ─────────────────────────────────────────

    #[test]
    fn magic_bytes_valid_png() {
        let png = [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
        assert!(validate_media_magic_bytes("image/png", &png));
    }

    #[test]
    fn magic_bytes_valid_jpeg() {
        let jpeg = [0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10];
        assert!(validate_media_magic_bytes("image/jpeg", &jpeg));
    }

    #[test]
    fn magic_bytes_valid_gif() {
        assert!(validate_media_magic_bytes("image/gif", b"GIF89a\x01\x00"));
        assert!(validate_media_magic_bytes("image/gif", b"GIF87a\x01\x00"));
    }

    #[test]
    fn magic_bytes_valid_webp() {
        let mut webp = vec![0u8; 12];
        webp[..4].copy_from_slice(b"RIFF");
        webp[8..12].copy_from_slice(b"WEBP");
        assert!(validate_media_magic_bytes("image/webp", &webp));
    }

    #[test]
    fn magic_bytes_valid_mp4() {
        let mut mp4 = vec![0u8; 12];
        mp4[4..8].copy_from_slice(b"ftyp");
        assert!(validate_media_magic_bytes("video/mp4", &mp4));
    }

    #[test]
    fn magic_bytes_valid_webm() {
        let webm = [0x1A, 0x45, 0xDF, 0xA3, 0x01, 0x00];
        assert!(validate_media_magic_bytes("video/webm", &webm));
    }

    #[test]
    fn magic_bytes_rejects_mismatch() {
        // HTML content masquerading as PNG
        assert!(!validate_media_magic_bytes("image/png", b"<html>"));
        // PNG bytes claimed as JPEG
        assert!(!validate_media_magic_bytes(
            "image/jpeg",
            &[0x89, 0x50, 0x4E, 0x47]
        ));
        // Empty body
        assert!(!validate_media_magic_bytes("image/png", &[]));
        // Text claimed as video
        assert!(!validate_media_magic_bytes("video/mp4", b"not video"));
    }

    #[test]
    fn magic_bytes_allows_unknown_subtypes() {
        // Unknown image/video subtypes should pass (fail-open)
        assert!(validate_media_magic_bytes("image/x-custom", b"anything"));
        assert!(validate_media_magic_bytes("video/x-custom", b"anything"));
    }

    // ── Media extension mapping ───────────────────────────────────────

    #[test]
    fn media_extension_maps_known_types() {
        assert_eq!(media_extension("image/png"), "png");
        assert_eq!(media_extension("image/jpeg"), "jpg");
        assert_eq!(media_extension("image/gif"), "gif");
        assert_eq!(media_extension("image/webp"), "webp");
        assert_eq!(media_extension("image/bmp"), "bmp");
        assert_eq!(media_extension("image/tiff"), "tiff");
        assert_eq!(media_extension("video/mp4"), "mp4");
        assert_eq!(media_extension("video/webm"), "webm");
        assert_eq!(media_extension("video/quicktime"), "mov");
        assert_eq!(media_extension("video/x-msvideo"), "avi");
    }

    #[test]
    fn media_extension_handles_params() {
        assert_eq!(media_extension("image/png; charset=utf-8"), "png");
        assert_eq!(media_extension("video/mp4; codecs=avc1"), "mp4");
    }

    #[test]
    fn media_extension_unknown_falls_back_to_bin() {
        assert_eq!(media_extension("image/x-custom"), "bin");
        assert_eq!(media_extension("video/x-custom"), "bin");
        assert_eq!(media_extension("application/octet-stream"), "bin");
    }

    // ── urlContext HYBRID (apex-ylqz Phase 2, SDD §13.2) ─────────────────

    const UC_MODEL: &str = "gemini-test-model";
    const UC_PATH: &str = "/v1beta/models/gemini-test-model:generateContent";
    /// Public IP literal: no DNS (check_ssrf checks literals directly), not an
    /// explicit local host. Never actually dialed in these tests — the client
    /// path is pointed at a closed loopback proxy so any fall-through fails
    /// fast and locally.
    const UC_URL: &str = "https://93.184.216.34/";
    /// Closed loopback proxy: any direct client fetch fails fast and
    /// deterministically (connection refused) without touching the network.
    const UC_DEAD_PROXY: &str = "http://127.0.0.1:1";

    type WebSearchClient = crate::implementations::web_search::client::WebSearchClient;

    fn uc_gemini_client(base_url: &str) -> WebSearchClient {
        uc_gemini_client_model(base_url, UC_MODEL)
    }

    fn uc_gemini_client_model(base_url: &str, model: &str) -> WebSearchClient {
        // (apex-blvg-C) same stub as `uc_gemini_client` with a selectable
        // model slug, to exercise the is_gemini_wire guard on the
        // urlContext routing predicate.
        let config = crate::implementations::web_search::WebSearchConfig::Enabled {
            api_key: "test-key".to_string(),
            base_url: base_url.to_string(),
            model: model.to_string(),
            extra_headers: indexmap::IndexMap::new(),
            alpha_test_key: None,
            allowed_domains: None,
            excluded_domains: None,
        };
        WebSearchClient::new(&config, None).expect("web search client builds for the stub")
    }

    fn uc_params(domains: &[&str]) -> WebFetchParams {
        WebFetchParams {
            url_context: Some(true),
            url_context_domains: Some(domains.iter().map(|s| s.to_string()).collect()),
            proxy_endpoint: Some(UC_DEAD_PROXY.to_string()),
            ..WebFetchParams::default()
        }
    }

    fn uc_client(params: &WebFetchParams, gemini: Option<WebSearchClient>) -> WebFetchClient {
        WebFetchClient::new(params, gemini).expect("web fetch client builds")
    }

    fn uc_grounded_body() -> serde_json::Value {
        serde_json::json!({
            "candidates": [{
                "content": {"role": "model", "parts": [{"text": "Latest stable kernel: 6.12"}]},
                "finishReason": "STOP",
                "groundingMetadata": {"groundingChunks": [
                    {"web": {"uri": "https://93.184.216.34/", "title": "Kernel page", "domain": "93.184.216.34"}}
                ]}
            }]
        })
    }

    fn uc_ungrounded_body() -> serde_json::Value {
        serde_json::json!({
            "candidates": [{
                "content": {"role": "model", "parts": [{"text": "Latest stable kernel: 6.12"}]},
                "finishReason": "STOP"
            }]
        })
    }

    // ── Routing predicate (apex-blvg-A: allowlist-for-direct) ────────────

    fn uc_params_full(domains: &[&str], internal: &[&str]) -> WebFetchParams {
        WebFetchParams {
            url_context: Some(true),
            // The superseded knob is set on purpose: the inverted predicate
            // must ignore it (apex-blvg-A).
            url_context_domains: Some(domains.iter().map(|s| s.to_string()).collect()),
            internal_host_allowlist: Some(internal.iter().map(|s| s.to_string()).collect()),
            proxy_endpoint: Some(UC_DEAD_PROXY.to_string()),
            ..WebFetchParams::default()
        }
    }

    #[test]
    fn uc_predicate_public_host_not_in_allowlist_routes_gemini() {
        // (c) public host, not in the internal_host_allowlist (and nothing
        // in the superseded url_context_domains knob either) → gemini
        // urlContext: public web is the default route under apex-blvg-A.
        let client = uc_client(
            &uc_params_full(&[], &[]),
            Some(uc_gemini_client(UC_DEAD_PROXY)),
        );
        assert!(client.uses_url_context(&Url::parse("https://example.com/").unwrap()));
        assert!(client.uses_url_context(&Url::parse(UC_URL).unwrap()));
    }

    #[test]
    fn uc_predicate_internal_allowlisted_host_routes_direct_not_gemini() {
        // (a) listed in internal_host_allowlist → DIRECT, NOT gemini — even
        // though the host is public and a gemini client is available.
        let client = uc_client(
            &uc_params_full(&["other.example.com"], &["netapp.com"]),
            Some(uc_gemini_client(UC_DEAD_PROXY)),
        );
        assert!(
            !client.uses_url_context(&Url::parse("https://netapp.com/wiki/page").unwrap()),
            "internal-allowlisted host routes direct, NOT gemini"
        );
    }

    #[test]
    fn uc_predicate_private_ip_host_routes_direct() {
        // (b) private-IP safety net: 10.x / 192.168.x never cross the
        // Gemini wire — listed in the superseded old knob or not.
        let client = uc_client(
            &uc_params_full(&["10.0.0.5", "192.168.1.10"], &[]),
            Some(uc_gemini_client(UC_DEAD_PROXY)),
        );
        assert!(!client.uses_url_context(&Url::parse("https://10.0.0.5/").unwrap()));
        assert!(!client.uses_url_context(&Url::parse("https://192.168.1.10/").unwrap()));
    }

    #[test]
    fn uc_predicate_glob_entry_matches_subdomain_direct() {
        // (d) `*.netapp.com` matches every subdomain → direct; the apex and
        // lookalike suffixes are not subdomains → gemini.
        let client = uc_client(
            &uc_params_full(&[], &["*.netapp.com"]),
            Some(uc_gemini_client(UC_DEAD_PROXY)),
        );
        assert!(!client.uses_url_context(&Url::parse("https://sub.netapp.com/x").unwrap()));
        assert!(!client.uses_url_context(&Url::parse("https://a.b.netapp.com/x").unwrap()));
        assert!(client.uses_url_context(&Url::parse("https://netapp.com/x").unwrap()));
        assert!(client.uses_url_context(&Url::parse("https://evilnetapp.com/x").unwrap()));
    }

    #[test]
    fn uc_predicate_loopback_routes_direct() {
        // (e) loopback (localhost / 127.0.0.1) is private → direct, never
        // gemini.
        let client = uc_client(
            &uc_params_full(&["127.0.0.1", "localhost"], &[]),
            Some(uc_gemini_client(UC_DEAD_PROXY)),
        );
        assert!(!client.uses_url_context(&Url::parse("http://localhost:8080/").unwrap()));
        assert!(!client.uses_url_context(&Url::parse("http://127.0.0.1:8080/").unwrap()));
    }

    #[test]
    fn uc_predicate_private_ip_stays_direct_even_when_allowlisted() {
        // A loopback listed in the internal allowlist is still direct: both
        // mechanisms agree on the client path (a private IP never crosses
        // the Gemini wire, under allow_local or not).
        let params = WebFetchParams {
            allow_local: Some(true),
            url_context: Some(true),
            internal_host_allowlist: Some(vec!["127.0.0.1".to_string()]),
            ..WebFetchParams::default()
        };
        let client = uc_client(&params, Some(uc_gemini_client(UC_DEAD_PROXY)));
        assert!(
            !client.uses_url_context(&Url::parse("http://127.0.0.1:8080/").unwrap()),
            "explicit loopback stays on the client path even under allow_local"
        );
    }

    #[test]
    fn uc_predicate_rejects_when_gemini_client_absent() {
        let client = uc_client(&uc_params(&["93.184.216.34"]), None);
        assert!(!client.uses_url_context(&Url::parse(UC_URL).unwrap()));
    }

    #[test]
    fn uc_predicate_rejects_when_flag_off() {
        // url_context is now baked true by default (apex-blvg-C), so the
        // flag-off case must be set explicitly.
        let params = WebFetchParams {
            url_context: Some(false),
            url_context_domains: Some(vec!["93.184.216.34".to_string()]),
            proxy_endpoint: Some(UC_DEAD_PROXY.to_string()),
            ..WebFetchParams::default()
        };
        let client = uc_client(&params, Some(uc_gemini_client(UC_DEAD_PROXY)));
        assert!(!client.uses_url_context(&Url::parse(UC_URL).unwrap()));
    }

    #[test]
    fn uc_predicate_internal_allowlist_normalizes_case_trailing_dot_and_www() {
        // Pin the DomainMatcher normalization on the INTERNAL matcher
        // (apex-blvg-A): trim, strip a trailing dot, strip a *lowercase*
        // "www." prefix, then lowercase.
        let client = uc_client(
            &uc_params_full(&[], &["NETAPP.COM.", "www.internal.netapp.com"]),
            Some(uc_gemini_client(UC_DEAD_PROXY)),
        );
        // Mixed case + trailing dot, no www: "NETAPP.COM." -> "netapp.com".
        assert!(!client.uses_url_context(&Url::parse("https://netapp.com/x").unwrap()));
        // Lowercase www is stripped: "www.internal.netapp.com" ->
        // "internal.netapp.com".
        assert!(!client.uses_url_context(&Url::parse("https://internal.netapp.com/y").unwrap()));
        // An unlisted public host still routes gemini.
        assert!(client.uses_url_context(&Url::parse("https://other.netapp.com/z").unwrap()));
    }

    // ── Baked zero-fuss defaults + is_gemini_wire guard (apex-blvg-C) ────

    #[test]
    fn uc_predicate_non_gemini_model_web_search_routes_direct_at_baked_default() {
        // (apex-blvg-C) stock-safety pin: a web_search backend whose model is
        // NOT a gemini slug (e.g. a stock gpt-pinned web_search) is not
        // Gemini-native — the is_gemini_wire guard keeps a public
        // (non-private, non-allowlisted) host on the DIRECT path even with
        // url_context at the baked default.
        let params = WebFetchParams {
            proxy_endpoint: Some(UC_DEAD_PROXY.to_string()),
            ..WebFetchParams::default()
        };
        let client = uc_client(
            &params,
            Some(uc_gemini_client_model(UC_DEAD_PROXY, "gpt-5.6-terra")),
        );
        assert!(
            !client.uses_url_context(&Url::parse("https://example.com/").unwrap()),
            "non-gemini web_search model keeps the direct path at the baked url_context default"
        );
    }

    #[test]
    fn uc_predicate_gemini_model_web_search_routes_gemini_at_baked_default() {
        // (apex-blvg-C) the baked url_context=true default engages the
        // urlContext routing for a Gemini-native web_search backend on a
        // public host — the zero-touch Linux fleet behavior with no config.
        let params = WebFetchParams {
            proxy_endpoint: Some(UC_DEAD_PROXY.to_string()),
            ..WebFetchParams::default()
        };
        let client = uc_client(
            &params,
            Some(uc_gemini_client_model(UC_DEAD_PROXY, "gemini-3.8-flash")),
        );
        assert!(
            client.uses_url_context(&Url::parse("https://example.com/").unwrap()),
            "gemini web_search model + baked default routes a public host to the Gemini wire"
        );
    }

    #[test]
    fn uc_predicate_baked_netapp_allowlist_routes_direct() {
        // (apex-blvg-C) the baked internal_host_allowlist=["*.netapp.com"]
        // default is a live safety net: NetApp internal subdomains route
        // DIRECT even with a gemini web_search backend and url_context=true.
        let params = WebFetchParams {
            url_context: Some(true),
            proxy_endpoint: Some(UC_DEAD_PROXY.to_string()),
            ..WebFetchParams::default()
        };
        let client = uc_client(&params, Some(uc_gemini_client(UC_DEAD_PROXY)));
        assert!(
            !client.uses_url_context(&Url::parse("https://sub.netapp.com/wiki").unwrap()),
            "baked *.netapp.com allowlist keeps internal subdomains on the direct path"
        );
    }

    // ── fetch_url_context (single attempt; None on any failure) ───────────

    #[tokio::test]
    async fn uc_fetch_url_context_returns_content_on_grounded_result() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path(UC_PATH))
            .respond_with(ResponseTemplate::new(200).set_body_json(uc_grounded_body()))
            .expect(1)
            .mount(&server)
            .await;
        let client = uc_client(
            &uc_params(&["93.184.216.34"]),
            Some(uc_gemini_client(&server.uri())),
        );
        let out = client
            .fetch_url_context(&Url::parse(UC_URL).unwrap(), UC_URL)
            .await
            .expect("grounded result must shape to Some");
        match out {
            WebFetchOutput::Content(content) => {
                assert_eq!(content.url, UC_URL);
                assert!(content.content.contains("<untrusted_context>"));
                assert!(content.content.contains("Latest stable kernel: 6.12"));
            }
            other => panic!("expected Content, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn uc_fetch_url_context_none_on_not_grounded() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path(UC_PATH))
            .respond_with(ResponseTemplate::new(200).set_body_json(uc_ungrounded_body()))
            .expect(1)
            .mount(&server)
            .await;
        let client = uc_client(
            &uc_params(&["93.184.216.34"]),
            Some(uc_gemini_client(&server.uri())),
        );
        assert!(
            client
                .fetch_url_context(&Url::parse(UC_URL).unwrap(), UC_URL)
                .await
                .is_none()
        );
    }

    #[tokio::test]
    async fn uc_fetch_url_context_none_on_empty_text_with_chunks() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let body = serde_json::json!({
            "candidates": [{
                "content": {"role": "model", "parts": [{}]},
                "finishReason": "STOP",
                "groundingMetadata": {"groundingChunks": [
                    {"web": {"uri": "https://93.184.216.34/", "title": "Kernel page"}}
                ]}
            }]
        });
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path(UC_PATH))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .expect(1)
            .mount(&server)
            .await;
        let client = uc_client(
            &uc_params(&["93.184.216.34"]),
            Some(uc_gemini_client(&server.uri())),
        );
        assert!(
            client
                .fetch_url_context(&Url::parse(UC_URL).unwrap(), UC_URL)
                .await
                .is_none()
        );
    }

    #[tokio::test]
    async fn uc_fetch_url_context_none_on_http_404() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path(UC_PATH))
            .respond_with(ResponseTemplate::new(404).set_body_string("not found"))
            .expect(1)
            .mount(&server)
            .await;
        let client = uc_client(
            &uc_params(&["93.184.216.34"]),
            Some(uc_gemini_client(&server.uri())),
        );
        assert!(
            client
                .fetch_url_context(&Url::parse(UC_URL).unwrap(), UC_URL)
                .await
                .is_none()
        );
    }

    #[tokio::test]
    async fn uc_fetch_url_context_none_on_http_401() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path(UC_PATH))
            .respond_with(ResponseTemplate::new(401).set_body_string("unauthorized"))
            .expect(1)
            .mount(&server)
            .await;
        let client = uc_client(
            &uc_params(&["93.184.216.34"]),
            Some(uc_gemini_client(&server.uri())),
        );
        assert!(
            client
                .fetch_url_context(&Url::parse(UC_URL).unwrap(), UC_URL)
                .await
                .is_none()
        );
    }

    #[tokio::test]
    async fn uc_fetch_url_context_none_on_malformed_200() {
        // P4 trap shape: 200 + a responses-wire object, never a success.
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path(UC_PATH))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"status": "incomplete", "tools": []})),
            )
            .expect(1)
            .mount(&server)
            .await;
        let client = uc_client(
            &uc_params(&["93.184.216.34"]),
            Some(uc_gemini_client(&server.uri())),
        );
        assert!(
            client
                .fetch_url_context(&Url::parse(UC_URL).unwrap(), UC_URL)
                .await
                .is_none()
        );
    }

    #[tokio::test]
    async fn uc_fetch_url_context_none_on_transport_error() {
        // gemini base_url points at a closed loopback port: connection refused.
        let client = uc_client(
            &uc_params(&["93.184.216.34"]),
            Some(uc_gemini_client(UC_DEAD_PROXY)),
        );
        assert!(
            client
                .fetch_url_context(&Url::parse(UC_URL).unwrap(), UC_URL)
                .await
                .is_none()
        );
    }

    #[tokio::test]
    async fn uc_fetch_url_context_none_when_gemini_absent() {
        let client = uc_client(&uc_params(&["93.184.216.34"]), None);
        assert!(
            client
                .fetch_url_context(&Url::parse(UC_URL).unwrap(), UC_URL)
                .await
                .is_none()
        );
    }

    // ── fetch()-level routing: no cache insert, client fallback ───────────

    #[tokio::test]
    async fn uc_fetch_success_is_routed_twice_and_never_cached() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path(UC_PATH))
            .respond_with(ResponseTemplate::new(200).set_body_json(uc_grounded_body()))
            .expect(2) // both fetches hit the LLM branch: nothing was cached
            .mount(&server)
            .await;
        let client = uc_client(
            &uc_params(&["93.184.216.34"]),
            Some(uc_gemini_client(&server.uri())),
        );
        for _ in 0..2 {
            let out = client
                .fetch(UC_URL, None, None, None)
                .await
                .expect("urlContext success must return Ok");
            match out {
                WebFetchOutput::Content(content) => {
                    assert!(content.content.contains("Latest stable kernel: 6.12"));
                }
                other => panic!("expected Content, got {other:?}"),
            }
        }
        assert!(
            client.cache.read().get(UC_URL).is_none(),
            "a urlContext success must NOT insert into the direct-fetch cache (cross-engine cache poisoning)"
        );
    }

    #[tokio::test]
    async fn uc_fetch_falls_back_to_client_path_on_not_grounded() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path(UC_PATH))
            .respond_with(ResponseTemplate::new(200).set_body_json(uc_ungrounded_body()))
            .expect(1) // R8: a single LLM attempt, no second hop
            .mount(&server)
            .await;
        let client = uc_client(
            &uc_params(&["93.184.216.34"]),
            Some(uc_gemini_client(&server.uri())),
        );
        let err = client
            .fetch(UC_URL, None, None, None)
            .await
            .expect_err("client path (dead proxy) must fail");
        assert!(matches!(err, WebFetchError::HttpRequest(_)));
    }

    #[tokio::test]
    async fn uc_fetch_falls_back_to_client_path_on_gemini_http_500() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path(UC_PATH))
            .respond_with(ResponseTemplate::new(500).set_body_string("boom"))
            .expect(1)
            .mount(&server)
            .await;
        let client = uc_client(
            &uc_params(&["93.184.216.34"]),
            Some(uc_gemini_client(&server.uri())),
        );
        let err = client
            .fetch(UC_URL, None, None, None)
            .await
            .expect_err("client path (dead proxy) must fail");
        assert!(matches!(err, WebFetchError::HttpRequest(_)));
    }

    #[tokio::test]
    async fn uc_fetch_falls_back_to_client_path_on_gemini_timeout() {
        use std::time::Duration;
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path(UC_PATH))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_delay(Duration::from_secs(5))
                    .set_body_json(uc_grounded_body()),
            )
            .expect(1)
            .mount(&server)
            .await;
        let mut gemini = uc_gemini_client(&server.uri());
        gemini.set_gemini_attempt_timeout_for_test(Duration::from_millis(200));
        let client = uc_client(&uc_params(&["93.184.216.34"]), Some(gemini));
        let err = client
            .fetch(UC_URL, None, None, None)
            .await
            .expect_err("client path (dead proxy) must fail");
        assert!(matches!(err, WebFetchError::HttpRequest(_)));
    }

    #[tokio::test]
    async fn uc_fetch_default_off_keeps_client_path_byte_identical() {
        // url_context=false (explicit opt-out; the default is now baked
        // true, apex-blvg-C) with a gemini client present: the LLM wire is
        // never called and the client path runs (dead proxy fails it
        // deterministically).
        let server = wiremock::MockServer::start().await;
        let params = WebFetchParams {
            url_context: Some(false),
            proxy_endpoint: Some(UC_DEAD_PROXY.to_string()),
            ..WebFetchParams::default()
        };
        let client = uc_client(&params, Some(uc_gemini_client(&server.uri())));
        let err = client
            .fetch(UC_URL, None, None, None)
            .await
            .expect_err("client path (dead proxy) must fail");
        assert!(matches!(err, WebFetchError::HttpRequest(_)));
        let reqs = server.received_requests().await.unwrap();
        assert!(
            reqs.is_empty(),
            "default-off must never call the Gemini wire"
        );
    }
}
