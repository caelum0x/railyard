# frozen_string_literal: true

require "base64"
require "json"

module Relay
  AITP_CHALLENGE_HEADER = "aitp-challenge"
  AITP_AUTHORIZATION_HEADER = "aitp-authorization"
  AITP_REQUEST_ID_HEADER = "aitp-request-id"
  AITP_VERSION_HEADER = "aitp-version"
  AITP_RECEIPT_HEADER = "aitp-receipt"

  AitpChallenge = Struct.new(
    :payee, :amount_micros, :currency, :resource, :nonce, :expiry, :scheme,
    keyword_init: true
  )
  PaymentRequired = Struct.new(
    :challenge, :request_id, :protocol_version,
    keyword_init: true
  )

  module Aitp
    UUID_PATTERN = /\A[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}\z/i.freeze

    module_function

    def decode_challenge(encoded)
      value = JSON.parse(Base64.urlsafe_decode64(encoded.to_s))
      raise ProtocolError, "Relay returned invalid AITP challenge fields" unless value.is_a?(Hash)

      strings = %w[payee resource nonce]
      valid = strings.all? { |key| value[key].is_a?(String) && !value[key].empty? }
      valid &&= value["amount_micros"].is_a?(Integer) && value["amount_micros"] >= 0
      valid &&= value["expiry"].is_a?(Integer) && value["expiry"] >= 0
      valid &&= value["currency"] == "AIT" && value["scheme"] == "aitp-hold"
      raise ProtocolError, "Relay returned invalid AITP challenge fields" unless valid

      AitpChallenge.new(
        payee: value["payee"], amount_micros: value["amount_micros"],
        currency: value["currency"], resource: value["resource"], nonce: value["nonce"],
        expiry: value["expiry"], scheme: value["scheme"]
      )
    rescue ArgumentError, JSON::ParserError => e
      raise ProtocolError, "Relay returned an invalid AITP challenge encoding: #{e.message}"
    end

    def payment_required(headers)
      normalized = headers.each_with_object({}) { |(key, value), out| out[key.to_s.downcase] = value }
      encoded = normalized[AITP_CHALLENGE_HEADER]
      unless encoded
        raise ProtocolError, "Relay returned HTTP 402 without an AITP challenge header"
      end
      request_id = normalized[AITP_REQUEST_ID_HEADER]
      validate_uuid(request_id, "AITP request id") if request_id
      PaymentRequired.new(
        challenge: decode_challenge(encoded), request_id: request_id,
        protocol_version: normalized[AITP_VERSION_HEADER]
      )
    end

    def validate_uuid(value, name)
      raise ArgumentError, "#{name} must be a UUID" unless UUID_PATTERN.match?(value.to_s)
    end

    def validate_header(value, name)
      string = value.to_s
      if string.strip.empty? || string.include?("\r") || string.include?("\n")
        raise ArgumentError, "#{name} must not be empty or contain newlines"
      end
      string
    end
  end
end
