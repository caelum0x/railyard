# frozen_string_literal: true

module Relay
  # Base class for every error raised by the SDK.
  class Error < StandardError; end

  class ConfigurationError < Error; end
  class ProtocolError < Error; end
  class TransportError < Error; end
  class ResponseTooLargeError < ProtocolError
    attr_reader :limit

    def initialize(limit)
      super("Relay response exceeded #{limit} bytes")
      @limit = limit
    end
  end

  # Raised when the SDK is asked for something the Relay gateway does not
  # expose as a first-class HTTP route (e.g. a dedicated balance endpoint).
  class NotSupportedError < Error; end

  # Raised for any non-2xx HTTP response from the gateway. Carries the parsed
  # `error.type` / `error.code` fields from the OpenAI-compatible error body.
  class APIError < Error
    attr_reader :status, :type, :code, :body

    def initialize(message, status: nil, type: nil, code: nil, body: nil)
      super(message)
      @status = status
      @type = type
      @code = code
      @body = body
    end
  end

  # HTTP 401 / 403 — bad or missing `Authorization: Bearer <key>`.
  class AuthenticationError < APIError; end

  # HTTP 402 family — billing / insufficient credits.
  class BillingError < APIError; end

  # HTTP 402 with `error.type == "insufficient_credits"` from the gateway
  # billing middleware.
  class InsufficientCreditsError < BillingError; end

  class PaymentRequiredError < BillingError
    attr_reader :payment

    def initialize(payment, body: nil)
      super(
        "Relay requires AITP payment", status: 402, type: "payment_required",
        code: "aitp_payment_required", body: body
      )
      @payment = payment
    end
  end

  # HTTP 409 returned when a paid retry names a request that already captured.
  class AlreadySettledError < APIError
    attr_reader :evidence

    def self.valid_evidence?(evidence)
      return false unless evidence.is_a?(Hash)
      receipt = evidence["receipt"]
      return false unless receipt.is_a?(Hash)

      evidence["request_id"].is_a?(String) && !evidence["request_id"].empty? &&
        evidence["principal"].is_a?(String) && !evidence["principal"].empty? &&
        evidence["settled_at_ms"].is_a?(Integer) &&
        receipt["sequence"].is_a?(Integer) && receipt["amount_micros"].is_a?(Integer) &&
        %w[operation operation_hash receipt_hash signature].all? do |field|
          receipt[field].is_a?(String)
        end
    end

    def initialize(evidence, body: nil)
      super(
        "Relay request already settled", status: 409, type: "request_already_settled",
        code: "request_already_settled", body: body
      )
      @evidence = evidence
    end
  end

  class UnverifiedSettlementEvidenceError < APIError
    attr_reader :evidence

    def initialize(evidence, body: nil)
      super(
        "Relay returned settlement evidence but no pinned Mint verifier is configured",
        status: 409, type: "unverified_settlement_evidence",
        code: "unverified_settlement_evidence", body: body
      )
      @evidence = evidence
    end
  end

  class InvalidSettlementEvidenceError < APIError
    attr_reader :evidence

    def initialize(evidence, body: nil)
      super(
        "Relay settlement evidence failed independent Mint verification",
        status: 409, type: "invalid_settlement_evidence",
        code: "invalid_settlement_evidence", body: body
      )
      @evidence = evidence
    end
  end

  # HTTP 404.
  class NotFoundError < APIError; end

  # HTTP 429.
  class RateLimitError < APIError; end

  # HTTP 5xx.
  class ServerError < APIError; end
end
