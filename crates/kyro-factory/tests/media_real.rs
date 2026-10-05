#![cfg(feature = "test-support")]
#[path = "../../kyro-app/tests/support/mod.rs"]
mod app_support;
use app_support::Fixture;
use base64::{Engine, engine::general_purpose::STANDARD};
use image::{DynamicImage, ImageBuffer, ImageFormat, Rgb};
use kyro_app::{
    Actor, AppError, OperationRequest,
    documents::{
        processing::{MediaReceipt, claim_media, complete_media, fail_media, prepare_media},
        processor::{ProcessorOutput, ProcessorRequest},
    },
};
use kyro_factory::{
    digest_bytes,
    media::{MediaConfig, MediaSandbox},
};
use serde_json::{Value, json};
use sqlx::Row;
use std::{io::Cursor, time::Duration};
use uuid::Uuid;

fn sandbox() -> MediaSandbox {
    let config: MediaConfig = serde_json::from_str(
        &std::env::var("KYRO_P2_MEDIA_CONFIG").expect("pinned media tools and processor required"),
    )
    .unwrap();
    MediaSandbox::new(config).unwrap()
}
async fn fixture() -> (Fixture, Actor) {
    let mut f = Fixture::new().await;
    let (actor, token) = Fixture::session(
        &f.admin,
        &f.core,
        f.actor.tenant_id(),
        f.actor.application_id(),
        Uuid::new_v4(),
        &["documents.admin"],
    )
    .await;
    f.actor = actor;
    f.token = token;
    let (worker, _) = Fixture::session(
        &f.admin,
        &f.core,
        f.actor.tenant_id(),
        f.actor.application_id(),
        Uuid::new_v4(),
        &["documents.processor"],
    )
    .await;
    (f, worker)
}
async fn upload(f: &Fixture, bytes: &[u8], media_type: &str) -> Uuid {
    let value = if bytes.len() < 32768 {
        f.op("B081", "upload", json!({"filename":"synthetic", "media_type":media_type, "content_base64":STANDARD.encode(bytes)}), None).await.unwrap()
    } else {
        let begun = f.op("B081", "upload.begin", json!({"filename":"synthetic", "media_type":media_type,"size_bytes":bytes.len(),"sha256":digest_bytes(bytes)}), None).await.unwrap();
        let id = begun["upload_id"].as_str().unwrap();
        for (index, chunk) in bytes.chunks(32768).enumerate() {
            f.op("B081", "upload.chunk", json!({"upload_id":id,"offset":index*32768,"content_base64":STANDARD.encode(chunk)}), None).await.unwrap();
        }
        f.op("B081", "upload.finish", json!({"upload_id":id}), None)
            .await
            .unwrap()
    };
    Uuid::parse_str(value["document_id"].as_str().unwrap()).unwrap()
}
async fn process_next(f: &Fixture, worker: &Actor, sandbox: &MediaSandbox) -> Uuid {
    let claim = claim_media(&f.core, worker.clone()).await.unwrap().unwrap();
    let input = prepare_media(&f.core, worker.clone(), claim).await.unwrap();
    let (output, receipt) = sandbox.process(&input).await.unwrap_or_else(|error| {
        if let Some(execution) = &error.execution {
            eprintln!(
                "public fixture processor diagnostic: {}",
                String::from_utf8_lossy(&execution.stderr)
            );
        }
        panic!("{error}");
    });
    complete_media(&f.core, worker.clone(), &input, output, receipt)
        .await
        .unwrap();
    input.claim().id
}
fn image() -> Vec<u8> {
    let pixels = ImageBuffer::from_fn(1024, 768, |x, y| {
        Rgb([(x % 251) as u8, (y % 239) as u8, ((x + y) % 233) as u8])
    });
    let mut bytes = Cursor::new(Vec::new());
    DynamicImage::ImageRgb8(pixels)
        .write_to(&mut bytes, ImageFormat::Png)
        .unwrap();
    bytes.into_inner()
}
fn copy_output(output: &ProcessorOutput) -> ProcessorOutput {
    serde_json::from_value(json!(output)).unwrap()
}
fn copy_receipt(receipt: &MediaReceipt) -> MediaReceipt {
    serde_json::from_value(json!(receipt)).unwrap()
}

#[tokio::test]
#[ignore = "requires real gVisor media tools, constrained PostgreSQL and Docker controller"]
async fn actual_thumbnail_and_source_fences() {
    let (f, worker) = fixture().await;
    let sb = sandbox();
    let original = image();
    let doc = upload(&f, &original, "image/png").await;
    process_next(&f, &worker, &sb).await;
    f.op(
        "B082",
        "update",
        json!({"document_id":doc,"expected_version":2,"title":"metadata is independent"}),
        None,
    )
    .await
    .unwrap();
    f.op(
        "B084",
        "thumbnail",
        json!({"document_id":doc,"width":256,"height":192,"format":"png"}),
        None,
    )
    .await
    .unwrap();
    let claim = claim_media(&f.core, worker.clone()).await.unwrap().unwrap();
    let input = prepare_media(&f.core, worker.clone(), claim).await.unwrap();
    assert!(matches!(
        input.request(),
        ProcessorRequest::Thumbnail {
            width: 256,
            height: 192,
            ..
        }
    ));
    let (output, receipt) = sb.process(&input).await.unwrap();
    let mut bad = copy_receipt(&receipt);
    bad.output_digest = "0".repeat(64);
    assert!(
        complete_media(&f.core, worker.clone(), &input, copy_output(&output), bad)
            .await
            .is_err()
    );
    let replay_output = copy_output(&output);
    let replay_receipt = copy_receipt(&receipt);
    complete_media(&f.core, worker.clone(), &input, output, receipt)
        .await
        .unwrap();
    assert!(
        complete_media(
            &f.core,
            worker.clone(),
            &input,
            replay_output,
            replay_receipt
        )
        .await
        .is_err()
    );
    let mut bytes = Vec::new();
    let result = loop {
        let result = f.op("B084","thumbnail.get",json!({"document_id":doc,"width":256,"height":192,"format":"png","offset":bytes.len()}),None).await.unwrap();
        bytes.extend(
            STANDARD
                .decode(result["content_base64"].as_str().unwrap())
                .unwrap(),
        );
        assert_eq!(result["next_offset"], bytes.len());
        if result["complete"] == true {
            break result;
        }
    };
    assert_eq!(image::load_from_memory(&bytes).unwrap().width(), 256);
    assert_eq!(image::load_from_memory(&bytes).unwrap().height(), 192);
    assert_eq!(digest_bytes(&bytes), result["sha256"].as_str().unwrap());
    assert_eq!(result["source_version"], 1);
    let source: Vec<u8> = sqlx::query_scalar(
        "SELECT content FROM app_document_versions WHERE document_id=$1 AND version=1",
    )
    .bind(doc)
    .fetch_one(&f.admin)
    .await
    .unwrap();
    assert_eq!(source, original);
    let (stranger, _) = Fixture::session(
        &f.admin,
        &f.core,
        f.actor.tenant_id(),
        f.actor.application_id(),
        Uuid::new_v4(),
        &["viewer"],
    )
    .await;
    assert!(
        f.dispatcher
            .dispatch(
                &f.core,
                stranger,
                OperationRequest {
                    component_id: "B084".into(),
                    action: "thumbnail.get".into(),
                    payload: json!({"document_id":doc,"width":256,"height":192,"format":"png"}),
                    idempotency_key: Uuid::new_v4().to_string(),
                    expected_version: None
                }
            )
            .await
            .is_err()
    );
    f.op(
        "B084",
        "thumbnail",
        json!({"document_id":doc,"width":128,"height":96,"format":"jpeg"}),
        None,
    )
    .await
    .unwrap();
    let claim = claim_media(&f.core, worker.clone()).await.unwrap().unwrap();
    let input = prepare_media(&f.core, worker.clone(), claim).await.unwrap();
    let (output, receipt) = sb.process(&input).await.unwrap();
    sqlx::query("UPDATE app_sessions SET revoked_at=clock_timestamp() WHERE id=$1")
        .bind(f.actor.session_id())
        .execute(&f.admin)
        .await
        .unwrap();
    assert!(
        complete_media(&f.core, worker.clone(), &input, output, receipt)
            .await
            .is_err()
    );
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM app_document_derivatives WHERE document_id=$1")
            .bind(doc)
            .fetch_one(&f.admin)
            .await
            .unwrap();
    assert_eq!(count, 1);
    fail_media(&f.core, worker.clone(), input.claim(), &AppError::Forbidden)
        .await
        .unwrap();
    assert!(claim_media(&f.core, f.actor.clone()).await.is_err());
}

#[tokio::test]
#[ignore = "requires real gVisor Poppler/Tesseract and constrained PostgreSQL"]
async fn actual_approved_pdf_text_and_two_page_ocr() {
    let (f, worker) = fixture().await;
    let sb = sandbox();
    let source = format!(
        "Bonjour {{name}} — facture €\n{}",
        (1..=48).map(|i| format!("Line {i}\n")).collect::<String>()
    )
    .replace("{name}", "{{name}}");
    let saved = f
        .op("B085", "save", json!({"source":source}), None)
        .await
        .unwrap();
    let template = saved["template_id"].as_str().unwrap();
    let request = json!({"template_id":template,"format":"pdf","values":{"name":"Kyro <literal>"}});
    assert!(f.op("B085", "render", request.clone(), None).await.is_err());
    f.op(
        "B085",
        "approve",
        json!({"template_id":template,"version":1}),
        None,
    )
    .await
    .unwrap();
    assert!(
        f.op(
            "B085",
            "render",
            json!({"template_id":template,"format":"pdf","values":{"unknown":"x"}}),
            None
        )
        .await
        .is_err()
    );
    let rendered = f.op("B085", "render", request, None).await.unwrap();
    let bytes = STANDARD
        .decode(rendered["content_base64"].as_str().unwrap())
        .unwrap();
    assert_eq!(digest_bytes(&bytes), rendered["sha256"].as_str().unwrap());
    let doc = upload(&f, &bytes, "application/pdf").await;
    process_next(&f, &worker, &sb).await;
    let enqueued = f
        .op(
            "B086",
            "extract",
            json!({"document_id":doc,"max_characters":100000}),
            None,
        )
        .await
        .unwrap();
    process_next(&f, &worker, &sb).await;
    let extracted = f
        .op("B086", "read", json!({"job_id":enqueued["job_id"]}), None)
        .await
        .unwrap();
    let text = extracted["text"].as_str().unwrap();
    assert!(text.contains("Bonjour Kyro <literal>"));
    assert!(text.contains('€'));
    assert!(text.contains("Line 48"));
    assert_eq!(
        extracted["provenance"]["pages"].as_array().unwrap().len(),
        2
    );
    assert_eq!(
        extracted["provenance"]["pages"][0]["method"],
        "poppler_text"
    );
    assert_eq!(extracted["automatic_actions"], false);
    let images = image_only_pdf();
    let doc = upload(&f, &images, "application/pdf").await;
    process_next(&f, &worker, &sb).await;
    let enqueued = f
        .op(
            "B086",
            "extract",
            json!({"document_id":doc,"max_characters":10000}),
            None,
        )
        .await
        .unwrap();
    process_next(&f, &worker, &sb).await;
    let extracted = f
        .op("B086", "read", json!({"job_id":enqueued["job_id"]}), None)
        .await
        .unwrap();
    let text = extracted["text"].as_str().unwrap();
    assert!(text.contains("PAGE ONE"), "synthetic OCR text: {text}");
    assert!(text.contains("PAGE TWO"), "synthetic OCR text: {text}");
    let pages = extracted["provenance"]["pages"].as_array().unwrap();
    assert_eq!(pages.len(), 2);
    assert!(pages.iter().all(|p| p["method"] == "tesseract"));
    let stored = sqlx::query("SELECT receipt,state FROM app_document_outbox WHERE id=$1")
        .bind(Uuid::parse_str(enqueued["job_id"].as_str().unwrap()).unwrap())
        .fetch_one(&f.admin)
        .await
        .unwrap();
    assert_eq!(stored.try_get::<String, _>("state").unwrap(), "succeeded");
    assert_eq!(
        stored.try_get::<Value, _>("receipt").unwrap()["schema_version"],
        1
    );
}

#[tokio::test]
#[ignore = "requires actual gVisor process timeout and disposable PostgreSQL"]
async fn timeout_parser_refusal_stale_lease_keep_originals() {
    let (f, worker) = fixture().await;
    let sb = sandbox();
    let bytes = b"bounded synthetic text";
    let doc = upload(&f, bytes, "text/plain").await;
    let claim = claim_media(&f.core, worker.clone()).await.unwrap().unwrap();
    let input = prepare_media(&f.core, worker.clone(), claim).await.unwrap();
    let error = sb
        .process_with_deadline(input.request(), input.bytes(), Duration::from_millis(1))
        .await
        .err()
        .expect("the actual processor cannot finish within 1 ms");
    assert_eq!(error.code, "sandbox_deadline_exceeded");
    fail_media(
        &f.core,
        worker.clone(),
        input.claim(),
        &AppError::Unavailable,
    )
    .await
    .unwrap();
    let row=sqlx::query("SELECT state,content FROM app_documents d JOIN app_document_versions v ON v.document_id=d.id WHERE d.id=$1").bind(doc).fetch_one(&f.admin).await.unwrap();
    assert_eq!(row.try_get::<String, _>("state").unwrap(), "quarantined");
    assert_eq!(row.try_get::<Vec<u8>, _>("content").unwrap(), bytes);
    f.op("B081", "scan.retry", json!({"document_id":doc}), None)
        .await
        .unwrap();
    let claim = claim_media(&f.core, worker.clone()).await.unwrap().unwrap();
    let input = prepare_media(&f.core, worker.clone(), claim).await.unwrap();
    sqlx::query("UPDATE app_document_outbox SET lease_until=clock_timestamp()-interval '1 second' WHERE id=$1").bind(input.claim().id).execute(&f.admin).await.unwrap();
    assert!(
        prepare_media(&f.core, worker.clone(), input.claim().clone())
            .await
            .is_err()
    );
    assert!(
        fail_media(
            &f.core,
            worker.clone(),
            input.claim(),
            &AppError::Unavailable
        )
        .await
        .is_err()
    );
    let next = claim_media(&f.core, worker.clone()).await.unwrap().unwrap();
    assert_eq!(next.id, input.claim().id);
    assert!(next.generation > input.claim().generation);
    let input = prepare_media(&f.core, worker.clone(), next).await.unwrap();
    let (output, receipt) = sb.process(&input).await.unwrap();
    complete_media(&f.core, worker.clone(), &input, output, receipt)
        .await
        .unwrap();
    let pdf = b"%PDF-1.4\n/JS (active)\n%%EOF";
    let request = ProcessorRequest::Scan {
        media_type: "application/pdf".into(),
        source_sha256: digest_bytes(pdf),
    };
    assert_eq!(
        sb.process_bytes(&request, pdf).await.err().unwrap().code,
        "media_processing_refused"
    );
    let request = ProcessorRequest::Scan {
        media_type: "text/plain".into(),
        source_sha256: "0".repeat(64),
    };
    assert_eq!(
        sb.process_bytes(&request, bytes).await.err().unwrap().code,
        "media_input_refused"
    );
}

// An image-only PDF: no font, text stream or OCR side channel. Pixel glyphs are
// public synthetic fixtures; the production parser must rasterize and recognize them.
fn image_only_pdf() -> Vec<u8> {
    let mut objects = vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Count 2 /Kids [3 0 R 6 0 R] >>".to_vec(),
    ];
    for (index, phrase) in ["KYRO OCR PAGE ONE", "KYRO OCR PAGE TWO"]
        .iter()
        .enumerate()
    {
        let first = 3 + index * 3;
        objects.push(format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 595 842] /Resources << /XObject << /I {} 0 R >> >> /Contents {} 0 R >>",first+1,first+2).into_bytes());
        let mut pixels = vec![255u8; 1000 * 120];
        for (letter, c) in phrase.chars().enumerate() {
            for (y, row) in glyph(c).iter().enumerate() {
                for x in 0..5 {
                    if row & (1 << (4 - x)) != 0 {
                        for dy in 0..6 {
                            for dx in 0..6 {
                                pixels[(35 + y * 6 + dy) * 1000 + 30 + letter * 42 + x * 6 + dx] =
                                    0;
                            }
                        }
                    }
                }
            }
        }
        let mut image=format!("<< /Type /XObject /Subtype /Image /Width 1000 /Height 120 /ColorSpace /DeviceGray /BitsPerComponent 8 /Length {} >>\nstream\n",pixels.len()).into_bytes();
        image.extend(pixels);
        image.extend(b"\nendstream");
        objects.push(image);
        let stream = b"q 535 0 0 64.2 30 700 cm /I Do Q\n";
        objects.push(
            format!(
                "<< /Length {} >>\nstream\n{}endstream",
                stream.len(),
                std::str::from_utf8(stream).unwrap()
            )
            .into_bytes(),
        );
    }
    let mut bytes = b"%PDF-1.4\n".to_vec();
    let mut offsets = vec![0];
    for (i, object) in objects.iter().enumerate() {
        offsets.push(bytes.len());
        bytes.extend(format!("{} 0 obj\n", i + 1).as_bytes());
        bytes.extend(object);
        bytes.extend(b"\nendobj\n");
    }
    let xref = bytes.len();
    bytes.extend(format!("xref\n0 {}\n0000000000 65535 f \n", offsets.len()).as_bytes());
    for offset in offsets.iter().skip(1) {
        bytes.extend(format!("{offset:010} 00000 n \n").as_bytes());
    }
    bytes.extend(
        format!(
            "trailer << /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            offsets.len()
        )
        .as_bytes(),
    );
    bytes
}
fn glyph(c: char) -> [u8; 7] {
    match c {
        'A' => [14, 17, 17, 31, 17, 17, 17],
        'C' => [14, 17, 16, 16, 16, 17, 14],
        'E' => [31, 16, 16, 30, 16, 16, 31],
        'G' => [14, 17, 16, 23, 17, 17, 14],
        'K' => [17, 18, 20, 24, 20, 18, 17],
        'N' => [17, 25, 25, 21, 19, 19, 17],
        'O' => [14, 17, 17, 17, 17, 17, 14],
        'P' => [30, 17, 17, 30, 16, 16, 16],
        'R' => [30, 17, 17, 30, 20, 18, 17],
        'T' => [31, 4, 4, 4, 4, 4, 4],
        'W' => [17, 17, 17, 21, 21, 21, 10],
        'Y' => [17, 17, 10, 4, 4, 4, 4],
        _ => [0; 7],
    }
}
