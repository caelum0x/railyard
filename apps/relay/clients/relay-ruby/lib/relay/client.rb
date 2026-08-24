# frozen_string_literal: true

require "json"

require_relative "version"
require_relative "errors"
require_relative "transport"

module Relay
  # Native Ruby client for the Relay gateway (a TensorZero fork).
  #
  # It speaks the gateway's OpenAI-compatible HTTP surface directly — no
  # `openai` gem dependency — targeting:
  #
  #   * POST /openai/v1/chat/completions  (JSON and SSE streaming)
  #
  # Relay addresses models/functions via the `model` field using the
  # `tensorzero::model_name::<m>` / `tensorzero::function_name::<f>` scheme;
  # see Relay.model_target / Relay.function_target.
  class Client
    CHAT_COMPLETIONS_PATH = "/openai/v1/chat/completions"

    attr_reader :base_url

    # @param base_url [String] gateway root, e.g. "http://localhost:3000"
    # @param api_key [String, nil] sent as `Authorization: Bearer <api_key>`
    # @param transport [#request, #stream] injectable HTTP transport (tests)
    # @param known_models [Array<String>] see #models (no public list route exists)
    # @param read_timeout [Integer] seconds for the default transport
    def initialize(base_url:, api_key: nil, transport: nil, known_models: nil, read_timeout: 300)
      raise ArgumentError, "base_url is required" if base_url.nil? || base_url.to_s.empty?

      @base_url = base_url.to_s.sub(%r{/+\z}, "")
      @api_key = api_key
      @transport = transport || NetHTTPTransport.new(read_timeout: read_timeout)
      @known_models = Array(known_models).map(&:to_s)
    end

    # Non-streaming chat completion.
    #
    # @param params [Hash] OpenAI-compatible params; `:model` and `:messages`
    #   are required. Relay-specific keys (e.g. "tensorzero::episode_id") pass
    #   through untouched.
    # @return [Hash] parsed OpenAICompatibleResponse
    def chat_completions(params)
      body = build_chat_body(params, stream: false)
      res = @transport.request(
        method: :post,
        url: url_for(CHAT_COMPLETIONS_PATH),
        headers: headers(accept: "application/json"),
        body: JSON.generate(body)
      )
      raise_for_status(res) if res.status >= 400
      parse_json(res.body)
    end

    # Streaming chat completion over SSE.
    #
    # Sets `stream: true` and (unless the caller opts out) requests usage in the
    # final chunk via `stream_options.include_usage: true`. Each parsed chunk
    # Hash (including the terminal usage-only chunk) is yielded to the block;
    # the SSE `[DONE]` sentinel terminates the stream and is not yielded.
    #
    # Without a block, returns an Enumerator of chunks.
    #
    # @param params [Hash] same shape as #chat_completions
    # @yieldparam chunk [Hash] a decoded OpenAI-style streaming chunk
    # @return [nil, Enumerator]
    def chat_completions_stream(params)
      return enum_for(:chat_completions_stream, params) unless block_given?

      body = build_chat_body(params, stream: true)
      body["stream_options"] = stream_options(body["stream_options"])

      buffer = +""
      done = false
      res = @transport.stream(
        method: :post,
        url: url_for(CHAT_COMPLETIONS_PATH),
        headers: headers(accept: "text/event-stream"),
        body: JSON.generate(body)
      ) do |raw_chunk|
        next if done

        buffer << raw_chunk
        buffer.gsub!("\r\n", "\n")
        done = drain_sse(buffer) { |chunk| yield chunk }
      end

      raise_for_status(res) if res && res.status && res.status >= 400
      nil
    end

    # Known model ids.
    #
    # IMPORTANT: the Relay gateway exposes NO public list-models route (there is
    # no GET /openai/v1/models). Rather than fake an OpenAI `/v1/models` payload,
    # this returns the static list you provided via `known_models:` at
    # construction (empty by default). Use Relay.model_target /
    # Relay.function_target to build the `model` field for requests.
    #
    # @return [Array<String>]
    def models
      @known_models.dup
    end

    # Credit / balance read.
    #
    # IMPORTANT: the gateway has NO dedicated balance route. Billing is an
    # optional middleware whose only client-visible signal is a
    # `402 insufficient_credits` on inference. Rather than invent a balance
    # number, this probes that signal with a minimal chat completion and reports
    # whether credits are currently sufficient.
    #
    # @param probe_model [String, nil] model to probe with (defaults to the
    #   first configured known_model)
    # @param probe_params [Hash, nil] override the entire probe request
    # @return [Hash] { sufficient:, checked_via:, ... }
    def credits(probe_model: nil, probe_params: nil)
      params = probe_params || default_probe(probe_model)
      chat_completions(params)
      { sufficient: true, checked_via: "inference_probe" }
    rescue InsufficientCreditsError => e
      {
        sufficient: false,
        checked_via: "inference_probe",
        error: e.message,
        type: e.type,
        code: e.code
      }
    end
    alias balance credits

    private

    def url_for(path)
      "#{@base_url}#{path}"
    end

    def headers(accept:)
      h = {
        "Content-Type" => "application/json",
        "Accept" => accept
      }
      h["Authorization"] = "Bearer #{@api_key}" if @api_key && !@api_key.to_s.empty?
      h
    end

    def build_chat_body(params, stream:)
      raise ArgumentError, "params must be a Hash" unless params.is_a?(Hash)

      body = stringify_keys(params)
      raise ArgumentError, "params[:model] is required" if blank?(body["model"])
      raise ArgumentError, "params[:messages] is required" if body["messages"].nil?

      body["stream"] = stream
      body
    end

    def stream_options(existing)
      opts = existing.is_a?(Hash) ? stringify_keys(existing) : {}
      opts["include_usage"] = true unless opts.key?("include_usage")
      opts
    end

    # Consumes complete SSE events from the front of `buffer` (mutating it),
    # yielding each decoded data payload as a Hash. Returns true once the
    # `[DONE]` sentinel has been seen so the caller can stop the stream.
    def drain_sse(buffer)
      while (idx = buffer.index("\n\n"))
        frame = buffer.slice!(0, idx + 2)
        data = extract_sse_data(frame)
        next if data.nil?
        return true if data == "[DONE]"

        yield JSON.parse(data)
      end
      false
    end

    # Joins the `data:` lines of a single SSE frame; returns nil if none.
    def extract_sse_data(frame)
      data_lines = frame.split("\n").select { |line| line.start_with?("data:") }
      return nil if data_lines.empty?

      data_lines.map { |line| line.sub(/\Adata:[ \t]?/, "") }.join("\n")
    end

    def default_probe(model)
      model ||= @known_models.first
      if blank?(model)
        raise ArgumentError,
              "credits/balance needs a model to probe with; pass probe_model: " \
              "or configure known_models:"
      end
      {
        "model" => model,
        "messages" => [{ "role" => "user", "content" => "ping" }],
        "max_tokens" => 1
      }
    end

    def raise_for_status(res)
      parsed = safe_json(res.body)
      err = parsed.is_a?(Hash) ? (parsed["error"] || {}) : {}
      err = {} unless err.is_a?(Hash)

      type = err["type"]
      code = err["code"]
      message = err["message"] || "Relay API error (HTTP #{res.status})"

      klass =
        if type == "insufficient_credits"
          InsufficientCreditsError
        else
          case res.status
          when 401, 403 then AuthenticationError
          when 402 then InsufficientCreditsError
          when 404 then NotFoundError
          when 429 then RateLimitError
          when 500..599 then ServerError
          else APIError
          end
        end

      raise klass.new(message, status: res.status, type: type, code: code, body: res.body)
    end

    def parse_json(body)
      return {} if body.nil? || body.empty?

      JSON.parse(body)
    rescue JSON::ParserError => e
      raise APIError.new("failed to parse Relay response as JSON: #{e.message}", body: body)
    end

    def safe_json(body)
      return nil if body.nil? || body.empty?

      JSON.parse(body)
    rescue JSON::ParserError
      nil
    end

    def stringify_keys(hash)
      hash.each_with_object({}) { |(k, v), acc| acc[k.to_s] = v }
    end

    def blank?(value)
      value.nil? || (value.respond_to?(:empty?) && value.empty?)
    end
  end
end
