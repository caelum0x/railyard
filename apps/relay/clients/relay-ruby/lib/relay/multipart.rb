# frozen_string_literal: true

require "securerandom"

module Relay
  MultipartField = Struct.new(:name, :value, keyword_init: true)
  MultipartPart = Struct.new(:name, :filename, :content_type, :body, keyword_init: true)

  # Dependency-free, binary-safe multipart/form-data builder.
  class MultipartForm
    def initialize
      @fields = []
      @files = []
    end

    def text(name, value)
      @fields << MultipartField.new(name: token(name, "multipart field name"), value: value.to_s)
      self
    end

    def file(name, filename, content_type, body)
      raise TypeError, "multipart file body must be a String" unless body.is_a?(String)

      @files << MultipartPart.new(
        name: token(name, "multipart part name"),
        filename: token(filename, "multipart filename"),
        content_type: valid_content_type(content_type),
        body: body.b.dup
      )
      self
    end

    def encode
      boundary = "relay-sdk-#{SecureRandom.hex(16)}"
      body = +"".b
      @fields.each do |field|
        body << "--#{boundary}\r\n".b
        body << "Content-Disposition: form-data; name=\"#{field.name}\"\r\n\r\n".b
        body << field.value.encode(Encoding::UTF_8).b
        body << "\r\n".b
      end
      @files.each do |part|
        body << "--#{boundary}\r\n".b
        body << "Content-Disposition: form-data; name=\"#{part.name}\"; filename=\"#{part.filename}\"\r\n".b
        body << "Content-Type: #{part.content_type}\r\n\r\n".b
        body << part.body
        body << "\r\n".b
      end
      body << "--#{boundary}--\r\n".b
      ["multipart/form-data; boundary=#{boundary}", body]
    end

    private

    def token(value, label)
      value = value.to_s
      if value.empty? || value.include?("\r") || value.include?("\n") || value.include?('"')
        raise ArgumentError, "#{label} is empty or contains unsafe header characters"
      end
      value
    end

    def valid_content_type(value)
      value = value.to_s
      if value.empty? || !value.include?("/") || value.include?("\r") || value.include?("\n")
        raise ArgumentError, "multipart content type is invalid"
      end
      value
    end
  end
end
