/**
 * @relay/sdk — native JavaScript/TypeScript client for the Relay gateway.
 */

export {
  RelayClient,
  relayModelTarget,
  relayFunctionTarget,
} from "./client.js";
export type {
  RelayClientOptions,
  FetchLike,
  FetchLikeResponse,
} from "./client.js";

export {
  RelayApiError,
  AuthError,
  InsufficientCreditsError,
  errorFromResponse,
} from "./errors.js";
export type { RelayErrorBody } from "./errors.js";

export {
  iterateSseData,
  parseChatCompletionStream,
} from "./stream.js";

export type {
  ChatRole,
  ChatMessage,
  ToolCall,
  RelayExtensions,
  StreamOptions,
  ChatCompletionParams,
  FinishReason,
  Usage,
  ChatCompletionChoice,
  ChatCompletion,
  ChatCompletionDelta,
  ChatCompletionChunkChoice,
  ChatCompletionChunk,
  RelayModel,
  CreditStatus,
} from "./types.js";
