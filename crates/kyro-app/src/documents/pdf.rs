//! A bounded plain-text PDF renderer. It interprets no HTML, script or URL.
use super::*;
use std::fmt::Write;

#[derive(Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(super) enum Format {
    #[default]
    Html,
    Pdf,
}

fn encoded(c: char) -> AppResult<u8> {
    match c {
        ' '..='~' | '\u{a0}'..='\u{ff}' => Ok(c as u8),
        '€' => Ok(0x80),
        '‘' => Ok(0x91),
        '’' => Ok(0x92),
        '“' => Ok(0x93),
        '”' => Ok(0x94),
        '–' => Ok(0x96),
        '—' => Ok(0x97),
        '…' => Ok(0x85),
        _ => Err(AppError::invalid("pdf_character_unavailable")),
    }
}
pub(super) fn render(escaped: &str) -> AppResult<Vec<u8>> {
    if escaped.len() > MAX_EDITORIAL_BYTES {
        return Err(AppError::Quota);
    }
    // Only undo entities produced by escape_html. Decode ampersand last so a
    // literal user entity is not interpreted a second time.
    let plain = escaped
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#x27;", "'")
        .replace("&amp;", "&");
    let mut lines = Vec::new();
    let mut line = Vec::new();
    for c in plain.chars() {
        if c == '\n' {
            lines.push(std::mem::take(&mut line));
            continue;
        }
        if c == '\r' {
            continue;
        }
        line.push(encoded(if c == '\t' { ' ' } else { c })?);
        if line.len() == 80 {
            lines.push(std::mem::take(&mut line));
        }
        if lines.len() > 800 {
            return Err(AppError::invalid("pdf_page_limit"));
        }
    }
    if !line.is_empty() || lines.is_empty() {
        lines.push(line);
    }
    let pages = lines.len().div_ceil(40);
    if pages > 20 {
        return Err(AppError::invalid("pdf_page_limit"));
    }
    let mut objects = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".as_bytes().to_vec(),
        Vec::new(),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>"
            .as_bytes()
            .to_vec(),
    ];
    let children = (0..pages)
        .map(|i| format!("{} 0 R", 4 + i * 2))
        .collect::<Vec<_>>()
        .join(" ");
    objects[1] = format!("<< /Type /Pages /Count {pages} /Kids [{children}] >>").into_bytes();
    for (page, page_lines) in lines.chunks(40).enumerate() {
        let stream_id = 5 + page * 2;
        objects.push(format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 595 842] /Resources << /Font << /F1 3 0 R >> >> /Contents {stream_id} 0 R >>").into_bytes());
        let mut stream = "BT\n/F1 11 Tf\n16 TL\n50 790 Td\n".to_string();
        for line in page_lines {
            stream.push('<');
            for b in line {
                write!(&mut stream, "{b:02X}").map_err(|_| AppError::Internal)?;
            }
            stream.push_str("> Tj\nT*\n");
        }
        stream.push_str("ET\n");
        objects.push(
            format!("<< /Length {} >>\nstream\n{stream}endstream", stream.len()).into_bytes(),
        );
    }
    let mut output = b"%PDF-1.4\n%\xe2\xe3\xcf\xd3\n".to_vec();
    let mut offsets = vec![0];
    for (index, object) in objects.iter().enumerate() {
        offsets.push(output.len());
        output.extend(format!("{} 0 obj\n", index + 1).as_bytes());
        output.extend(object);
        output.extend(b"\nendobj\n");
    }
    let xref = output.len();
    output.extend(format!("xref\n0 {}\n0000000000 65535 f \n", offsets.len()).as_bytes());
    for offset in offsets.iter().skip(1) {
        output.extend(format!("{offset:010} 00000 n \n").as_bytes());
    }
    output.extend(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            offsets.len()
        )
        .as_bytes(),
    );
    if output.len() > 524288 {
        return Err(AppError::Quota);
    }
    Ok(output)
}
