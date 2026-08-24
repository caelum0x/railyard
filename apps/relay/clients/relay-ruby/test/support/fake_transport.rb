# frozen_string_literal: true

# In-memory transport used by the test suite. It records every request and
# replays scripted responses, so the SDK is exercised end-to-end (request
# building, header/auth injection, SSE parsing, error mapping) without any
# network access or webmock dependency.
class FakeTransport
  attr_reader :requests

  def initialize
    @requests = []
    @responses = []
    @stream_chunks = []
    @stream_status = 200
    @stream_headers = {}
    @stream_error_body = nil
  end

  # Queue a canned response for the next #request call (FIFO).
  def enqueue(status:, body:, headers: {})
    @responses << Relay::Response.new(status: status, headers: headers, body: body)
    self
  end

  # Script the raw SSE chunks the next #stream call will emit.
  def script_stream(chunks, status: 200, headers: {}, error_body: nil)
    @stream_chunks = chunks
    @stream_status = status
    @stream_headers = headers
    @stream_error_body = error_body
    self
  end

  def last_request
    @requests.last
  end

  def request(method:, url:, headers:, body:)
    @requests << { method: method, url: url, headers: headers, body: body }
    @responses.shift || Relay::Response.new(status: 200, headers: {}, body: "{}")
  end

  def stream(method:, url:, headers:, body:)
    @requests << { method: method, url: url, headers: headers, body: body }

    if @stream_status >= 400
      return Relay::Response.new(
        status: @stream_status,
        headers: @stream_headers,
        body: @stream_error_body
      )
    end

    @stream_chunks.each { |chunk| yield chunk }
    Relay::Response.new(status: @stream_status, headers: @stream_headers, body: nil)
  end
end
