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
  AlreadySettledError,
  InvalidSettlementEvidenceError,
  AuthError,
  InsufficientCreditsError,
  PaymentRequiredError,
  UnverifiedSettlementEvidenceError,
  errorFromResponse,
} from "./errors.js";
export type { RelayErrorBody } from "./errors.js";

export {
  AITP_CHALLENGE_HEADER,
  AITP_AUTHORIZATION_HEADER,
  AITP_REQUEST_ID_HEADER,
  AITP_VERSION_HEADER,
  AITP_RECEIPT_HEADER,
  decodeAitpChallenge,
  paymentRequiredFromHeaders,
} from "./aitp.js";

export {
  iterateSseData,
  parseChatCompletionStream,
} from "./stream.js";

export { MultipartForm } from "./multipart.js";
export type { MultipartField, MultipartPart } from "./multipart.js";

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
  AitpChallenge,
  PaymentRequired,
  AitpAuthorizer,
  GatewayStatus,
  GatewayHealth,
  DataEnvelope,
  CatalogSnapshot,
  CatalogModel,
  ModelCapabilities,
  ModelPrice,
  PricingSource,
  PriceQuoteRequest,
  PriceQuote,
  SettlementReceiptProof,
  SettlementEvidence,
  SettlementEvidenceVerifier,
  CostEvent,
  PendingCostEvent,
  CostEventState,
  CostEventListQuery,
  CostEventPage,
} from "./types.js";
