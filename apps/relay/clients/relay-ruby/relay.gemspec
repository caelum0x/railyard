# frozen_string_literal: true

lib = File.expand_path("lib", __dir__)
$LOAD_PATH.unshift(lib) unless $LOAD_PATH.include?(lib)
require "relay/version"

Gem::Specification.new do |s|
  s.name        = "relay"
  s.version     = Relay::VERSION
  s.summary     = "Native Ruby client for the Relay gateway (OpenAI-compatible)"
  s.description = "Relay is a native Ruby SDK for the Relay inference gateway. It wraps the " \
                  "OpenAI-compatible HTTP surface (chat completions with JSON and SSE " \
                  "streaming) plus model addressing and a credits/billing probe, with no " \
                  "dependency on the openai gem."
  s.authors     = ["Relay"]
  s.email       = ["hello@relay.dev"]
  s.homepage    = "https://github.com/relay/relay"
  s.license     = "Apache-2.0"

  s.required_ruby_version = ">= 2.6"

  s.files = Dir[
    "lib/**/*.rb",
    "README.md",
    "relay.gemspec"
  ]
  s.require_paths = ["lib"]

  s.metadata = {
    "source_code_uri" => "https://github.com/relay/relay",
    "homepage_uri" => "https://github.com/relay/relay"
  }

  s.add_development_dependency "minitest", ">= 5.0"
  s.add_development_dependency "rake", ">= 12.0"
end
