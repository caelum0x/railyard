//! Minimal, dependency-free PDF writer.
//!
//! Produces a single-page A4 PDF with text lines using the standard Helvetica
//! font (a PDF base-14 font, so no font embedding is needed). The output is a
//! real, valid PDF — `%PDF-1.4` header, cross-reference table, and trailer —
//! that `file`, `pdfinfo`, and `pdftotext` all read.

/// A4 page dimensions in PDF points (1/72 inch).
pub const PAGE_WIDTH: f32 = 595.0;
pub const PAGE_HEIGHT: f32 = 842.0;

/// One line of text placed at (`x`, `y` from the bottom-left) at `size` pt.
pub struct TextLine {
    pub x: f32,
    pub y: f32,
    pub size: f32,
    pub text: String,
    /// Use the bold font variant.
    pub bold: bool,
}

impl TextLine {
    pub fn new(x: f32, y: f32, size: f32, text: impl Into<String>) -> Self {
        Self {
            x,
            y,
            size,
            text: text.into(),
            bold: false,
        }
    }
    pub fn bold(mut self) -> Self {
        self.bold = true;
        self
    }
}

/// Escape a string for a PDF literal string `( … )`.
fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '(' | ')' | '\\' => {
                out.push('\\');
                out.push(c);
            }
            // Keep it to printable ASCII; replace the rest to stay WinAnsi-safe.
            c if (c as u32) < 128 => out.push(c),
            _ => out.push('?'),
        }
    }
    out
}

/// Build the page content stream bytes from text lines.
fn content_stream(lines: &[TextLine]) -> Vec<u8> {
    let mut content = String::new();
    for line in lines {
        let font = if line.bold { "F2" } else { "F1" };
        content.push_str(&format!(
            "BT /{} {} Tf {:.2} {:.2} Td ({}) Tj ET\n",
            font,
            line.size,
            line.x,
            line.y,
            escape(&line.text)
        ));
    }
    content.into_bytes()
}

/// Assemble a PDF file from pre-rendered object bodies, writing the header,
/// cross-reference table, and trailer. `objects[0]` must be the catalog.
fn assemble(objects: &[Vec<u8>]) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::new();
    out.extend_from_slice(b"%PDF-1.4\n%\xE2\xE3\xCF\xD3\n");
    let mut offsets = Vec::with_capacity(objects.len());
    for (i, body) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
        out.extend_from_slice(body);
        out.extend_from_slice(b"\nendobj\n");
    }
    let xref_start = out.len();
    let count = objects.len() + 1;
    out.extend_from_slice(format!("xref\n0 {count}\n").as_bytes());
    out.extend_from_slice(b"0000000000 65535 f \n");
    for off in &offsets {
        out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!("trailer\n<< /Size {count} /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
            .as_bytes(),
    );
    out
}

/// Render text lines into a valid PDF document (bytes).
pub fn render(lines: &[TextLine]) -> Vec<u8> {
    let content_bytes = content_stream(lines);

    // Objects: 1 catalog, 2 pages, 3 page, 4 content, 5 font(Helvetica),
    // 6 font(Helvetica-Bold).
    let mut objects: Vec<Vec<u8>> = Vec::new();
    objects.push(b"<< /Type /Catalog /Pages 2 0 R >>".to_vec());
    objects.push(b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec());
    objects.push(
        format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {:.0} {:.0}] \
             /Resources << /Font << /F1 5 0 R /F2 6 0 R >> >> /Contents 4 0 R >>",
            PAGE_WIDTH, PAGE_HEIGHT
        )
        .into_bytes(),
    );
    // Content stream object (with /Length).
    let mut stream_obj = format!("<< /Length {} >>\nstream\n", content_bytes.len()).into_bytes();
    stream_obj.extend_from_slice(&content_bytes);
    stream_obj.extend_from_slice(b"\nendstream");
    objects.push(stream_obj);
    objects.push(b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec());
    objects.push(b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica-Bold >>".to_vec());

    assemble(&objects)
}

/// Render text lines into a PDF that carries `xml_bytes` as an embedded,
/// associated file named `filename` (`/AFRelationship /Data`) — the container
/// shape a Factur-X / ZUGFeRD hybrid invoice uses (CII XML inside the PDF).
///
/// This produces a valid PDF with the CII companion attached and associated at
/// the document level. Strict PDF/A-3 conformance additionally requires XMP
/// metadata and an OutputIntent, which this dependency-free writer does not
/// emit — see the crate docs. Object layout (fixed):
/// 1 Catalog, 2 Pages, 3 Page, 4 Content, 5/6 Fonts, 7 EmbeddedFile, 8 Filespec.
pub fn render_with_attachment(lines: &[TextLine], filename: &str, xml_bytes: &[u8]) -> Vec<u8> {
    let content_bytes = content_stream(lines);

    let mut objects: Vec<Vec<u8>> = Vec::new();
    // 1: Catalog — carries the EmbeddedFiles name tree and the /AF array.
    objects.push(
        format!(
            "<< /Type /Catalog /Pages 2 0 R \
             /Names << /EmbeddedFiles << /Names [ ({filename}) 8 0 R ] >> >> \
             /AF [8 0 R] >>"
        )
        .into_bytes(),
    );
    // 2: Pages.
    objects.push(b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec());
    // 3: Page.
    objects.push(
        format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {:.0} {:.0}] \
             /Resources << /Font << /F1 5 0 R /F2 6 0 R >> >> /Contents 4 0 R >>",
            PAGE_WIDTH, PAGE_HEIGHT
        )
        .into_bytes(),
    );
    // 4: Content stream.
    let mut stream_obj = format!("<< /Length {} >>\nstream\n", content_bytes.len()).into_bytes();
    stream_obj.extend_from_slice(&content_bytes);
    stream_obj.extend_from_slice(b"\nendstream");
    objects.push(stream_obj);
    // 5/6: Fonts.
    objects.push(b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec());
    objects.push(b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica-Bold >>".to_vec());
    // 7: EmbeddedFile stream (the CII XML). `#2F` is an escaped `/` in the name.
    let mut ef = format!(
        "<< /Type /EmbeddedFile /Subtype /text#2Fxml /Params << /Size {} >> /Length {} >>\nstream\n",
        xml_bytes.len(),
        xml_bytes.len()
    )
    .into_bytes();
    ef.extend_from_slice(xml_bytes);
    ef.extend_from_slice(b"\nendstream");
    objects.push(ef);
    // 8: Filespec associating the embedded file as invoice data.
    objects.push(
        format!(
            "<< /Type /Filespec /F ({filename}) /UF ({filename}) \
             /AFRelationship /Data /Desc (Factur-X invoice data) \
             /EF << /F 7 0 R /UF 7 0 R >> >>"
        )
        .into_bytes(),
    );

    assemble(&objects)
}
