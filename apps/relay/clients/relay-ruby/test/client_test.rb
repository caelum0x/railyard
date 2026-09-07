# frozen_string_literal: true

require_relative "test_helper"

class RelayClientTest < Minitest::Test
  def setup
    @transport = FakeTransport.new
    @client = Relay::Client.new(
      base_url: "http://localhost:3000",
      api_key: "sk-test-123",
      transport: @transport,
      known_models: ["gpt-4o-mini", "claude"]
    )
  end

  # ---- construction ---------------------------------------------------------

  def test_requires_base_url
    assert_raises(Relay::ConfigurationError) { Relay::Client.new(base_url: nil, api_key: "k") }
    assert_raises(Relay::ConfigurationError) { Relay::Client.new(base_url: "", api_key: "k") }
  end

  def test_strips_trailing_slash_from_base_url
    c = Relay::Client.new(base_url: "http://localhost:3000///", transport: @transport)
    assert_equal "http://localhost:3000", c.base_url
  end

  # ---- chat_completions (non-streaming) ------------------------------------

  def test_chat_completions_request_shape_and_auth
    @transport.enqueue(
      status: 200,
      body: JSON.generate(
        "id" => "abc",
        "choices" => [{ "index" => 0, "finish_reason" => "stop",
                        "message" => { "role" => "assistant", "content" => "hello" } }],
        "usage" => { "prompt_tokens" => 3, "completion_tokens" => 1, "total_tokens" => 4 }
      )
    )

    result = @client.chat_completions(
      model: Relay.model_target("gpt-4o-mini"),
      messages: [{ role: "user", content: "hi" }],
      temperature: 0.2
    )

    req = @transport.last_request
    assert_equal :post, req[:method]
    assert_equal "http://localhost:3000/openai/v1/chat/completions", req[:url]
    assert_equal "Bearer sk-test-123", req[:headers]["Authorization"]
    assert_equal "application/json", req[:headers]["Content-Type"]

    sent = JSON.parse(req[:body])
    assert_equal "tensorzero::model_name::gpt-4o-mini", sent["model"]
    assert_equal [{ "role" => "user", "content" => "hi" }], sent["messages"]
    assert_equal 0.2, sent["temperature"]
    assert_equal false, sent["stream"]

    assert_equal "hello", result["choices"][0]["message"]["content"]
    assert_equal 4, result["usage"]["total_tokens"]
  end

  def test_chat_completions_omits_auth_header_without_key
    client = Relay::Client.new(base_url: "http://localhost:3000", transport: @transport)
    @transport.enqueue(status: 200, body: "{}")
    client.chat_completions(model: "m", messages: [{ role: "user", content: "x" }])
    refute @transport.last_request[:headers].key?("Authorization")
  end

  def test_chat_completions_validates_required_params
    assert_raises(ArgumentError) { @client.chat_completions(messages: []) }
    assert_raises(ArgumentError) { @client.chat_completions(model: "m") }
    assert_raises(ArgumentError) { @client.chat_completions("not a hash") }
  end

  def test_passthrough_tensorzero_keys
    @transport.enqueue(status: 200, body: "{}")
    @client.chat_completions(
      "model" => "m",
      "messages" => [{ "role" => "user", "content" => "x" }],
      "tensorzero::episode_id" => "ep-1"
    )
    sent = JSON.parse(@transport.last_request[:body])
    assert_equal "ep-1", sent["tensorzero::episode_id"]
  end

  # ---- streaming ------------------------------------------------------------

  def test_streaming_yields_deltas_and_stops_at_done
    chunks = [
      "data: #{JSON.generate('choices' => [{ 'delta' => { 'content' => 'Hel' } }])}\n\n",
      "data: #{JSON.generate('choices' => [{ 'delta' => { 'content' => 'lo' } }])}\n\n",
      "data: #{JSON.generate('choices' => [], 'usage' => { 'total_tokens' => 5 })}\n\n",
      "data: [DONE]\n\n",
      "data: #{JSON.generate('choices' => [{ 'delta' => { 'content' => 'AFTER' } }])}\n\n"
    ]
    @transport.script_stream(chunks)

    received = []
    @client.chat_completions_stream(
      model: "m",
      messages: [{ role: "user", content: "hi" }]
    ) { |c| received << c }

    # Terminal [DONE] stops iteration; nothing after it is yielded.
    assert_equal 3, received.length
    assert_equal "Hel", received[0]["choices"][0]["delta"]["content"]
    assert_equal "lo", received[1]["choices"][0]["delta"]["content"]
    assert_equal 5, received[2]["usage"]["total_tokens"]
  end

  def test_streaming_sets_stream_and_include_usage
    @transport.script_stream(["data: [DONE]\n\n"])
    @client.chat_completions_stream(model: "m", messages: [{ role: "user", content: "hi" }]) { |_| }

    req = @transport.last_request
    assert_equal "text/event-stream", req[:headers]["Accept"]
    sent = JSON.parse(req[:body])
    assert_equal true, sent["stream"]
    assert_equal true, sent["stream_options"]["include_usage"]
  end

  def test_streaming_forces_terminal_usage_for_settlement
    @transport.script_stream(["data: [DONE]\n\n"])
    @client.chat_completions_stream(
      model: "m",
      messages: [{ role: "user", content: "hi" }],
      stream_options: { include_usage: false }
    ) { |_| }
    sent = JSON.parse(@transport.last_request[:body])
    assert_equal true, sent["stream_options"]["include_usage"]
  end

  def test_streaming_handles_frames_split_across_chunks
    frame = "data: #{JSON.generate('choices' => [{ 'delta' => { 'content' => 'X' } }])}\n\n"
    mid = frame.length / 2
    @transport.script_stream([frame[0...mid], frame[mid..-1]])

    received = []
    @client.chat_completions_stream(model: "m", messages: [{ role: "user", content: "hi" }]) { |c| received << c }
    assert_equal 1, received.length
    assert_equal "X", received[0]["choices"][0]["delta"]["content"]
  end

  def test_streaming_without_block_returns_enumerator
    @transport.script_stream([
      "data: #{JSON.generate('choices' => [{ 'delta' => { 'content' => 'A' } }])}\n\n",
      "data: [DONE]\n\n"
    ])
    enum = @client.chat_completions_stream(model: "m", messages: [{ role: "user", content: "hi" }])
    assert_kind_of Enumerator, enum
    assert_equal "A", enum.first["choices"][0]["delta"]["content"]
  end

  def test_streaming_maps_error_status
    @transport.script_stream([], status: 402, headers: payment_headers,
      error_body: JSON.generate("error" => { "type" => "payment_required", "message" => "payment required" }))
    assert_raises(Relay::PaymentRequiredError) do
      @client.chat_completions_stream(model: "m", messages: [{ role: "user", content: "hi" }]) { |_| }
    end
  end

  # ---- error mapping --------------------------------------------------------

  def test_maps_402_to_typed_payment_required
    @transport.enqueue(status: 402,
      headers: payment_headers,
      body: JSON.generate("error" => { "message" => "payment required" }))
    err = assert_raises(Relay::PaymentRequiredError) do
      @client.chat_completions(model: "m", messages: [{ role: "user", content: "x" }])
    end
    assert_equal 402, err.status
    assert_equal REQUEST_ID, err.payment.request_id
    assert_equal 7_500, err.payment.challenge.amount_micros
  end

  def test_maps_401_to_authentication_error
    @transport.enqueue(status: 401, body: JSON.generate("error" => { "message" => "bad key" }))
    assert_raises(Relay::AuthenticationError) do
      @client.chat_completions(model: "m", messages: [{ role: "user", content: "x" }])
    end
  end

  def test_maps_404_to_not_found
    @transport.enqueue(status: 404, body: "{}")
    assert_raises(Relay::NotFoundError) do
      @client.chat_completions(model: "m", messages: [{ role: "user", content: "x" }])
    end
  end

  def test_maps_500_to_server_error
    @transport.enqueue(status: 500, body: "{}")
    assert_raises(Relay::ServerError) do
      @client.chat_completions(model: "m", messages: [{ role: "user", content: "x" }])
    end
  end

  def test_generic_4xx_uses_body_message
    @transport.enqueue(status: 400, body: JSON.generate("error" => { "message" => "bad request", "type" => "invalid" }))
    err = assert_raises(Relay::APIError) do
      @client.chat_completions(model: "m", messages: [{ role: "user", content: "x" }])
    end
    assert_equal "bad request", err.message
    assert_equal 400, err.status
  end

  # ---- models ---------------------------------------------------------------

  def test_models_returns_configured_known_models
    assert_equal ["gpt-4o-mini", "claude"], @client.models
  end

  def test_models_defaults_to_empty
    c = Relay::Client.new(base_url: "http://localhost:3000", transport: @transport)
    assert_equal [], c.models
  end

  def test_addressing_helpers
    assert_equal "tensorzero::model_name::gpt-4o", Relay.model_target("gpt-4o")
    assert_equal "tensorzero::function_name::summarize", Relay.function_target("summarize")
  end

  # ---- credits / balance ----------------------------------------------------

  def test_credits_reports_sufficient_on_success
    @transport.enqueue(status: 200, body: JSON.generate("choices" => []))
    result = @client.credits
    assert_equal true, result[:sufficient]
    assert_equal "dry_run_inference_probe", result[:checked_via]

    # It probes via a minimal chat completion.
    sent = JSON.parse(@transport.last_request[:body])
    assert_equal "gpt-4o-mini", sent["model"]
    assert_equal 1, sent["max_tokens"]
    assert_equal true, sent["tensorzero::dryrun"]
  end

  def test_credits_reports_insufficient_on_402
    @transport.enqueue(status: 402, headers: payment_headers,
      body: JSON.generate("error" => { "message" => "payment required" }))
    result = @client.balance
    assert_equal false, result[:sufficient]
    assert_equal "payment_required", result[:type]
  end

  def test_credits_requires_a_probe_model
    c = Relay::Client.new(base_url: "http://localhost:3000", transport: @transport)
    assert_raises(ArgumentError) { c.credits }
  end

  REQUEST_ID = "018f7e23-7b55-7d9a-8000-000000000002"

  def payment_headers
    challenge = {
      payee: "ait1qrelay", amount_micros: 7_500, currency: "AIT",
      resource: "POST /openai/v1/chat/completions#m@#{REQUEST_ID}",
      nonce: "nonce", expiry: 1_800_000_000_000, scheme: "aitp-hold"
    }
    {
      "aitp-challenge" => Base64.urlsafe_encode64(JSON.generate(challenge), padding: false),
      "aitp-request-id" => REQUEST_ID,
      "aitp-version" => "1"
    }
  end
end
