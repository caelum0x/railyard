use uuid::Uuid;

use crate::error::{RelaySdkError, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MultipartField {
    pub name: String,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MultipartPart {
    pub name: String,
    pub filename: String,
    pub content_type: String,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MultipartForm {
    fields: Vec<MultipartField>,
    files: Vec<MultipartPart>,
}

impl MultipartForm {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn text(mut self, name: impl Into<String>, value: impl Into<String>) -> Result<Self> {
        let name = valid_token(name.into(), "multipart field name")?;
        self.fields.push(MultipartField {
            name,
            value: value.into(),
        });
        Ok(self)
    }

    pub fn file(
        mut self,
        name: impl Into<String>,
        filename: impl Into<String>,
        content_type: impl Into<String>,
        bytes: impl Into<Vec<u8>>,
    ) -> Result<Self> {
        self.files.push(MultipartPart {
            name: valid_token(name.into(), "multipart part name")?,
            filename: valid_token(filename.into(), "multipart filename")?,
            content_type: valid_content_type(content_type.into())?,
            bytes: bytes.into(),
        });
        Ok(self)
    }

    pub(crate) fn encode(self) -> (String, Vec<u8>) {
        let boundary = format!("relay-sdk-{}", Uuid::now_v7().simple());
        let mut body = Vec::new();
        for field in self.fields {
            body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
            body.extend_from_slice(
                format!(
                    "Content-Disposition: form-data; name=\"{}\"\r\n\r\n",
                    field.name
                )
                .as_bytes(),
            );
            body.extend_from_slice(field.value.as_bytes());
            body.extend_from_slice(b"\r\n");
        }
        for file in self.files {
            body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
            body.extend_from_slice(
                format!(
                    "Content-Disposition: form-data; name=\"{}\"; filename=\"{}\"\r\nContent-Type: {}\r\n\r\n",
                    file.name, file.filename, file.content_type
                )
                .as_bytes(),
            );
            body.extend_from_slice(&file.bytes);
            body.extend_from_slice(b"\r\n");
        }
        body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
        (format!("multipart/form-data; boundary={boundary}"), body)
    }
}

fn valid_token(value: String, label: &str) -> Result<String> {
    if value.is_empty() || value.contains(['\r', '\n', '"']) {
        return Err(RelaySdkError::InvalidRequest(format!(
            "{label} is empty or contains unsafe header characters"
        )));
    }
    Ok(value)
}

fn valid_content_type(value: String) -> Result<String> {
    if value.parse::<http::HeaderValue>().is_err() || !value.contains('/') {
        return Err(RelaySdkError::InvalidRequest(
            "multipart content type is invalid".into(),
        ));
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multipart_preserves_binary_bytes_and_closes_boundary() {
        let form = MultipartForm::new()
            .text("purpose", "batch")
            .unwrap()
            .file(
                "file",
                "requests.jsonl",
                "application/jsonl",
                vec![0, 1, 255],
            )
            .unwrap();
        let (content_type, body) = form.encode();
        let boundary = content_type.split("boundary=").nth(1).unwrap();
        assert!(body.windows(3).any(|window| window == [0, 1, 255]));
        assert!(body.ends_with(format!("--{boundary}--\r\n").as_bytes()));
    }
}
