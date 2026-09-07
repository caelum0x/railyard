# frozen_string_literal: true

require "json"
require "uri"

require_relative "version"
require_relative "errors"
require_relative "aitp"
require_relative "transport"
require_relative "multipart"

module Relay
  class Client
    PATHS = {
      status: "status",
      health: "health",
      catalog: "v1/relay/catalog",
      quotes: "v1/relay/quotes",
      chat: "openai/v1/chat/completions",
      embeddings: "openai/v1/embeddings",
      inference: "inference",
      batch: "batch_inference",
      cost_events: "v1/billing/cost-events"
    }.freeze

    attr_reader :base_url

    def initialize(
      base_url:, api_key: nil, transport: nil, known_models: nil,
      read_timeout: 300, max_response_bytes: 16 * 1024 * 1024,
      max_sse_event_bytes: 2 * 1024 * 1024, on_abandoned_stream: nil,
      settlement_evidence_verifier: nil
    )
      @base_url = normalize_base_url(base_url)
      @api_key = api_key.nil? ? nil : Aitp.validate_header(api_key, "Relay API key")
      if max_response_bytes <= 0 || max_sse_event_bytes <= 0
        raise ConfigurationError, "Relay response limits must be positive"
      end
      @transport = transport || NetHTTPTransport.new(read_timeout: read_timeout)
      @known_models = Array(known_models).map(&:to_s).freeze
      @max_response_bytes = max_response_bytes
      @max_sse_event_bytes = max_sse_event_bytes
      @on_abandoned_stream = on_abandoned_stream
      @settlement_evidence_verifier = settlement_evidence_verifier
    end

    def status
      request_json(:get, PATHS[:status])
    end

    def health
      request_json(:get, PATHS[:health], accepted_status: 503)
    end

    def catalog
      data(request_json(:get, PATHS[:catalog]))
    end

    def quote(model:, input_tokens:, output_tokens:)
      raise ArgumentError, "quote model is required" if blank?(model)
      validate_token_count(input_tokens, "input_tokens")
      validate_token_count(output_tokens, "output_tokens")
      data(request_json(
        :post, PATHS[:quotes],
        body: { model: model, input_tokens: input_tokens, output_tokens: output_tokens }
      ))
    end

    def chat_completions(params)
      request_json(:post, PATHS[:chat], body: build_chat_body(params, stream: false))
    end

    def paid_chat_completions(params, authorizer:)
      paid_json(PATHS[:chat], build_chat_body(params, stream: false), authorizer)
    end

    def chat_completions_stream(params, &block)
      return enum_for(:chat_completions_stream, params) unless block

      stream_once(PATHS[:chat], build_chat_body(params, stream: true), &block)
    end

    def paid_chat_completions_stream(params, authorizer:, &block)
      unless block
        return enum_for(:paid_chat_completions_stream, params, authorizer: authorizer)
      end
      body = build_chat_body(params, stream: true)
      first = stream_once(PATHS[:chat], body, allow_payment: true, &block)
      return nil if first.status >= 200 && first.status < 300

      payment_error = error_for(first)
      raise payment_error unless payment_error.is_a?(PaymentRequiredError)
      request_id = payment_error.payment.request_id
      raise ProtocolError, "AITP challenge is missing its request id" unless request_id

      authorization = Aitp.validate_header(authorizer.call(payment_error.payment), "AITP authorization")
      second = stream_once(
        PATHS[:chat], body,
        extra_headers: {
          AITP_AUTHORIZATION_HEADER => authorization,
          AITP_REQUEST_ID_HEADER => request_id
        },
        allow_payment: true,
        &block
      )
      raise error_for(second) unless second.status >= 200 && second.status < 300
      nil
    end

    def embeddings(params)
      request_json(:post, PATHS[:embeddings], body: params)
    end

    def paid_embeddings(params, authorizer:)
      paid_json(PATHS[:embeddings], params, authorizer)
    end

    def inference(params)
      request_json(:post, PATHS[:inference], body: params)
    end

    def paid_inference(params, authorizer:)
      paid_json(PATHS[:inference], params, authorizer)
    end

    def batch_inference(params)
      request_json(:post, PATHS[:batch], body: params)
    end

    def paid_batch_inference(params, authorizer:)
      paid_json(PATHS[:batch], params, authorizer)
    end

    def batch_inference_status(batch_id)
      Aitp.validate_uuid(batch_id, "batch id")
      request_json(:get, "#{PATHS[:batch]}/#{batch_id}")
    end

    def settlement_evidence(request_id)
      evidence = settlement_evidence_unverified(request_id)
      unless AlreadySettledError.valid_evidence?(evidence)
        raise ProtocolError, "Relay returned malformed settlement evidence"
      end
      unless @settlement_evidence_verifier
        raise UnverifiedSettlementEvidenceError.new(evidence, body: evidence)
      end
      verified = begin
        @settlement_evidence_verifier.call(evidence) == true
      rescue StandardError
        false
      end
      raise InvalidSettlementEvidenceError.new(evidence, body: evidence) unless verified

      evidence
    end

    def settlement_evidence_unverified(request_id)
      Aitp.validate_uuid(request_id, "request id")
      data(request_json(:get, "v1/billing/settlements/#{request_id}"))
    end

    def cost_events(state: nil, before: nil, limit: 50)
      raise ArgumentError, "cost event limit must be between 1 and 100" unless (1..100).cover?(limit)
      states = [nil, "pending", "published", "dead_lettered"]
      raise ArgumentError, "invalid cost event state" unless states.include?(state)
      Aitp.validate_uuid(before, "cost event cursor") if before
      query = { limit: limit }
      query[:state] = state if state
      query[:before] = before if before
      data(request_json(:get, "#{PATHS[:cost_events]}?#{URI.encode_www_form(query)}"))
    end

    def cost_event(event_id)
      Aitp.validate_uuid(event_id, "cost event id")
      data(request_json(:get, "#{PATHS[:cost_events]}/#{event_id}"))
    end

    def replay_cost_event(event_id)
      Aitp.validate_uuid(event_id, "cost event id")
      data(request_json(:post, "#{PATHS[:cost_events]}/#{event_id}/replay", body: {}))
    end

    def custom_json(method:, path:, body: nil)
      request_json(method.to_s.downcase.to_sym, path, body: body)
    end

    def multipart(path, form)
      content_type, body = form.encode
      response = raw_request(path, body, content_type)
      raise error_for(response) unless response.status >= 200 && response.status < 300
      parse_json(response.body, response.headers)
    end

    def paid_multipart(path, form, authorizer:)
      content_type, body = form.encode
      first = raw_request(path, body, content_type)
      return parse_json(first.body, first.headers) if first.status >= 200 && first.status < 300

      payment_error = error_for(first)
      raise payment_error unless payment_error.is_a?(PaymentRequiredError)
      request_id = payment_error.payment.request_id
      raise ProtocolError, "AITP challenge is missing its request id" unless request_id
      authorization = Aitp.validate_header(authorizer.call(payment_error.payment), "AITP authorization")
      second = raw_request(
        path, body, content_type,
        AITP_AUTHORIZATION_HEADER => authorization,
        AITP_REQUEST_ID_HEADER => request_id
      )
      raise error_for(second) unless second.status >= 200 && second.status < 300
      parse_json(second.body, second.headers)
    end

    # Backward-compatible local list. New callers should use #catalog.
    def models
      @known_models.dup
    end

    # Compatibility-only availability probe. Dry-run avoids provider execution.
    def credits(probe_model: nil, probe_params: nil)
      params = probe_params || default_probe(probe_model)
      chat_completions(params)
      { sufficient: true, checked_via: "dry_run_inference_probe" }
    rescue PaymentRequiredError, InsufficientCreditsError => e
      {
        sufficient: false, checked_via: "dry_run_inference_probe",
        error: e.message, type: e.type, code: e.code
      }
    end
    alias balance credits

    private

    def request_json(method, path, body: nil, accepted_status: nil, extra_headers: nil)
      response = @transport.request(
        method: method,
        url: url_for(path),
        headers: headers(accept: "application/json", body: !body.nil?, extra: extra_headers),
        body: body.nil? ? nil : encode(body)
      )
      unless (response.status >= 200 && response.status < 300) || response.status == accepted_status
        raise error_for(response)
      end
      parse_json(response.body, response.headers)
    end

    def paid_json(path, body, authorizer)
      first = raw_json_request(path, body)
      return parse_json(first.body, first.headers) if first.status >= 200 && first.status < 300

      payment_error = error_for(first)
      raise payment_error unless payment_error.is_a?(PaymentRequiredError)
      request_id = payment_error.payment.request_id
      raise ProtocolError, "AITP challenge is missing its request id" unless request_id
      authorization = Aitp.validate_header(authorizer.call(payment_error.payment), "AITP authorization")
      second = raw_json_request(
        path, body,
        AITP_AUTHORIZATION_HEADER => authorization,
        AITP_REQUEST_ID_HEADER => request_id
      )
      raise error_for(second) unless second.status >= 200 && second.status < 300
      parse_json(second.body, second.headers)
    end

    def raw_json_request(path, body, extra = nil)
      raw_request(path, encode(body), "application/json", extra)
    end

    def raw_request(path, body, content_type, extra = nil)
      @transport.request(
        method: :post, url: url_for(path),
        headers: headers(
          accept: "application/json", body: true, extra: extra,
          content_type: content_type
        ),
        body: body
      )
    end

    def stream_once(path, body, extra_headers: nil, allow_payment: false)
      buffer = +"".b
      done = false
      metadata = nil
      response = @transport.stream(
        method: :post, url: url_for(path),
        headers: headers(accept: "text/event-stream", body: true, extra: extra_headers),
        body: encode(body),
        on_response: ->(value) { metadata = value }
      ) do |raw_chunk|
        next if done
        buffer << raw_chunk
        if buffer.bytesize > @max_sse_event_bytes && !sse_separator?(buffer)
          raise ProtocolError, "Relay SSE event exceeded #{@max_sse_event_bytes} bytes"
        end
        done = drain_sse(buffer) { |chunk| yield chunk }
      end
      if response.status >= 400 && !(allow_payment && response.status == 402)
        raise error_for(response)
      end
      response
    ensure
      successful = metadata && metadata.status >= 200 && metadata.status < 300
      request_id = metadata && metadata.headers[AITP_REQUEST_ID_HEADER]
      @on_abandoned_stream.call(request_id) if successful && !done && request_id && @on_abandoned_stream
    end

    def drain_sse(buffer)
      while (separator = next_separator(buffer))
        index, length = separator
        frame = buffer.slice!(0, index + length).byteslice(0, index)
        if frame.bytesize > @max_sse_event_bytes
          raise ProtocolError, "Relay SSE event exceeded #{@max_sse_event_bytes} bytes"
        end
        data_value = extract_sse_data(frame)
        next if data_value.nil?
        return true if data_value == "[DONE]"
        parsed = JSON.parse(data_value)
        raise ProtocolError, "Relay SSE data must be a JSON object" unless parsed.is_a?(Hash)
        yield parsed
      end
      false
    rescue JSON::ParserError => e
      raise ProtocolError, "Relay SSE data is not valid JSON: #{e.message}"
    end

    def next_separator(buffer)
      choices = []
      choices << [buffer.index("\n\n"), 2] if buffer.include?("\n\n")
      choices << [buffer.index("\r\n\r\n"), 4] if buffer.include?("\r\n\r\n")
      choices.min_by(&:first)
    end

    def sse_separator?(buffer)
      buffer.include?("\n\n") || buffer.include?("\r\n\r\n")
    end

    def extract_sse_data(frame)
      text = frame.dup.force_encoding(Encoding::UTF_8)
      raise ProtocolError, "Relay SSE frame is not valid UTF-8" unless text.valid_encoding?
      lines = text.gsub("\r\n", "\n").split("\n")
      data_lines = lines.select { |line| line.start_with?("data:") }
      return nil if data_lines.empty?
      data_lines.map { |line| line.sub(/\Adata:[ \t]?/, "") }.join("\n")
    end

    def error_for(response)
      check_size(response.body, response.headers)
      parsed = safe_json(response.body)
      return PaymentRequiredError.new(Aitp.payment_required(response.headers), body: parsed) if response.status == 402

      error = parsed.is_a?(Hash) && parsed["error"].is_a?(Hash) ? parsed["error"] : {}
      message = error["message"] || "Relay API error (HTTP #{response.status})"
      if response.status == 409 && error["code"] == "request_already_settled" &&
         AlreadySettledError.valid_evidence?(parsed["data"])
        evidence = parsed["data"]
        return UnverifiedSettlementEvidenceError.new(evidence, body: parsed) unless @settlement_evidence_verifier

        verified = begin
          @settlement_evidence_verifier.call(evidence) == true
        rescue StandardError
          false
        end
        return AlreadySettledError.new(evidence, body: parsed) if verified

        return InvalidSettlementEvidenceError.new(evidence, body: parsed)
      end
      klass = case response.status
              when 401, 403 then AuthenticationError
              when 404 then NotFoundError
              when 429 then RateLimitError
              when 500..599 then ServerError
              else APIError
              end
      klass.new(
        message, status: response.status, type: error["type"], code: error["code"],
        body: parsed || response.body
      )
    end

    def parse_json(body, response_headers = {})
      body ||= ""
      check_size(body, response_headers)
      parsed = JSON.parse(body)
      raise ProtocolError, "Relay JSON response must be an object" unless parsed.is_a?(Hash)
      parsed
    rescue JSON::ParserError => e
      raise ProtocolError, "Relay returned malformed JSON: #{e.message}"
    end

    def safe_json(body)
      return nil if body.nil? || body.empty?
      JSON.parse(body)
    rescue JSON::ParserError
      nil
    end

    def check_size(body, response_headers)
      declared = response_headers["content-length"].to_i
      size = body.nil? ? 0 : body.bytesize
      raise ResponseTooLargeError, @max_response_bytes if declared > @max_response_bytes || size > @max_response_bytes
    end

    def data(envelope)
      value = envelope["data"]
      raise ProtocolError, "Relay data envelope is malformed" unless value.is_a?(Hash)
      value
    end

    def encode(body)
      JSON.generate(body)
    rescue JSON::GeneratorError => e
      raise ArgumentError, "Relay request is not valid JSON: #{e.message}"
    end

    def headers(accept:, body:, extra:, content_type: "application/json")
      values = { "Accept" => accept }
      values["Content-Type"] = content_type if body
      values["Authorization"] = "Bearer #{@api_key}" if @api_key
      (extra || {}).each do |name, value|
        values[Aitp.validate_header(name, "header name")] = Aitp.validate_header(value, "header #{name}")
      end
      values
    end

    def url_for(path)
      raise ArgumentError, "Relay request path must be safe and relative" unless safe_path?(path)
      "#{@base_url}/#{path}"
    end

    def safe_path?(path)
      path = path.to_s
      plain = path.split("?", 2).first
      !path.empty? && !path.start_with?("/") && !path.include?("://") &&
        !path.include?("\r") && !path.include?("\n") &&
        !plain.split("/").include?("..") && !plain.downcase.include?("%2e")
    end

    def normalize_base_url(raw)
      uri = URI.parse(raw.to_s)
      unless %w[http https].include?(uri.scheme) && uri.host
        raise ConfigurationError, "Relay base URL must be absolute HTTP(S)"
      end
      if uri.userinfo || uri.query || uri.fragment
        raise ConfigurationError, "Relay base URL must not contain credentials, query, or fragment"
      end
      raw.to_s.sub(%r{/+\z}, "")
    rescue URI::InvalidURIError => e
      raise ConfigurationError, "Relay base URL is invalid: #{e.message}"
    end

    def build_chat_body(params, stream:)
      raise ArgumentError, "params must be a Hash" unless params.is_a?(Hash)
      body = stringify_keys(params)
      raise ArgumentError, "params[:model] is required" if blank?(body["model"])
      unless body["messages"].is_a?(Array) && !body["messages"].empty?
        raise ArgumentError, "params[:messages] must be non-empty"
      end
      raise ArgumentError, "Relay only supports n=1" if body.key?("n") && body["n"] != 1
      body["stream"] = stream
      if stream
        options = body["stream_options"].is_a?(Hash) ? stringify_keys(body["stream_options"]) : {}
        options["include_usage"] = true
        body["stream_options"] = options
      end
      body
    end

    def default_probe(model)
      model ||= @known_models.first
      raise ArgumentError, "credits/balance needs a probe model" if blank?(model)
      {
        "model" => model,
        "messages" => [{ "role" => "user", "content" => "ping" }],
        "max_tokens" => 1,
        "tensorzero::dryrun" => true
      }
    end

    def validate_token_count(value, name)
      raise ArgumentError, "#{name} must be a non-negative integer" unless value.is_a?(Integer) && value >= 0
    end

    def stringify_keys(hash)
      hash.each_with_object({}) { |(key, value), out| out[key.to_s] = value }
    end

    def blank?(value)
      value.nil? || (value.respond_to?(:empty?) && value.empty?)
    end
  end
end
