# frozen_string_literal: true

module Relay
  # Base class for every error raised by the SDK.
  class Error < StandardError; end

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

  # HTTP 404.
  class NotFoundError < APIError; end

  # HTTP 429.
  class RateLimitError < APIError; end

  # HTTP 5xx.
  class ServerError < APIError; end
end
