# frozen_string_literal: true

require_relative "test_helper"

class RelayProductTest < Minitest::Test
  REQUEST_ID = "018f7e23-7b55-7d9a-8000-000000000002"
  EVENT_ID = "018f7e23-7b55-7d9a-8000-000000000003"

  def setup
    @transport = FakeTransport.new
    @client = Relay::Client.new(base_url: "https://relay.example", transport: @transport)
  end

  def test_decodes_and_validates_aitp_challenge
    payment = Relay::Aitp.payment_required(payment_headers)

    assert_equal REQUEST_ID, payment.request_id
    assert_equal "1", payment.protocol_version
    assert_equal "ait1qrelay", payment.challenge.payee
    assert_equal 7_500, payment.challenge.amount_micros
    assert_equal "AIT", payment.challenge.currency
    assert_equal "aitp-hold", payment.challenge.scheme

    bad = payment_headers.merge("aitp-challenge" => Base64.urlsafe_encode64("{}", padding: false))
    assert_raises(Relay::ProtocolError) { Relay::Aitp.payment_required(bad) }
  end

  def test_paid_json_authorizes_once_and_retries_exactly_once
    @transport.enqueue(status: 402, headers: payment_headers, body: payment_body)
    @transport.enqueue(status: 200, body: JSON.generate("id" => "completion-1"))
    observed = nil

    response = @client.paid_chat_completions(chat_request, authorizer: lambda { |payment|
      observed = payment
      "signed-mint-authorization"
    })

    assert_equal "completion-1", response["id"]
    assert_equal 2, @transport.requests.length
    assert_equal 7_500, observed.challenge.amount_micros
    retry_headers = @transport.requests.last[:headers]
    assert_equal "signed-mint-authorization", retry_headers["aitp-authorization"]
    assert_equal REQUEST_ID, retry_headers["aitp-request-id"]
  end

  def test_paid_json_never_loops_after_a_second_402
    2.times { @transport.enqueue(status: 402, headers: payment_headers, body: payment_body) }

    assert_raises(Relay::PaymentRequiredError) do
      @client.paid_inference({ model_name: "summarize", input: "hello" }, authorizer: ->(_) { "signed" })
    end
    assert_equal 2, @transport.requests.length
  end

  def test_settled_paid_retry_returns_typed_terminal_evidence
    @transport.enqueue(status: 402, headers: payment_headers, body: payment_body)
    evidence = {
      "request_id" => REQUEST_ID,
      "principal" => "acct_owner",
      "receipt" => {
        "sequence" => 7,
        "operation" => "hold.capture",
        "operation_hash" => "op",
        "amount_micros" => 42,
        "receipt_hash" => "hash",
        "signature" => "sig"
      },
      "settled_at_ms" => 123
    }
    @transport.enqueue(
      status: 409,
      body: JSON.generate(
        "error" => {
          "message" => "request already settled",
          "type" => "request_already_settled",
          "code" => "request_already_settled"
        },
        "data" => evidence
      )
    )
    verified_client = Relay::Client.new(
      base_url: "https://relay.example", transport: @transport,
      settlement_evidence_verifier: ->(candidate) { candidate["request_id"] == REQUEST_ID }
    )

    error = assert_raises(Relay::AlreadySettledError) do
      verified_client.paid_inference(
        { model_name: "summarize", input: "hello" },
        authorizer: ->(_) { "signed" }
      )
    end
    assert_equal REQUEST_ID, error.evidence["request_id"]
    assert_equal 42, error.evidence.dig("receipt", "amount_micros")
    assert_equal 2, @transport.requests.length
  end

  def test_settled_evidence_without_verifier_fails_closed
    evidence = {
      "request_id" => REQUEST_ID,
      "principal" => "acct_owner",
      "receipt" => {
        "sequence" => 7, "operation" => "hold.capture", "operation_hash" => "op",
        "amount_micros" => 42, "receipt_hash" => "hash", "signature" => "sig"
      },
      "settled_at_ms" => 123
    }
    @transport.enqueue(
      status: 409,
      body: JSON.generate(
        "error" => { "code" => "request_already_settled" }, "data" => evidence
      )
    )

    assert_raises(Relay::UnverifiedSettlementEvidenceError) do
      @client.inference(model_name: "summarize", input: "hello")
    end
  end

  def test_direct_settlement_lookup_requires_verification
    evidence = {
      "request_id" => REQUEST_ID,
      "principal" => "acct_owner",
      "receipt" => {
        "sequence" => 7, "operation" => "hold.capture", "operation_hash" => "op",
        "amount_micros" => 42, "receipt_hash" => "hash", "signature" => "sig"
      },
      "settled_at_ms" => 123
    }
    @transport.enqueue(status: 200, body: JSON.generate("data" => evidence))
    assert_raises(Relay::UnverifiedSettlementEvidenceError) do
      @client.settlement_evidence(REQUEST_ID)
    end

    verified_transport = FakeTransport.new
    verified_transport.enqueue(status: 200, body: JSON.generate("data" => evidence))
    client = Relay::Client.new(
      base_url: "https://relay.example", transport: verified_transport,
      settlement_evidence_verifier: ->(candidate) { candidate["request_id"] == REQUEST_ID }
    )
    assert_equal evidence, client.settlement_evidence(REQUEST_ID)
  end

  def test_paid_multipart_replays_identical_binary_body_once
    @transport.enqueue(status: 402, headers: payment_headers, body: payment_body)
    @transport.enqueue(status: 200, body: JSON.generate("uploaded" => true))
    form = Relay::MultipartForm.new
      .text("purpose", "batch")
      .file("file", "requests.bin", "application/octet-stream", [0, 1, 255].pack("C*"))

    result = @client.paid_multipart("v1/files", form, authorizer: ->(_) { "signed-upload" })

    assert_equal true, result["uploaded"]
    assert_equal 2, @transport.requests.length
    assert_equal @transport.requests[0][:body], @transport.requests[1][:body]
    assert_includes @transport.requests[0][:body].bytes.each_cons(3).to_a, [0, 1, 255]
    content_type = @transport.requests[0][:headers]["Content-Type"]
    assert_match %r{\Amultipart/form-data; boundary=relay-sdk-}, content_type
    boundary = content_type.split("boundary=", 2).last
    assert @transport.requests[0][:body].end_with?("--#{boundary}--\r\n".b)
    assert_equal "signed-upload", @transport.requests[1][:headers]["aitp-authorization"]
  end

  def test_multipart_rejects_header_injection
    assert_raises(ArgumentError) do
      Relay::MultipartForm.new.file(
        "file", "bad\r\nX-Evil: yes", "application/octet-stream", "".b
      )
    end
  end

  def test_paid_stream_retries_with_authorization_and_yields_usage
    @transport.script_stream([], status: 402, headers: payment_headers, error_body: payment_body)
    @transport.script_stream([
      "data: #{JSON.generate('choices' => [{ 'delta' => { 'content' => 'paid' } }])}\n\n",
      "data: #{JSON.generate('choices' => [], 'usage' => { 'total_tokens' => 8 })}\n\n",
      "data: [DONE]\n\n"
    ], headers: { "aitp-request-id" => REQUEST_ID })

    chunks = []
    @client.paid_chat_completions_stream(chat_request, authorizer: ->(_) { "signed" }) do |chunk|
      chunks << chunk
    end

    assert_equal "paid", chunks.first.dig("choices", 0, "delta", "content")
    assert_equal 8, chunks.last.dig("usage", "total_tokens")
    assert_equal 2, @transport.requests.length
    assert_equal "signed", @transport.requests.last[:headers]["aitp-authorization"]
  end

  def test_health_accepts_degraded_503_as_typed_payload
    @transport.enqueue(
      status: 503,
      body: JSON.generate("status" => "degraded", "components" => { "billing" => "unavailable" })
    )

    health = @client.health
    assert_equal "degraded", health["status"]
    assert_equal "unavailable", health.dig("components", "billing")
  end

  def test_catalog_quote_and_cost_event_routes
    @transport.enqueue(status: 200, body: envelope("schema_version" => 1, "revision" => "r1", "models" => []))
    @transport.enqueue(status: 200, body: envelope("amount_micros" => 7_500))
    @transport.enqueue(status: 200, body: envelope("items" => [], "next_cursor" => EVENT_ID))
    @transport.enqueue(status: 200, body: envelope("id" => EVENT_ID, "state" => "dead_lettered"))
    @transport.enqueue(status: 200, body: envelope("id" => EVENT_ID, "state" => "pending"))

    assert_equal "r1", @client.catalog["revision"]
    assert_equal 7_500, @client.quote(model: "openai/gpt-4o", input_tokens: 1_000, output_tokens: 500)["amount_micros"]
    page = @client.cost_events(state: "dead_lettered", before: REQUEST_ID, limit: 25)
    assert_equal EVENT_ID, page["next_cursor"]
    assert_equal "dead_lettered", @client.cost_event(EVENT_ID)["state"]
    assert_equal "pending", @client.replay_cost_event(EVENT_ID)["state"]

    assert_equal "https://relay.example/v1/billing/cost-events?limit=25&state=dead_lettered&before=#{REQUEST_ID}",
                 @transport.requests[2][:url]
    assert_raises(ArgumentError) { @client.cost_events(limit: 101) }
  end

  def test_early_stream_drop_hands_request_to_recovery_but_done_does_not
    abandoned = []
    client = Relay::Client.new(
      base_url: "https://relay.example", transport: @transport,
      on_abandoned_stream: ->(request_id) { abandoned << request_id }
    )
    chunk = "data: #{JSON.generate('choices' => [{ 'delta' => { 'content' => 'x' } }])}\n\n"
    @transport.script_stream([chunk], headers: { "aitp-request-id" => REQUEST_ID })
    @transport.script_stream([chunk, "data: [DONE]\n\n"], headers: { "aitp-request-id" => EVENT_ID })

    observed = nil
    client.chat_completions_stream(chat_request) do |value|
      observed = value
      break
    end
    assert_equal "x", observed.dig("choices", 0, "delta", "content")
    client.chat_completions_stream(chat_request) { |_| }

    assert_equal [REQUEST_ID], abandoned
  end

  def test_response_sse_and_url_safety_bounds
    small = Relay::Client.new(
      base_url: "https://relay.example", transport: @transport,
      max_response_bytes: 8, max_sse_event_bytes: 12
    )
    @transport.enqueue(status: 200, body: JSON.generate("value" => "too large"))
    assert_raises(Relay::ResponseTooLargeError) { small.status }

    @transport.script_stream(["data: {\"long\":true}\n\n"])
    assert_raises(Relay::ProtocolError) do
      small.chat_completions_stream(chat_request) { |_| }
    end

    assert_raises(ArgumentError) { @client.custom_json(method: :get, path: "../admin") }
    assert_raises(Relay::ConfigurationError) do
      Relay::Client.new(base_url: "https://user:secret@relay.example")
    end
  end

  private

  def chat_request
    { model: "tensorzero::model_name::gpt-4o", messages: [{ role: "user", content: "hello" }] }
  end

  def payment_body
    JSON.generate("error" => { "type" => "payment_required", "message" => "payment required" })
  end

  def envelope(data)
    JSON.generate("data" => data)
  end

  def payment_headers
    challenge = {
      payee: "ait1qrelay", amount_micros: 7_500, currency: "AIT",
      resource: "POST /openai/v1/chat/completions#gpt-4o@#{REQUEST_ID}",
      nonce: "nonce", expiry: 1_800_000_000_000, scheme: "aitp-hold"
    }
    {
      "aitp-challenge" => Base64.urlsafe_encode64(JSON.generate(challenge), padding: false),
      "aitp-request-id" => REQUEST_ID,
      "aitp-version" => "1"
    }
  end
end
