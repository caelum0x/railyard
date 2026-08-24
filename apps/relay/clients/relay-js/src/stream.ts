/**
 * SSE parsing for streamed chat completions.
 *
 * The gateway streams `text/event-stream` frames separated by a blank line.
 * Each frame carries one or more `data:` lines; the terminal frame is the
 * literal `data: [DONE]`. Usage arrives only in the final data chunk when
 * `stream_options.include_usage` is set.
 */

import type { ChatCompletionChunk } from "./types.js";

const DONE_TOKEN = "[DONE]";

/**
 * Turn a byte stream (fetch Response.body) into decoded SSE data payloads.
 * Yields the raw JSON string of each `data:` frame, excluding the [DONE]
 * sentinel which terminates iteration.
 */
export async function* iterateSseData(
  stream: ReadableStream<Uint8Array>,
): AsyncGenerator<string, void, unknown> {
  const reader = stream.getReader();
  const decoder = new TextDecoder();
  let buffer = "";

  try {
    for (;;) {
      const { done, value } = await reader.read();
      if (done) break;
      buffer += decoder.decode(value, { stream: true });

      // Frames are separated by a blank line. Handle both \n\n and \r\n\r\n.
      let sep = findSeparator(buffer);
      while (sep !== -1) {
        const rawFrame = buffer.slice(0, sep.index);
        buffer = buffer.slice(sep.index + sep.length);

        const data = extractData(rawFrame);
        if (data !== null) {
          if (data === DONE_TOKEN) return;
          yield data;
        }
        sep = findSeparator(buffer);
      }
    }

    // Flush any trailing frame that lacked a terminating blank line.
    const tail = extractData(buffer);
    if (tail !== null && tail !== DONE_TOKEN) {
      yield tail;
    }
  } finally {
    reader.releaseLock();
  }
}

/**
 * Parse an SSE byte stream into typed ChatCompletionChunk objects.
 */
export async function* parseChatCompletionStream(
  stream: ReadableStream<Uint8Array>,
): AsyncGenerator<ChatCompletionChunk, void, unknown> {
  for await (const data of iterateSseData(stream)) {
    let chunk: ChatCompletionChunk;
    try {
      chunk = JSON.parse(data) as ChatCompletionChunk;
    } catch (cause) {
      throw new Error(`Failed to parse SSE chunk as JSON: ${data}`, { cause });
    }
    yield chunk;
  }
}

interface Separator {
  index: number;
  length: number;
}

function findSeparator(buffer: string): Separator | -1 {
  const dbl = buffer.indexOf("\n\n");
  const crlf = buffer.indexOf("\r\n\r\n");
  if (crlf !== -1 && (dbl === -1 || crlf < dbl)) {
    return { index: crlf, length: 4 };
  }
  if (dbl !== -1) {
    return { index: dbl, length: 2 };
  }
  return -1;
}

/**
 * Extract the concatenated `data:` payload from a single SSE frame, or null
 * if the frame carries no data lines (e.g. keep-alive comments starting `:`).
 */
function extractData(frame: string): string | null {
  const lines = frame.split(/\r?\n/);
  const dataParts: string[] = [];
  for (const line of lines) {
    if (line.startsWith("data:")) {
      // Strip the "data:" prefix and a single optional leading space.
      dataParts.push(line.slice(5).replace(/^ /, ""));
    }
  }
  if (dataParts.length === 0) return null;
  return dataParts.join("\n");
}
