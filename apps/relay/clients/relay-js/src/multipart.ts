const encoder = new TextEncoder();
let fallbackBoundarySequence = 0;

export interface MultipartField {
  name: string;
  value: string;
}

export interface MultipartPart {
  name: string;
  filename: string;
  contentType: string;
  bytes: Uint8Array;
}

/** Binary-safe multipart/form-data builder for Relay extension endpoints. */
export class MultipartForm {
  private readonly fields: MultipartField[] = [];
  private readonly files: MultipartPart[] = [];

  text(name: string, value: string): this {
    this.fields.push({ name: validToken(name, "multipart field name"), value });
    return this;
  }

  file(
    name: string,
    filename: string,
    contentType: string,
    bytes: Uint8Array | ArrayBuffer,
  ): this {
    this.files.push({
      name: validToken(name, "multipart part name"),
      filename: validToken(filename, "multipart filename"),
      contentType: validContentType(contentType),
      bytes: bytes instanceof Uint8Array ? bytes.slice() : new Uint8Array(bytes.slice(0)),
    });
    return this;
  }

  /** Encode once, then reuse the exact bytes for an AITP-authorized retry. */
  encode(): { contentType: string; body: Uint8Array } {
    const boundary = randomBoundary();
    const chunks: Uint8Array[] = [];
    for (const field of this.fields) {
      chunks.push(
        encoder.encode(
          `--${boundary}\r\nContent-Disposition: form-data; name="${field.name}"\r\n\r\n`,
        ),
        encoder.encode(field.value),
        encoder.encode("\r\n"),
      );
    }
    for (const file of this.files) {
      chunks.push(
        encoder.encode(
          `--${boundary}\r\nContent-Disposition: form-data; name="${file.name}"; filename="${file.filename}"\r\nContent-Type: ${file.contentType}\r\n\r\n`,
        ),
        file.bytes,
        encoder.encode("\r\n"),
      );
    }
    chunks.push(encoder.encode(`--${boundary}--\r\n`));
    const length = chunks.reduce((total, chunk) => total + chunk.byteLength, 0);
    const body = new Uint8Array(length);
    let offset = 0;
    for (const chunk of chunks) {
      body.set(chunk, offset);
      offset += chunk.byteLength;
    }
    return { contentType: `multipart/form-data; boundary=${boundary}`, body };
  }
}

function randomBoundary(): string {
  if (globalThis.crypto?.getRandomValues) {
    const bytes = globalThis.crypto.getRandomValues(new Uint8Array(16));
    return `relay-sdk-${Array.from(bytes, (byte) => byte.toString(16).padStart(2, "0")).join("")}`;
  }
  // Some React Native / Expo runtimes expose fetch and typed arrays without Web
  // Crypto. Boundary secrecy is not a security property; uniqueness within the
  // process is sufficient for multipart framing.
  fallbackBoundarySequence += 1;
  return `relay-sdk-${Date.now().toString(16)}-${fallbackBoundarySequence.toString(16)}`;
}

function validToken(value: string, label: string): string {
  if (!value || /[\r\n"]/.test(value)) {
    throw new Error(`${label} is empty or contains unsafe header characters`);
  }
  return value;
}

function validContentType(value: string): string {
  if (!value.includes("/") || /[\r\n]/.test(value) || !value.trim()) {
    throw new Error("multipart content type is invalid");
  }
  return value;
}
