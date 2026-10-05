//! Executed only as --media-process in the offline sandbox. Input paths,
//! executables and flags are fixed; document bytes never become instructions.
use super::*;
use serde::Serialize;
use std::{
    fs,
    io::Read,
    process::{Command, Stdio},
};

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum ProcessorRequest {
    Scan {
        media_type: String,
        source_sha256: String,
    },
    Thumbnail {
        width: u32,
        height: u32,
        format: String,
        source_sha256: String,
    },
    Extract {
        media_type: String,
        max_characters: usize,
        source_sha256: String,
    },
}
impl ProcessorRequest {
    pub fn source_sha256(&self) -> &str {
        match self {
            Self::Scan { source_sha256, .. }
            | Self::Thumbnail { source_sha256, .. }
            | Self::Extract { source_sha256, .. } => source_sha256,
        }
    }
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum ProcessorOutput {
    Scan {
        format_validated: bool,
        policy: String,
    },
    Thumbnail {
        content_base64: String,
    },
    Extract {
        text: String,
        pages: Vec<Page>,
    },
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Page {
    pub number: usize,
    pub method: String,
    pub text_offset: usize,
    pub characters: usize,
}

fn tool(exe: &str, args: &[String], limit: usize) -> AppResult<Vec<u8>> {
    let mut child = Command::new(exe)
        .args(args)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("LANG", "C.UTF-8")
        .env("HOME", "/work")
        .env("OMP_THREAD_LIMIT", "1")
        .current_dir("/work")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| AppError::Unavailable)?;
    let mut bytes = Vec::new();
    let read = child
        .stdout
        .take()
        .ok_or(AppError::Internal)?
        .take((limit + 1) as u64)
        .read_to_end(&mut bytes);
    if read.is_err() || bytes.len() > limit {
        let _ = child.kill();
        let _ = child.wait();
        return Err(AppError::Quota);
    }
    if !child.wait().map_err(|_| AppError::Unavailable)?.success() {
        return Err(AppError::invalid("document_parser_refused"));
    }
    Ok(bytes)
}
fn pdf_pages() -> AppResult<usize> {
    let info = String::from_utf8(tool("/usr/bin/pdfinfo", &["/input/content".into()], 16384)?)
        .map_err(|_| AppError::invalid("document_parser_refused"))?;
    let pages = info
        .lines()
        .find_map(|l| l.strip_prefix("Pages:"))
        .and_then(|s| s.trim().parse::<usize>().ok())
        .ok_or(AppError::invalid("document_page_count_unknown"))?;
    if !(1..=20).contains(&pages) {
        return Err(AppError::invalid("document_page_limit"));
    }
    Ok(pages)
}
fn text(bytes: Vec<u8>) -> AppResult<String> {
    let text = String::from_utf8(bytes).map_err(|_| AppError::invalid("document_text_encoding"))?;
    if text.contains('\0') {
        return Err(AppError::invalid("document_text_encoding"));
    }
    Ok(text.replace('\u{c}', "\n"))
}
fn ocr(path: &str) -> AppResult<String> {
    text(tool(
        "/usr/bin/tesseract",
        &[
            path.into(),
            "stdout".into(),
            "-l".into(),
            "eng+fra".into(),
            "--psm".into(),
            "6".into(),
        ],
        MAX_EXTRACTED_BYTES,
    )?)
}
pub fn execute(request: &ProcessorRequest, bytes: &[u8]) -> AppResult<ProcessorOutput> {
    if bytes.is_empty()
        || bytes.len() > MAX_UPLOAD_BYTES
        || !valid_sha256_hex(request.source_sha256())
        || digest_hex(bytes) != request.source_sha256()
    {
        return Err(AppError::invalid("media_source_mismatch"));
    }
    match request {
        ProcessorRequest::Scan { media_type, .. } => {
            validate_upload(UploadInput {
                filename: "source".into(),
                media_type: media_type.clone(),
                content_base64: base64::engine::general_purpose::STANDARD.encode(bytes),
            })?;
            match media_type.as_str() {
                "image/png" | "image/jpeg" => {
                    transform_thumbnail(bytes, 1, 1, "png")?;
                }
                "application/pdf" => {
                    // Reject known active-document structures as an additional
                    // restriction. This is format validation, not an AV claim.
                    for marker in [
                        b"/JavaScript".as_slice(),
                        b"/JS",
                        b"/Launch",
                        b"/OpenAction",
                        b"/EmbeddedFile",
                        b"/XFA",
                    ] {
                        if bytes.windows(marker.len()).any(|w| w == marker) {
                            return Err(AppError::invalid("active_pdf_content_denied"));
                        }
                    }
                    pdf_pages()?;
                }
                _ => {}
            }
            Ok(ProcessorOutput::Scan {
                format_validated: true,
                policy: "bounded_format_validation_v1".into(),
            })
        }
        ProcessorRequest::Thumbnail {
            width,
            height,
            format,
            ..
        } => {
            let output = transform_thumbnail(bytes, *width, *height, format)?;
            Ok(ProcessorOutput::Thumbnail {
                content_base64: base64::engine::general_purpose::STANDARD.encode(output),
            })
        }
        ProcessorRequest::Extract {
            media_type,
            max_characters,
            ..
        } => {
            if *max_characters == 0 || *max_characters > MAX_EXTRACTED_BYTES {
                return Err(AppError::Quota);
            }
            let mut result = String::new();
            let mut pages = Vec::new();
            let texts = match media_type.as_str() {
                "application/pdf" => {
                    let count = pdf_pages()?;
                    let mut values = Vec::new();
                    for number in 1..=count {
                        let mut page = text(tool(
                            "/usr/bin/pdftotext",
                            &[
                                "-f".into(),
                                number.to_string(),
                                "-l".into(),
                                number.to_string(),
                                "-enc".into(),
                                "UTF-8".into(),
                                "-nopgbrk".into(),
                                "/input/content".into(),
                                "-".into(),
                            ],
                            MAX_EXTRACTED_BYTES,
                        )?)?;
                        let method = if page.trim().is_empty() {
                            tool(
                                "/usr/bin/pdftoppm",
                                &[
                                    "-f".into(),
                                    number.to_string(),
                                    "-l".into(),
                                    number.to_string(),
                                    "-singlefile".into(),
                                    "-scale-to".into(),
                                    "1600".into(),
                                    "-png".into(),
                                    "/input/content".into(),
                                    "/work/page".into(),
                                ],
                                16384,
                            )?;
                            let metadata = fs::metadata("/work/page.png")
                                .map_err(|_| AppError::Unavailable)?;
                            if metadata.len() > MAX_OUTPUT_BYTES as u64 {
                                return Err(AppError::Quota);
                            }
                            page = ocr("/work/page.png")?;
                            fs::remove_file("/work/page.png").map_err(|_| AppError::Unavailable)?;
                            "tesseract"
                        } else {
                            "poppler_text"
                        };
                        values.push((number, method, page));
                        if values.iter().map(|(_, _, s)| s.len()).sum::<usize>()
                            > MAX_EXTRACTED_BYTES
                        {
                            return Err(AppError::Quota);
                        }
                    }
                    values
                }
                "image/png" | "image/jpeg" => {
                    transform_thumbnail(bytes, 1, 1, "png")?;
                    vec![(1, "tesseract", ocr("/input/content")?)]
                }
                "text/plain" | "text/csv" | "application/json" => {
                    vec![(1, "utf8", text(bytes.to_vec())?)]
                }
                _ => return Err(AppError::invalid("unsupported_extraction_type")),
            };
            let mut characters = 0;
            for (number, method, value) in texts {
                let count = value.chars().count();
                if characters + count > *max_characters
                    || result.len() + value.len() > MAX_EXTRACTED_BYTES
                {
                    return Err(AppError::Quota);
                }
                pages.push(Page {
                    number,
                    method: method.into(),
                    text_offset: characters,
                    characters: count,
                });
                characters += count;
                result.push_str(&value);
            }
            Ok(ProcessorOutput::Extract {
                text: result,
                pages,
            })
        }
    }
}
pub fn run_fixed() -> AppResult<()> {
    let params = fs::read("/input/request.json").map_err(|_| AppError::Unavailable)?;
    if params.len() > 8192 {
        return Err(AppError::Quota);
    }
    let request: ProcessorRequest =
        serde_json::from_slice(&params).map_err(|_| AppError::invalid("media_request_invalid"))?;
    let metadata = fs::symlink_metadata("/input/content").map_err(|_| AppError::Unavailable)?;
    if !metadata.is_file() || metadata.len() > MAX_UPLOAD_BYTES as u64 {
        return Err(AppError::Quota);
    }
    let bytes = fs::read("/input/content").map_err(|_| AppError::Unavailable)?;
    let output = execute(&request, &bytes)?;
    let encoded = serde_json::to_vec(&output).map_err(|_| AppError::Internal)?;
    if encoded.len() > 12 * 1024 * 1024 {
        return Err(AppError::Quota);
    }
    use std::io::Write;
    fs::File::create("/output/result.json")
        .and_then(|mut f| f.write_all(&encoded))
        .map_err(|_| AppError::Unavailable)?;
    Ok(())
}
