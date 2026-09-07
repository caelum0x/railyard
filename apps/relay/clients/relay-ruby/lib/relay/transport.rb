# frozen_string_literal: true

require "net/http"
require "socket"
require "timeout"
require "uri"

module Relay
  # Immutable value object describing an HTTP response.
  Response = Struct.new(:status, :headers, :body, keyword_init: true)

  # Default transport, backed by the standard library `Net::HTTP`.
  #
  # The transport interface is deliberately tiny so it can be swapped out in
  # tests (see test/support/fake_transport.rb) without any network access:
  #
  #   * #request(method:, url:, headers:, body:)        -> Relay::Response
  #   * #stream(method:, url:, headers:, body:) { |raw_chunk| ... } -> Relay::Response
  #
  # `#stream` yields raw response-body chunks as they arrive; SSE framing is
  # handled by Relay::Client, keeping the transport protocol-agnostic.
  class NetHTTPTransport
    def initialize(open_timeout: 10, read_timeout: 300)
      @open_timeout = open_timeout
      @read_timeout = read_timeout
    end

    def request(method:, url:, headers:, body:)
      uri = URI.parse(url)
      res = build_http(uri).request(build_request(method, uri, headers, body))
      Response.new(status: res.code.to_i, headers: normalize_headers(res), body: res.body)
    rescue IOError, SystemCallError, Timeout::Error, SocketError => e
      raise TransportError, "Relay transport failed: #{e.message}"
    end

    def stream(method:, url:, headers:, body:, on_response: nil)
      uri = URI.parse(url)
      status = nil
      resp_headers = {}
      error_body = nil

      build_http(uri).request(build_request(method, uri, headers, body)) do |res|
        status = res.code.to_i
        resp_headers = normalize_headers(res)
        on_response.call(Response.new(status: status, headers: resp_headers, body: nil)) if on_response

        if status >= 400
          # Buffer the whole error payload so Client can map it to a typed error.
          error_body = +""
          res.read_body { |chunk| error_body << chunk }
        else
          res.read_body { |chunk| yield chunk }
        end
      end

      Response.new(status: status, headers: resp_headers, body: error_body)
    rescue IOError, SystemCallError, Timeout::Error, SocketError => e
      raise TransportError, "Relay transport failed: #{e.message}"
    end

    private

    def build_http(uri)
      http = Net::HTTP.new(uri.host, uri.port)
      http.use_ssl = (uri.scheme == "https")
      http.open_timeout = @open_timeout
      http.read_timeout = @read_timeout
      http
    end

    def build_request(method, uri, headers, body)
      klass =
        case method.to_s.upcase
        when "GET"  then Net::HTTP::Get
        when "POST" then Net::HTTP::Post
        when "PUT" then Net::HTTP::Put
        when "PATCH" then Net::HTTP::Patch
        when "DELETE" then Net::HTTP::Delete
        else raise ArgumentError, "unsupported HTTP method: #{method}"
        end

      req = klass.new(uri.request_uri)
      headers.each { |k, v| req[k] = v }
      req.body = body if body
      req
    end

    def normalize_headers(res)
      out = {}
      res.each_header { |k, v| out[k.downcase] = v }
      out
    end
  end
end
